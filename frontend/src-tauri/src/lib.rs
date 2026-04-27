use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

#[cfg(target_os = "windows")]
static FULLSCREEN_HIDING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
use percent_encoding::percent_decode_str;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether the efficiency-mode notch hover tracking thread should be running.
static EFFICIENCY_HOVER_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Whether the hover poll thread is actually alive (set true on entry, false on exit).
static EFFICIENCY_HOVER_THREAD_ALIVE: AtomicBool = AtomicBool::new(false);
/// Whether the mini panel is currently expanded (used by the hover poll to
/// decide which detection region to check — collapsed notch area vs expanded
/// panel area).
static EFFICIENCY_EXPANDED: AtomicBool = AtomicBool::new(false);
/// Cached screen geometry for the notch hover poll thread so it doesn't need
/// to access NSWindow from a background thread.
/// (screen_x, screen_y, screen_width, screen_height, notch_offset)
static NOTCH_SCREEN_INFO: Mutex<Option<(f64, f64, f64, f64, f64)>> = Mutex::new(None);
/// Cached mini window frame (x, y, w, h) in macOS screen coordinates
/// (bottom-left origin).  Updated by `set_mini_expanded` and
/// `resize_mini_height` so the hover poll can use the real frame size
/// instead of hard-coded constants.
static MINI_WINDOW_FRAME: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
/// Temporary frame snapshot used by pet-context menu expansion. We store the
/// original collapsed frame before expanding, then restore exactly to avoid
/// mascot "teleport" after right-click close.
static PET_MENU_RESTORE_FRAME: Mutex<Option<(f64, f64, f64, f64)>> = Mutex::new(None);
/// Generation counter for pet-context alpha restore (legacy resize path).
#[cfg(target_os = "macos")]
static PET_ALPHA_GEN: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Whether the pet-mode click-through poll thread should be running.
static PET_PASSTHROUGH_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Whether the pet-mode click-through poll thread is alive.
static PET_PASSTHROUGH_THREAD_ALIVE: AtomicBool = AtomicBool::new(false);
/// Whether the pet-mode context menu is currently open. When true the poll
/// thread disables ignoresMouseEvents so the entire expanded window accepts
/// clicks (for the menu buttons). When false, only the mascot area accepts
/// clicks and the rest is pass-through.
static PET_CONTEXT_MENU_OPEN: AtomicBool = AtomicBool::new(false);

/// Per-host SSH backoff state.
struct SshBackoffState {
    fail_count: u32,
    fail_epoch: u64,
}

static SSH_BACKOFF: std::sync::OnceLock<Mutex<HashMap<String, SshBackoffState>>> = std::sync::OnceLock::new();

fn ssh_backoff_map() -> &'static Mutex<HashMap<String, SshBackoffState>> {
    SSH_BACKOFF.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Stores which SSH key was accepted for each host (user@host → key path).
/// Populated by ensure_ssh_master via `ssh -v` output parsing.
static SSH_KEY_USED: std::sync::OnceLock<Mutex<HashMap<String, String>>> = std::sync::OnceLock::new();

fn ssh_key_map() -> &'static Mutex<HashMap<String, String>> {
    SSH_KEY_USED.get_or_init(|| Mutex::new(HashMap::new()))
}

fn unix_now() -> u64 {
    SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_secs()
}

fn ssh_backoff_remaining(host_key: &str) -> Option<u64> {
    let map = ssh_backoff_map().lock().unwrap();
    let state = map.get(host_key)?;
    if state.fail_count == 0 { return None; }
    let cooldown = std::cmp::min(15u64 * 2u64.pow(state.fail_count.saturating_sub(1)), 300);
    let elapsed = unix_now().saturating_sub(state.fail_epoch);
    if elapsed < cooldown { Some(cooldown - elapsed) } else { None }
}

fn ssh_backoff_record_failure(host_key: &str) {
    let mut map = ssh_backoff_map().lock().unwrap();
    let state = map.entry(host_key.to_string()).or_insert(SshBackoffState { fail_count: 0, fail_epoch: 0 });
    state.fail_count += 1;
    state.fail_epoch = unix_now();
}

fn ssh_backoff_reset(host_key: &str) {
    let mut map = ssh_backoff_map().lock().unwrap();
    map.remove(host_key);
}

use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Emitter, Manager, WebviewUrl, WebviewWindowBuilder,
};
#[cfg(unix)]
use libc;

/// Apply CREATE_NO_WINDOW on Windows to prevent console popups from child processes.
#[cfg(windows)]
fn hide_window_cmd(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Apply CREATE_NO_WINDOW on Windows to prevent console popups (tokio version).
#[cfg(windows)]
fn hide_window_tokio_cmd(cmd: &mut tokio::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

// ---------------------------------------------------------------------------
// Windows SSH multiplexer — a persistent `ssh -T` subprocess per host
// that serialises commands over stdin/stdout, avoiding the per-exec overhead
// of a full TCP+SSH handshake (Windows lacks ControlMaster).
// ---------------------------------------------------------------------------
#[cfg(windows)]
mod win_ssh_mux {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::process::{Child, Command};
    use tokio::sync::Mutex as TokioMutex;

    struct MuxChild {
        stdin: tokio::process::ChildStdin,
        stdout: BufReader<tokio::process::ChildStdout>,
        child: Child,
    }

    // One multiplexed SSH session per user@host.
    // The TokioMutex serialises commands so marker boundaries never interleave.
    static MUX_SESSIONS: OnceLock<Mutex<HashMap<String, Arc<TokioMutex<Option<MuxChild>>>>>> =
        OnceLock::new();

    fn mux_map() -> &'static Mutex<HashMap<String, Arc<TokioMutex<Option<MuxChild>>>>> {
        MUX_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn session_lock(host_key: &str) -> Arc<TokioMutex<Option<MuxChild>>> {
        let mut map = mux_map().lock().unwrap();
        map.entry(host_key.to_string())
            .or_insert_with(|| Arc::new(TokioMutex::new(None)))
            .clone()
    }

    /// Spawn the persistent SSH process if it isn't already running.
    pub async fn ensure(ssh_user: &str, ssh_host: &str) -> Result<(), String> {
        let host_key = format!("{}@{}", ssh_user, ssh_host);
        let lock = session_lock(&host_key);
        let mut guard = lock.lock().await;

        // Already running and alive?
        if let Some(ref mut m) = *guard {
            if m.child.try_wait().ok().flatten().is_none() {
                return Ok(());
            }
            // Process exited — fall through and respawn.
        }

        let mut cmd = Command::new("ssh");
        cmd.args([
                "-T",
                "-o", "StrictHostKeyChecking=no",
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=10",
                "-o", "ServerAliveInterval=15",
                "-o", "ServerAliveCountMax=3",
                &host_key,
            ])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        super::hide_window_tokio_cmd(&mut cmd);
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("ssh mux spawn: {}", e))?;

        let stdin = child.stdin.take().ok_or("ssh mux: no stdin")?;
        let stdout = child.stdout.take().ok_or("ssh mux: no stdout")?;
        let reader = BufReader::new(stdout);

        *guard = Some(MuxChild { stdin, stdout: reader, child });

        // Validate the connection with a quick echo test.
        drop(guard);
        match exec_inner(ssh_user, ssh_host, "echo __oc_mux_ready__").await {
            Ok(out) if out.contains("__oc_mux_ready__") => Ok(()),
            Ok(out) => {
                kill(ssh_user, ssh_host).await;
                Err(format!("ssh mux validation unexpected output: {}", out))
            }
            Err(e) => {
                kill(ssh_user, ssh_host).await;
                Err(format!("ssh mux validation failed: {}", e))
            }
        }
    }

    /// Send `cmd` through the persistent session and collect its stdout + exit code.
    pub async fn exec(ssh_user: &str, ssh_host: &str, cmd: &str) -> Result<String, String> {
        exec_inner(ssh_user, ssh_host, cmd).await
    }

    async fn exec_inner(ssh_user: &str, ssh_host: &str, cmd: &str) -> Result<String, String> {
        let host_key = format!("{}@{}", ssh_user, ssh_host);
        let lock = session_lock(&host_key);
        let mut guard = lock.lock().await;
        let mux = guard.as_mut().ok_or_else(|| "ssh mux: not connected".to_string())?;

        // Check the process is still alive.
        if mux.child.try_wait().ok().flatten().is_some() {
            *guard = None;
            return Err("ssh mux: process exited".to_string());
        }

        // Unique marker that cannot appear in normal command output.
        let marker = format!("__OCCLAW_{}__", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());

        // Wrap the command so we can capture its exit code after a unique delimiter.
        // The shell on the remote side will:
        //   1. Run cmd, capturing exit code in __ec
        //   2. Print a blank line + the marker + exit_code on one line
        let wrapped = format!(
            "{cmd}\n__ec=$?\necho \"\"\necho \"{marker} $__ec\"\n",
            cmd = cmd,
            marker = marker,
        );

        mux.stdin
            .write_all(wrapped.as_bytes())
            .await
            .map_err(|e| format!("ssh mux write: {}", e))?;
        mux.stdin.flush().await.map_err(|e| format!("ssh mux flush: {}", e))?;

        // Read lines until we see the marker.
        let mut output = String::new();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let exit_code: i32;

        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                // Subprocess may be hung — kill and return error.
                *guard = None;
                return Err("ssh mux: command timed out after 30s".to_string());
            }

            let mut line = String::new();
            let read_result = tokio::time::timeout(remaining, mux.stdout.read_line(&mut line)).await;

            match read_result {
                Ok(Ok(0)) => {
                    // EOF — the SSH process died.
                    *guard = None;
                    return Err("ssh mux: connection lost (EOF)".to_string());
                }
                Ok(Ok(_)) => {
                    if let Some(rest) = line.trim().strip_prefix(&marker) {
                        exit_code = rest.trim().parse().unwrap_or(-1);
                        break;
                    }
                    output.push_str(&line);
                }
                Ok(Err(e)) => {
                    *guard = None;
                    return Err(format!("ssh mux read: {}", e));
                }
                Err(_) => {
                    *guard = None;
                    return Err("ssh mux: command timed out after 30s".to_string());
                }
            }
        }

        if exit_code == 0 {
            Ok(output)
        } else if exit_code == 255 {
            // Transport-level failure — mark session dead so it respawns.
            *guard = None;
            Err(format!("ssh mux transport error (exit 255)"))
        } else {
            Err(format!("ssh cmd failed [exit {}]\nstdout: {}", exit_code, output.trim()))
        }
    }

    /// Kill the persistent subprocess for a given host.
    pub async fn kill(ssh_user: &str, ssh_host: &str) {
        let host_key = format!("{}@{}", ssh_user, ssh_host);
        let lock = session_lock(&host_key);
        let mut guard = lock.lock().await;
        if let Some(ref mut m) = *guard {
            let _ = m.child.kill().await;
        }
        *guard = None;
    }

    /// Kill all persistent subprocesses.
    pub async fn kill_all() {
        let keys: Vec<String> = {
            mux_map().lock().unwrap().keys().cloned().collect()
        };
        for key in keys {
            let lock = session_lock(&key);
            let mut guard = lock.lock().await;
            if let Some(ref mut m) = *guard {
                let _ = m.child.kill().await;
            }
            *guard = None;
        }
    }
}

/// Fix PATH for macOS GUI apps which only get /usr/bin:/bin:/usr/sbin:/sbin.
/// openclaw is a Node.js script installed via pnpm, so both `openclaw` and `node`
/// must be reachable via PATH.
/// On Windows, GUI apps inherit the full user PATH, so no fix is needed.
fn fix_path() {
    #[cfg(target_os = "macos")]
    {
        for shell in ["/bin/zsh", "/bin/bash"] {
            if let Ok(output) = std::process::Command::new(shell)
                .args(["-lic", "echo $PATH"])
                .output()
            {
                if output.status.success() {
                    let shell_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if !shell_path.is_empty() {
                        std::env::set_var("PATH", &shell_path);
                        log::info!("[fix_path] PATH set to: {}", &shell_path);
                        return;
                    }
                }
            }
        }
        log::warn!("[fix_path] could not get PATH from login shell");
    }
    #[cfg(target_os = "windows")]
    {
        // Windows GUI apps inherit the full user/system PATH from the registry.
        // No fix needed — openclaw and node should be reachable if installed.
        log::info!("[fix_path] Windows: using inherited PATH");
    }
}

/// Managed state: tracks the PID of the currently running `openclaw agent` subprocess.
/// Used by interrupt_agent to SIGINT the active turn.
struct ActiveAgentPid {
    pid: Mutex<Option<u32>>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub label: Option<String>,
    pub status: String,
    pub model: Option<String>,
    pub channel: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GatewayStatus {
    pub active: bool,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentInfo {
    pub id: String,
    #[serde(rename = "identityName")]
    pub identity_name: Option<String>,
    #[serde(rename = "identityEmoji")]
    pub identity_emoji: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionHealth {
    pub key: String,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentHealth {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub active: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<SessionHealth>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResult {
    pub agents: Vec<AgentHealth>,
    /// Whether the local OpenClaw gateway process is running.
    /// Always `true` for remote connections (we can't check remote process).
    /// Frontend uses this to auto-remove the local connection when gateway is dead.
    #[serde(default = "default_true", rename = "gatewayAlive")]
    pub gateway_alive: bool,
}

fn default_true() -> bool { true }

/// Check whether the local OpenClaw gateway process is alive.
///
/// OpenClaw uses a lock file at `$TMPDIR/openclaw-<uid>/gateway.<hash>.lock`
/// containing `{"pid": <n>, ...}`. When the gateway shuts down it deletes the
/// lock file. If the file is missing or the PID inside is no longer running,
/// the gateway is considered dead — meaning any "active" session state left in
/// the JSONL files is stale and should be forced to inactive.
fn is_openclaw_gateway_alive() -> bool {
    // Build the lock directory: $TMPDIR/openclaw-<uid>
    let tmp = std::env::temp_dir();
    #[cfg(unix)]
    let lock_dir = {
        let uid = unsafe { libc::getuid() };
        tmp.join(format!("openclaw-{}", uid))
    };
    #[cfg(windows)]
    let lock_dir = tmp.join("openclaw");

    // Look for any gateway.*.lock file in the lock directory
    let rd = match std::fs::read_dir(&lock_dir) {
        Ok(rd) => rd,
        Err(_) => return false, // no lock dir → gateway not running
    };
    for entry in rd.filter_map(|e| e.ok()) {
        let fname = entry.file_name();
        let name = fname.to_string_lossy();
        if !name.starts_with("gateway.") || !name.ends_with(".lock") {
            continue;
        }
        // Read the lock file to extract the PID
        if let Ok(contents) = std::fs::read_to_string(entry.path()) {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(&contents) {
                if let Some(pid) = val["pid"].as_u64() {
                    // Check if the process is still alive (kill -0)
                    #[cfg(unix)]
                    {
                        let alive = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
                        if alive { return true; }
                    }
                    #[cfg(windows)]
                    {
                        // OpenProcess with PROCESS_QUERY_LIMITED_INFORMATION
                        // returns Ok(handle) if the process exists.
                        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
                        if let Ok(handle) = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid as u32) } {
                            let _ = unsafe { windows::Win32::Foundation::CloseHandle(handle) };
                            return true;
                        }
                    }
                }
            }
        }
    }
    false
}

/// Get the user home directory string in a cross-platform way.
fn home_dir_string() -> String {
    dirs::home_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| {
            #[cfg(unix)]
            { std::env::var("HOME").unwrap_or_else(|_| "/tmp".into()) }
            #[cfg(windows)]
            { std::env::var("USERPROFILE").unwrap_or_else(|_| "C:\\".into()) }
        })
}

/// Returns the full set of open .jsonl file paths across all agents.
/// On macOS/Linux: uses `lsof +D` to detect open files.
/// On Windows: falls back to checking file modification time (recent = active).
async fn lsof_open_jsonl_paths() -> std::collections::HashSet<String> {
    #[cfg(unix)]
    {
        let home = home_dir_string();
        let agents_dir = format!("{}/.openclaw/agents", home);
        let lsof_bin = if std::path::Path::new("/usr/sbin/lsof").exists() { "/usr/sbin/lsof" } else { "lsof" };
        let Ok(output) = tokio::process::Command::new(lsof_bin)
            .args(["+D", &agents_dir])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .await
        else { return std::collections::HashSet::new() };
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines()
            .filter(|l| l.contains(".jsonl"))
            .filter_map(|l| l.split_whitespace().last().map(|s| s.to_string()))
            .collect()
    }
    #[cfg(windows)]
    {
        // Windows fallback: find .jsonl files modified in the last 5 seconds
        // (indicates an active agent writing to them)
        let home = home_dir_string();
        let agents_dir = PathBuf::from(&home).join(".openclaw").join("agents");
        let mut result = std::collections::HashSet::new();
        let now = SystemTime::now();
        if let Ok(agents) = std::fs::read_dir(&agents_dir) {
            for agent_entry in agents.flatten() {
                let sessions_dir = agent_entry.path().join("sessions");
                if let Ok(files) = std::fs::read_dir(&sessions_dir) {
                    for file_entry in files.flatten() {
                        let path = file_entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            if let Ok(meta) = path.metadata() {
                                if let Ok(modified) = meta.modified() {
                                    if now.duration_since(modified).unwrap_or_default().as_secs() < 5 {
                                        result.insert(path.to_string_lossy().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        result
    }
}

/// Read the last `n` lines of a file using pure Rust (Windows replacement for `tail -n`
/// which is not available on Windows).
#[cfg(windows)]
fn tail_lines_from_file(path: &std::path::Path, n: usize) -> Vec<String> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return vec![] };
    let Ok(meta) = file.metadata() else { return vec![] };
    let len = meta.len();
    // Read up to 8KB from the end — more than enough for a handful of JSONL lines
    let read_size = std::cmp::min(len, 8192) as usize;
    let _ = file.seek(SeekFrom::End(-(read_size as i64)));
    let mut buf = vec![0u8; read_size];
    let Ok(bytes_read) = file.read(&mut buf) else { return vec![] };
    let text = String::from_utf8_lossy(&buf[..bytes_read]);
    let all_lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
    if all_lines.len() <= n {
        all_lines
    } else {
        all_lines[all_lines.len() - n..].to_vec()
    }
}

/// Single `lsof +D` over the entire agents dir → set of active agent directory names.
/// A .jsonl being held open by a process = that agent is working.
/// On Windows: uses file modification time heuristic instead of lsof.
async fn lsof_active_agents() -> std::collections::HashSet<String> {
    #[cfg(unix)]
    {
        let home = home_dir_string();
        let agents_dir = format!("{}/.openclaw/agents", home);
        let mut active = std::collections::HashSet::new();

        let lsof_bin = if std::path::Path::new("/usr/sbin/lsof").exists() {
            "/usr/sbin/lsof"
        } else {
            "lsof"
        };

        let Ok(output) = tokio::process::Command::new(lsof_bin)
            .args(["+D", &agents_dir])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
            .await
        else {
            return active;
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        let prefix = ".openclaw/agents/";
        for line in stdout.lines() {
            if !line.contains(".jsonl") {
                continue;
            }
            if let Some(idx) = line.find(prefix) {
                let rest = &line[idx + prefix.len()..];
                if let Some(slash) = rest.find('/') {
                    active.insert(rest[..slash].to_string());
                }
            }
        }
        active
    }
    #[cfg(windows)]
    {
        // Windows: find agent directories that have recently modified .jsonl files
        let home = home_dir_string();
        let agents_dir = PathBuf::from(&home).join(".openclaw").join("agents");
        let mut active = std::collections::HashSet::new();
        let now = SystemTime::now();
        if let Ok(agents) = std::fs::read_dir(&agents_dir) {
            for agent_entry in agents.flatten() {
                let agent_name = agent_entry.file_name().to_string_lossy().to_string();
                let sessions_dir = agent_entry.path().join("sessions");
                if let Ok(files) = std::fs::read_dir(&sessions_dir) {
                    for file_entry in files.flatten() {
                        let path = file_entry.path();
                        if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                            if let Ok(meta) = path.metadata() {
                                if let Ok(modified) = meta.modified() {
                                    if now.duration_since(modified).unwrap_or_default().as_secs() < 5 {
                                        active.insert(agent_name.clone());
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        active
    }
}

/// Generic helper: call OpenClaw remote API via /tools/invoke
async fn invoke_tool(url: &str, token: &str, tool: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/tools/invoke", url))
        .header("Authorization", format!("Bearer {}", token))
        .json(&serde_json::json!({ "tool": tool, "args": args }))
        .send()
        .await
        .map_err(|e| format!("remote request failed: {}", e))?;
    let status = resp.status();
    let text = resp.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("remote API error ({}): {}", status, text));
    }
    serde_json::from_str(&text).map_err(|e| format!("parse remote response: {} body: {}", e, &text[..text.len().min(200)]))
}

/// Extract sessions array from remote API response, handling both formats:
/// - Old: { "result": [ ... ] }
/// - New (MCP): { "result": { "content": [...], "details": { "sessions": [...] } } }
fn extract_sessions(result: &serde_json::Value) -> Vec<serde_json::Value> {
    let r = result.get("result").unwrap_or(result);
    if let Some(sessions) = r.pointer("/details/sessions").and_then(|v| v.as_array()) {
        return sessions.clone();
    }
    if let Some(arr) = r.as_array() {
        return arr.clone();
    }
    vec![]
}

/// Check if a session is active (local mode fallback).
fn is_session_active(s: &serde_json::Value) -> bool {
    if let Some(active) = s.get("active").and_then(|v| v.as_bool()) {
        return active;
    }
    if let Some(updated_at) = s.get("updatedAt").and_then(|v| v.as_u64()) {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        return now_ms.saturating_sub(updated_at) < 3_000;
    }
    false
}

/// Check Queue status from session_status statusText.
fn is_queue_active(status_text: &str) -> bool {
    status_text.lines().any(|line| {
        if let Some(q) = line.split("Queue:").nth(1) {
            let q = q.trim();
            !q.starts_with("collect") && !q.starts_with("idle") && !q.starts_with("waiting")
        } else {
            false
        }
    })
}

/// Remote activity detection: Queue active (instant) OR updatedAt within 3s (smooth stop).
async fn is_remote_session_active(url: &str, token: &str, session_key: &str, s: &serde_json::Value) -> bool {
    if let Ok(status) = invoke_tool(url, token, "session_status", serde_json::json!({"sessionKey": session_key})).await {
        let sr = status.get("result").unwrap_or(&status);
        let det = sr.get("details").unwrap_or(sr);
        if let Some(text) = det["statusText"].as_str() {
            if is_queue_active(text) {
                return true;
            }
        }
    }
    // Queue says idle — use updatedAt as a brief buffer for smooth transition
    is_session_active(s)
}

/// Parse tail lines of a session .jsonl to determine if an agent is active.
///
/// OpenClaw JSONL format: each line is `{"type":"message","message":{...}}`
/// Key fields on `message`:
///   - `role`: "user" | "assistant" | "toolResult"
///   - `usage`: present (object) when an API call is complete
///   - `content`: array of `{type: "text"|"toolCall"|"thinking"|"image", ...}`
///   - NOTE: stop_reason is NOT present in OpenClaw JSONL
///
/// A single turn may involve multiple API calls (tool use loop):
///   1. user message          content=['text']           ← user prompt
///   2. assistant message     content=['toolCall']       ← calls a tool, NOT done
///   3. toolResult message    content=['text']           ← tool output
///   4. assistant message     content=['toolCall']       ← calls another tool, still NOT done
///   5. toolResult message    content=['text']           ← tool output
///   6. assistant message     content=['text']           ← final reply, turn done
///
/// Between steps 2→3 and 4→5 the queue briefly goes idle, but the turn is NOT over.
/// We check: if the last assistant message has "toolCall" content, the turn continues.
/// Also: if the last message is "toolResult", the agent is about to process it → active.
/// This affects: pet working/idle animation, completion sound, session active indicator.
fn check_agent_active_from_lines(lines: &[String]) -> bool {
    let mut last_role = String::new();
    let mut has_usage = false;
    let mut has_tool_call = false;
    for line in lines.iter().rev() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if val["type"].as_str() == Some("message") {
                last_role = val["message"]["role"].as_str().unwrap_or("").to_string();
                has_usage = val["message"]["usage"].is_object();
                // Check if assistant message contains a toolCall content block
                if let Some(content) = val["message"]["content"].as_array() {
                    has_tool_call = content.iter().any(|c| c["type"].as_str() == Some("toolCall"));
                }
                break;
            }
        }
    }
    // Active when:
    //   - last msg is "user" → waiting for assistant response
    //   - last msg is "toolResult" → agent will process tool output next
    //   - last msg is "assistant" without usage → still streaming
    //   - last msg is "assistant" with toolCall content → called a tool, turn continues
    // Inactive when:
    //   - last msg is "assistant" with usage, no toolCall → turn truly ended
    last_role == "user"
        || last_role == "toolResult"
        || (last_role == "assistant" && (!has_usage || has_tool_call))
}

/// Build AgentHealth with session-level data from sessions.json + tail outputs.
fn build_agent_health_from_meta(
    agent_id: &str,
    meta_json: &str,
    tails: &std::collections::HashMap<String, Vec<String>>,
) -> AgentHealth {
    let mut sessions = Vec::new();
    let mut any_active = false;

    if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(meta_json) {
        for (key, val) in map.iter() {
            let sf = val["sessionFile"].as_str().unwrap_or("");
            if sf.is_empty() { continue; }
            // Match session file path to tail output by basename
            #[cfg(windows)]
            let basename = sf.rsplit(|c: char| c == '/' || c == '\\').next().unwrap_or("");
            #[cfg(not(windows))]
            let basename = sf.rsplit('/').next().unwrap_or("");
            let active = if let Some(lines) = tails.get(basename) {
                check_agent_active_from_lines(lines)
            } else {
                false
            };
            if active { any_active = true; }
            sessions.push(SessionHealth { key: key.clone(), active });
        }
    }

    // Fallback: no sessions.json or parse failed — check all tails directly (v1.3.3 behavior)
    if sessions.is_empty() && !tails.is_empty() {
        for (fname, lines) in tails {
            let active = check_agent_active_from_lines(lines);
            if active { any_active = true; }
            // Use filename (without .jsonl) as session key
            let key = fname.strip_suffix(".jsonl").unwrap_or(fname).to_string();
            sessions.push(SessionHealth { key, active });
        }
    }

    AgentHealth { agent_id: agent_id.to_string(), active: any_active, sessions }
}

/// Get the SSH control socket path for a given host.
/// On macOS/Linux: /tmp/oc-claw-ssh-user@host:22
/// On Windows: returns a path in %TEMP% (used only as a "marker" since ControlMaster
/// is not supported; the marker file tracks whether a connection was recently validated).
fn ssh_control_path(ssh_user: &str, ssh_host: &str) -> String {
    #[cfg(unix)]
    { format!("/tmp/oc-claw-ssh-{}@{}:22", ssh_user, ssh_host) }
    #[cfg(windows)]
    {
        let temp = std::env::temp_dir();
        temp.join(format!("oc-claw-ssh-{}@{}.marker", ssh_user, ssh_host))
            .to_string_lossy().to_string()
    }
}

/// Ensure an SSH ControlMaster socket is established (called once, reused by all ssh_exec).
/// On Windows, ControlMaster is not available — we just validate the connection once
/// and create a marker file. Each ssh_exec call will open its own SSH connection.
/// Implements exponential backoff on connection failure (15s, 30s, 60s, … capped at 300s)
/// to avoid flooding the server with reconnection attempts.
async fn ensure_ssh_master(ssh_host: &str, ssh_user: &str) -> Result<(), String> {
    let host_key = format!("{}@{}", ssh_user, ssh_host);
    if let Some(remaining) = ssh_backoff_remaining(&host_key) {
        return Err(format!("SSH connection to {} backing off, retry in {}s", host_key, remaining));
    }

    let control_path = ssh_control_path(ssh_user, ssh_host);
    // Fast path: socket/marker already exists, reuse the master connection.
    if std::path::Path::new(&control_path).exists() { return Ok(()); }

    // Per-host lock so only one task establishes the master at a time.
    use std::sync::OnceLock;
    use tokio::sync::Mutex as TokioMutex;
    static LOCKS: OnceLock<Mutex<HashMap<String, std::sync::Arc<TokioMutex<()>>>>> = OnceLock::new();
    let lock = {
        let mut locks = LOCKS.get_or_init(|| Mutex::new(HashMap::new())).lock().unwrap();
        locks.entry(host_key.clone()).or_insert_with(|| Arc::new(TokioMutex::new(()))).clone()
    };
    let _guard = lock.lock().await;
    // Re-check after acquiring the lock
    if std::path::Path::new(&control_path).exists() { return Ok(()); }

    #[cfg(unix)]
    {
        let cp = format!("ControlPath={}", control_path);
        let child = tokio::process::Command::new("ssh")
            .args([
                "-o", "StrictHostKeyChecking=no",
                "-o", "BatchMode=yes",
                "-o", "ConnectTimeout=10",
                "-o", "ControlMaster=yes",
                "-o", &cp,
                "-o", "ControlPersist=600",
                "-o", "ServerAliveInterval=15",
                "-o", "ServerAliveCountMax=3",
                "-fN",
                &host_key,
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("ssh master spawn: {}", e))?;

        let child_id = child.id();

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            child.wait_with_output(),
        ).await;

        let output = match result {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                ssh_backoff_record_failure(&host_key);
                return Err(format!("ssh master wait: {}", e));
            }
            Err(_) => {
                if let Some(pid) = child_id {
                    unsafe { libc::kill(pid as i32, libc::SIGKILL); }
                }
                ssh_backoff_record_failure(&host_key);
                return Err(format!("ssh master to {} timed out after 15s", host_key));
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ssh_backoff_record_failure(&host_key);
            let count = ssh_backoff_map().lock().unwrap().get(&host_key).map(|s| s.fail_count).unwrap_or(0);
            let code = output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
            log::warn!("[ssh] connection to {} failed (attempt {}), entering backoff", host_key, count);
            return Err(format!("SSH master failed [exit {}]: {}", code, stderr));
        }

        // Wait for the socket file to appear
        for _ in 0..30 {
            if std::path::Path::new(&control_path).exists() { break; }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if !std::path::Path::new(&control_path).exists() {
            ssh_backoff_record_failure(&host_key);
            return Err(format!("ssh master socket for {} never appeared", host_key));
        }
    }

    #[cfg(windows)]
    {
        // Windows: use persistent SSH subprocess multiplexer instead of per-command
        // connections. This avoids the TCP+SSH handshake overhead on every call and
        // prevents hitting server-side MaxStartups limits.
        if let Err(e) = win_ssh_mux::ensure(ssh_user, ssh_host).await {
            ssh_backoff_record_failure(&host_key);
            let count = ssh_backoff_map().lock().unwrap().get(&host_key).map(|s| s.fail_count).unwrap_or(0);
            log::warn!("[ssh] connection to {} failed (attempt {}), entering backoff", host_key, count);
            return Err(format!("SSH connection failed: {}", e));
        }
        // Create marker file so the fast-path check at the top works.
        let _ = std::fs::write(&control_path, "connected");
    }

    // Detect which key was used by querying ssh config for this host.
    let mut ssh_g_cmd = tokio::process::Command::new("ssh");
    ssh_g_cmd.args(["-G", &host_key])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    hide_window_tokio_cmd(&mut ssh_g_cmd);
    if let Ok(cfg_output) = ssh_g_cmd.output().await
    {
        let cfg = String::from_utf8_lossy(&cfg_output.stdout);
        for line in cfg.lines() {
            if let Some(path) = line.strip_prefix("identityfile ") {
                let expanded = path.replace("~", &home_dir_string());
                if std::path::Path::new(&expanded).exists() {
                    log::info!("[ssh] {} will use key: {}", host_key, expanded);
                    ssh_key_map().lock().unwrap().insert(host_key.clone(), expanded);
                    break;
                }
            }
        }
    }

    ssh_backoff_reset(&host_key);
    Ok(())
}

/// Execute a command on remote host via SSH.
/// On macOS/Linux: reuses ControlMaster socket for fast multiplexed connections.
/// On Windows: routes through a persistent SSH subprocess (win_ssh_mux) so all
///   commands share a single TCP connection instead of opening one per call.
/// If the command fails (e.g. stale socket), removes the socket and retries once.
async fn ssh_exec(ssh_host: &str, ssh_user: &str, cmd: &str) -> Result<String, String> {
    ensure_ssh_master(ssh_host, ssh_user).await?;
    let safe_cmd = format!(
        "export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:$PATH && {}",
        cmd
    );

    #[cfg(windows)]
    {
        match win_ssh_mux::exec(ssh_user, ssh_host, &safe_cmd).await {
            Ok(out) => return Ok(out),
            Err(e) if e.contains("transport error") || e.contains("connection lost") || e.contains("process exited") || e.contains("not connected") || e.contains("timed out") => {
                log::warn!("[ssh] transport error, removing marker and retrying: {}", e);
                let _ = tokio::fs::remove_file(&ssh_control_path(ssh_user, ssh_host)).await;
                ensure_ssh_master(ssh_host, ssh_user).await?;
                return win_ssh_mux::exec(ssh_user, ssh_host, &safe_cmd).await;
            }
            Err(e) => return Err(e),
        }
    }

    #[cfg(unix)]
    {
        let target = format!("{}@{}", ssh_user, ssh_host);
        let control_path = ssh_control_path(ssh_user, ssh_host);
        let cp = format!("ControlPath={}", control_path);

        let mut ssh_args: Vec<&str> = vec![
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", &cp,
        ];
        ssh_args.push(&target);
        ssh_args.push(&safe_cmd);

        let output = tokio::process::Command::new("ssh")
            .args(&ssh_args)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("ssh: {}", e))?;
        if output.status.success() {
            return Ok(String::from_utf8_lossy(&output.stdout).to_string());
        }

        let exit_code = output.status.code().unwrap_or(-1);
        if exit_code != 255 {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut msg = format!("ssh cmd failed [exit {}]", exit_code);
            if !stderr.trim().is_empty() { msg.push_str(&format!("\nstderr: {}", stderr.trim())); }
            if !stdout.trim().is_empty() { msg.push_str(&format!("\nstdout: {}", stdout.trim())); }
            return Err(msg);
        }

        log::warn!("[ssh] transport error (exit 255), removing stale socket and retrying");
        let _ = tokio::fs::remove_file(&control_path).await;
        ensure_ssh_master(ssh_host, ssh_user).await?;

        let mut ssh_args2: Vec<&str> = vec![
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", &cp,
        ];
        ssh_args2.push(&target);
        ssh_args2.push(&safe_cmd);

        let output = tokio::process::Command::new("ssh")
            .args(&ssh_args2)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
            .await
            .map_err(|e| format!("ssh: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let code = output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
            let mut msg = format!("ssh cmd failed [exit {}]", code);
            if !stderr.trim().is_empty() { msg.push_str(&format!("\nstderr: {}", stderr.trim())); }
            if !stdout.trim().is_empty() { msg.push_str(&format!("\nstdout: {}", stdout.trim())); }
            return Err(msg);
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

/// Close an active SSH ControlMaster socket (macOS/Linux) or persistent mux subprocess (Windows).
async fn close_ssh_master(ssh_host: &str, ssh_user: &str) -> Result<(), String> {
    let control_path = ssh_control_path(ssh_user, ssh_host);
    #[cfg(unix)]
    {
        if std::path::Path::new(&control_path).exists() {
            let target = format!("{}@{}", ssh_user, ssh_host);
            let cp = format!("ControlPath={}", control_path);
            let _ = tokio::process::Command::new("ssh")
                .args(["-o", &cp, "-O", "exit", &target])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .output()
                .await;
        }
    }
    #[cfg(windows)]
    {
        win_ssh_mux::kill(ssh_user, ssh_host).await;
    }
    let _ = tokio::fs::remove_file(&control_path).await;
    ssh_backoff_reset(&format!("{}@{}", ssh_user, ssh_host));
    log::info!("[close_ssh_master] closed socket for {}@{}", ssh_user, ssh_host);
    Ok(())
}

fn tray_labels(lang: &str) -> (&'static str, &'static str, &'static str) {
    match lang {
        "zh" => ("显示", "隐藏", "退出"),
        "ja" => ("表示", "非表示", "終了"),
        "ko" => ("표시", "숨기기", "종료"),
        "es" => ("Mostrar", "Ocultar", "Salir"),
        "fr" => ("Afficher", "Masquer", "Quitter"),
        _ => ("Show", "Hide", "Quit"),
    }
}

#[tauri::command]
fn update_tray_language(app: tauri::AppHandle, lang: String) -> Result<(), String> {
    let (show_label, hide_label, quit_label) = tray_labels(&lang);
    let show = MenuItem::with_id(&app, "show", show_label, true, None::<&str>).map_err(|e| e.to_string())?;
    let hide = MenuItem::with_id(&app, "hide", hide_label, true, None::<&str>).map_err(|e| e.to_string())?;
    let quit = MenuItem::with_id(&app, "quit", quit_label, true, None::<&str>).map_err(|e| e.to_string())?;
    let menu = Menu::with_items(&app, &[&show, &hide, &quit]).map_err(|e| e.to_string())?;
    if let Some(tray) = app.tray_by_id("main") {
        tray.set_menu(Some(menu)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn exit_app(app: tauri::AppHandle) {
    app.exit(0);
}

/// Returns the SSH key path that was used to authenticate a connection,
/// or null if unknown (e.g. socket was already established before this session).
#[tauri::command]
fn get_ssh_key_info(ssh_host: String, ssh_user: String) -> Option<String> {
    let key = format!("{}@{}", ssh_user, ssh_host);
    ssh_key_map().lock().unwrap().get(&key).cloned()
}

/// Reset backoff, gracefully close the existing SSH master process, and
/// remove the socket — so the next connection starts completely fresh.
/// Called before user-initiated "test connection" to avoid making the user
/// wait out a backoff timer or fight a stale/conflicting master process.
#[tauri::command]
async fn reset_ssh(ssh_host: String, ssh_user: String) {
    let host_key = format!("{}@{}", ssh_user, ssh_host);
    ssh_backoff_reset(&host_key);
    // Gracefully shut down the existing master process via `-O exit`,
    // then remove the socket file. This prevents orphaned ssh processes
    // from piling up and conflicting with the new master.
    let _ = close_ssh_master(&ssh_host, &ssh_user).await;
    // Clear cached key info since we're starting fresh
    ssh_key_map().lock().unwrap().remove(&host_key);
    log::info!("[reset_ssh] cleared backoff, killed master, and reset for {}", host_key);
}

#[tauri::command]
async fn close_ssh(ssh_host: Option<String>, ssh_user: Option<String>) -> Result<(), String> {
    let sh = ssh_host.unwrap_or_default();
    let su = ssh_user.unwrap_or_default();
    if sh.is_empty() || su.is_empty() {
        // Clean up all stale SSH sockets/markers
        #[cfg(unix)]
        let scan_dir = PathBuf::from("/tmp");
        #[cfg(windows)]
        let scan_dir = std::env::temp_dir();

        if let Ok(mut entries) = tokio::fs::read_dir(&scan_dir).await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("oc-claw-ssh-") {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                    log::info!("[close_ssh] removed stale socket/marker: {}", name);
                }
            }
        }
        #[cfg(windows)]
        { win_ssh_mux::kill_all().await; }
        // Clear all backoff entries
        ssh_backoff_map().lock().unwrap().clear();
        return Ok(());
    }
    close_ssh_master(&sh, &su).await
}

/// Resolve the backgrounds directory — dev: public/assets/backgrounds, prod: app_data/backgrounds
fn backgrounds_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let project_root = exe
            .parent().and_then(|p| p.parent()).and_then(|p| p.parent()).and_then(|p| p.parent())
            .ok_or("cannot resolve project root")?;
        Ok(project_root.join("public").join("assets").join("backgrounds"))
    } else {
        app.path().app_data_dir().map(|p| p.join("backgrounds")).map_err(|e| e.to_string())
    }
}

/// List available background image filenames.
#[tauri::command]
async fn list_backgrounds(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let mut names = Vec::new();
    // In dev mode, backgrounds_dir already points to public/assets/backgrounds/ which has everything.
    // In prod, scan both bundled (read-only) and custom (app_data) directories.
    let bg_dir = backgrounds_dir(&app)?;
    let mut dirs_to_scan: Vec<PathBuf> = vec![bg_dir];
    if !cfg!(debug_assertions) {
        if let Ok(rd) = app.path().resource_dir() {
            dirs_to_scan.push(rd.join("assets").join("backgrounds"));
        }
    }
    for dir in &dirs_to_scan {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if (name.ends_with(".png") || name.ends_with(".jpg") || name.ends_with(".webp"))
                        && !names.contains(&name.to_string())
                    {
                        names.push(name.to_string());
                    }
                }
            }
        }
    }
    names.sort();
    Ok(names)
}

/// Save a user-uploaded background image. Returns the saved filename.
#[tauri::command]
async fn save_background(app: tauri::AppHandle, file_name: String, data_url: String) -> Result<String, String> {
    use base64::Engine;
    if file_name.contains("..") || file_name.contains('/') || file_name.contains('\\') {
        return Err("invalid file name".into());
    }
    let dir = backgrounds_dir(&app)?;
    tokio::fs::create_dir_all(&dir).await.map_err(|e| format!("create dir: {e}"))?;
    let b64 = data_url.find(",").map(|i| &data_url[i + 1..]).unwrap_or(&data_url);
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|e| format!("base64: {e}"))?;
    tokio::fs::write(dir.join(&file_name), &bytes).await.map_err(|e| format!("write: {e}"))?;
    Ok(file_name)
}

/// Serve a background image as base64 data URL (for custom backgrounds not in public/).
#[tauri::command]
async fn get_background_data(app: tauri::AppHandle, file_name: String) -> Result<String, String> {
    use base64::Engine;
    if file_name.contains("..") || file_name.contains('/') || file_name.contains('\\') {
        return Err("invalid file name".into());
    }
    // Try bundled first, then custom dir
    let bundled = app.path().resource_dir().map_err(|e| e.to_string())?
        .join("assets").join("backgrounds").join(&file_name);
    let custom = backgrounds_dir(&app)?.join(&file_name);
    let path = if bundled.exists() { bundled } else { custom };
    let data = tokio::fs::read(&path).await.map_err(|e| format!("read: {e}"))?;
    let ext = file_name.rsplit('.').next().unwrap_or("png");
    let mime = match ext { "jpg" | "jpeg" => "image/jpeg", "webp" => "image/webp", _ => "image/png" };
    Ok(format!("data:{};base64,{}", mime, base64::engine::general_purpose::STANDARD.encode(&data)))
}

#[tauri::command]
async fn read_local_file(path: String) -> Result<String, String> {
    use base64::Engine;
    let data = tokio::fs::read(&path).await.map_err(|e| format!("read failed: {e}"))?;
    Ok(base64::engine::general_purpose::STANDARD.encode(&data))
}

async fn ssh_read_file(ssh_host: &str, ssh_user: &str, path: &str) -> Result<String, String> {
    // Use double quotes so ~ expands, but escape any embedded double quotes
    let escaped = path.replace('"', r#"\""#);
    ssh_exec(ssh_host, ssh_user, &format!("cat \"{}\"", escaped)).await
}

/// Check if an agent is active by reading the tail of the latest .jsonl file via SSH.
/// If the last message-type entry is a user message (no assistant response yet), agent is working.
async fn ssh_is_agent_active(ssh_host: &str, ssh_user: &str, agent_id: &str) -> bool {
    let agent_dir = if agent_id.is_empty() { "main" } else { agent_id };
    // Read the last 5 lines of the newest .jsonl file
    let cmd = format!(
        "f=$(ls -t $HOME/.openclaw/agents/{}/sessions/*.jsonl 2>/dev/null | head -1); [ -f \"$f\" ] && tail -5 \"$f\"",
        agent_dir
    );
    let output = match ssh_exec(ssh_host, ssh_user, &cmd).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    // Walk backwards through lines to find the last message entry
    let lines: Vec<String> = output.lines().map(|l| l.to_string()).collect();
    check_agent_active_from_lines(&lines)
}

/// Check if a specific session file is active by reading its tail.
async fn ssh_is_session_file_active(ssh_host: &str, ssh_user: &str, session_file: &str) -> bool {
    let escaped = session_file.replace('"', r#"\""#);
    let cmd = format!("tail -5 \"{}\" 2>/dev/null", escaped);
    let output = match ssh_exec(ssh_host, ssh_user, &cmd).await {
        Ok(s) => s,
        Err(_) => return false,
    };
    for line in output.lines().rev() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if val["type"].as_str() == Some("message") {
                let role = val["message"]["role"].as_str().unwrap_or("");
                let has_usage = val["message"]["usage"].is_object();
                return role == "user" || (role == "assistant" && !has_usage);
            }
        }
    }
    false
}

fn remote_sessions_json_path(agent_id: &str) -> String {
    let agent_dir = if agent_id.is_empty() { "main" } else { agent_id };
    format!("$HOME/.openclaw/agents/{}/sessions/sessions.json", agent_dir)
}

fn sessions_json_path(agent_id: &str) -> PathBuf {
    let home = home_dir_string();
    let agent_dir = if agent_id.is_empty() { "main" } else { agent_id };
    PathBuf::from(home).join(".openclaw").join("agents").join(agent_dir).join("sessions").join("sessions.json")
}

#[tauri::command]
async fn get_status(_gateway_url: String, _token: String, agent_id: String) -> Result<GatewayStatus, String> {
    // Step 1: check gateway is running
    #[cfg(unix)]
    {
        let pgrep_gw = tokio::process::Command::new("pgrep")
            .args(["-x", "openclaw-gateway"])
            .output()
            .await
            .map_err(|e| format!("pgrep: {}", e))?;
        if !pgrep_gw.status.success() {
            return Err("gateway not running".into());
        }
    }
    #[cfg(windows)]
    {
        // On Windows, openclaw gateway runs as a node.exe process (not a separate
        // openclaw-gateway.exe binary).  Check whether anything is listening on
        // the default gateway port (18789) instead.
        let mut ps_cmd = tokio::process::Command::new("powershell");
        ps_cmd.args([
                "-NoProfile",
                "-Command",
                "(Get-NetTCPConnection -LocalPort 18789 -State Listen -ErrorAction SilentlyContinue | Measure-Object).Count",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());
        hide_window_tokio_cmd(&mut ps_cmd);
        let listening = ps_cmd.output()
            .await
            .map_err(|e| format!("powershell: {}", e))?;
        let count_str = String::from_utf8_lossy(&listening.stdout).trim().to_string();
        let count: u32 = count_str.parse().unwrap_or(0);
        if count == 0 {
            return Err("gateway not running".into());
        }
    }

    // Step 2: check if any .jsonl is being actively used for this agent
    let active_agents = lsof_active_agents().await;
    let agent_dir = if agent_id.is_empty() { "main" } else { &agent_id };
    let active = active_agents.contains(agent_dir);

    // Step 3: read sessions.json → session list
    let path = sessions_json_path(&agent_id);
    let sessions = match tokio::fs::read_to_string(&path).await {
        Ok(content) => {
            let map: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&content).unwrap_or_default();
            map.iter()
                .map(|(key, val)| SessionInfo {
                    id: val["sessionId"].as_str().unwrap_or(key).to_string(),
                    label: Some(key.clone()),
                    status: "stored".into(),
                    model: None,
                    channel: val["lastChannel"].as_str().map(|s| s.to_string()),
                })
                .collect()
        }
        Err(_) => vec![],
    };

    Ok(GatewayStatus { active, sessions })
}

#[tauri::command]
async fn send_chat(message: String, agent_id: String, state: tauri::State<'_, ActiveAgentPid>) -> Result<String, String> {
    // Read sessions.json to get the first sessionId
    let path = sessions_json_path(&agent_id);
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read sessions.json: {}", e))?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    let session_id = map
        .values()
        .find_map(|v| v["sessionId"].as_str())
        .ok_or("no session found")?
        .to_string();

    // Spawn openclaw agent and track its PID so interrupt_agent can SIGINT it
    let child = tokio::process::Command::new("openclaw")
        .args([
            "agent",
            "--message",
            &message,
            "--session-id",
            &session_id,
            "--json",
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("openclaw agent: {}", e))?;

    // Store PID for interrupt_agent
    if let Some(pid) = child.id() {
        *state.pid.lock().unwrap() = Some(pid);
    }

    let output = child.wait_with_output().await.map_err(|e| format!("openclaw agent wait: {}", e))?;

    // Clear PID once done
    *state.pid.lock().unwrap() = None;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Try to parse JSON from stdout first — exit code may be non-zero due to config warnings
    // even when the agent turn succeeded
    if let Some(json_start) = stdout.find('{') {
        if let Ok(body) = serde_json::from_str::<serde_json::Value>(&stdout[json_start..]) {
            let reply = body["result"]["payloads"]
                .as_array()
                .and_then(|arr| arr.first())
                .and_then(|p| p["text"].as_str())
                .unwrap_or("")
                .to_string();
            return Ok(reply);
        }
    }

    // No usable JSON — treat as real failure
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("openclaw agent failed: {}", stderr));
    }

    Ok(String::new())
}

/// Built-in assets directory (read-only in production).
fn builtin_assets_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let project_root = exe
            .parent().and_then(|p| p.parent()).and_then(|p| p.parent()).and_then(|p| p.parent())
            .ok_or("cannot resolve project root")?;
        Ok(project_root.join("public").join("assets").join("builtin"))
    } else {
        let dir = app.path().resource_dir().map(|p| p.join("assets").join("builtin")).map_err(|e| e.to_string())?;
        log::info!("[assets] builtin_assets_dir={} exists={}", dir.display(), dir.exists());
        Ok(dir)
    }
}

/// Custom (user-created) assets directory (always writable).
fn custom_assets_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    if cfg!(debug_assertions) {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let project_root = exe
            .parent().and_then(|p| p.parent()).and_then(|p| p.parent()).and_then(|p| p.parent())
            .ok_or("cannot resolve project root")?;
        Ok(project_root.join("public").join("assets").join("custom"))
    } else {
        app.path().app_data_dir().map(|p| p.join("characters")).map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn scan_characters(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let mut results = vec![];

    // Load characters.json for IP mapping and defaults
    let mut ip_map: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut config_defaults: Option<serde_json::Map<String, serde_json::Value>> = None;
    if let Ok(builtin_dir) = builtin_assets_dir(&app) {
        let config_path = builtin_dir.join("characters.json");
        if let Ok(data) = std::fs::read_to_string(&config_path) {
            if let Ok(config) = serde_json::from_str::<serde_json::Value>(&data) {
                if let Some(ips) = config.get("ips").and_then(|v| v.as_array()) {
                    for ip in ips {
                        let ip_name = ip.get("name").and_then(|n| n.as_str()).unwrap_or("");
                        if let Some(chars) = ip.get("characters").and_then(|c| c.as_array()) {
                            for ch in chars {
                                if let Some(ch_name) = ch.as_str() {
                                    ip_map.insert(ch_name.to_string(), ip_name.to_string());
                                }
                            }
                        }
                    }
                }
                if let Some(defaults) = config.get("defaults").and_then(|d| d.as_object()) {
                    config_defaults = Some(defaults.clone());
                }
            }
        }
    }

    // Scan a directory and append characters to results
    fn scan_dir(base: &std::path::Path, url_prefix: &str, builtin: bool, ip_map: &std::collections::HashMap<String, String>, results: &mut Vec<serde_json::Value>) {
        let entries = match std::fs::read_dir(base) {
            Ok(e) => e,
            Err(_) => return,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            if !entry.path().is_dir() { continue; }
            let name = entry.file_name().to_string_lossy().to_string();

            let mut work_gifs = vec![];
            let mut rest_gifs = vec![];
            let mut crawl_gifs = vec![];
            let mut angry_gifs = vec![];
            let mut shy_gifs = vec![];
            let pet_dir = entry.path().join("pet");
            if pet_dir.exists() {
                for (subdir, target) in [("work", &mut work_gifs), ("rest", &mut rest_gifs), ("crawl", &mut crawl_gifs), ("angry", &mut angry_gifs), ("shy", &mut shy_gifs)] {
                    if let Ok(files) = std::fs::read_dir(pet_dir.join(subdir)) {
                        for f in files.filter_map(|f| f.ok()) {
                            if f.path().extension().map(|e| e == "gif").unwrap_or(false) {
                                target.push(format!("{}/{}/pet/{}/{}", url_prefix, name, subdir, f.file_name().to_string_lossy()));
                            }
                        }
                    }
                }
            }

            let mut mini_actions: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            let mini_dir = entry.path().join("mini");
            if mini_dir.exists() {
                if let Ok(cats) = std::fs::read_dir(&mini_dir) {
                    for cat in cats.filter_map(|c| c.ok()) {
                        if !cat.path().is_dir() { continue; }
                        let cat_name = cat.file_name().to_string_lossy().to_string();
                        let mut gifs = vec![];
                        if let Ok(files) = std::fs::read_dir(cat.path()) {
                            for f in files.filter_map(|f| f.ok()) {
                                if f.path().extension().map(|e| e == "gif").unwrap_or(false) {
                                    gifs.push(serde_json::Value::String(
                                        format!("{}/{}/mini/{}/{}", url_prefix, name, cat_name, f.file_name().to_string_lossy())
                                    ));
                                }
                            }
                        }
                        if !gifs.is_empty() {
                            mini_actions.insert(cat_name, serde_json::Value::Array(gifs));
                        }
                    }
                }
            }

            // Scan large/ directory for large-mascot videos. On macOS we want
            // to prefer HEVC-with-alpha containers first, because WKWebView
            // does not render WebM alpha correctly.
            let mut large_actions: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
            let large_dir = entry.path().join("large");
            if large_dir.exists() {
                if let Ok(files) = std::fs::read_dir(&large_dir) {
                    let mut preferred: std::collections::HashMap<String, (u8, String)> = std::collections::HashMap::new();
                    for f in files.filter_map(|f| f.ok()) {
                        if let Some(ext) = f.path().extension().and_then(|e| e.to_str()) {
                            let priority = match ext.to_ascii_lowercase().as_str() {
                                "mov" => 0,
                                "mp4" => 1,
                                "webm" => 2,
                                _ => continue,
                            };
                            let stem = f.path().file_stem().unwrap_or_default().to_string_lossy().to_string();
                            let url = format!("{}/{}/large/{}", url_prefix, name, f.file_name().to_string_lossy());
                            let should_replace = preferred
                                .get(&stem)
                                .map(|(existing_priority, _)| priority < *existing_priority)
                                .unwrap_or(true);
                            if should_replace {
                                preferred.insert(stem, (priority, url));
                            }
                        }
                    }
                    for (stem, (_, url)) in preferred {
                        large_actions.insert(stem, serde_json::Value::String(url));
                    }
                }
            }

            let mut char_obj = serde_json::Map::new();
            char_obj.insert("builtin".into(), serde_json::Value::Bool(builtin));
            if let Some(ip_name) = ip_map.get(&name) {
                char_obj.insert("ip".into(), serde_json::Value::String(ip_name.clone()));
            }
            char_obj.insert("name".into(), serde_json::Value::String(name));
            char_obj.insert("workGifs".into(), serde_json::Value::Array(work_gifs.into_iter().map(serde_json::Value::String).collect()));
            char_obj.insert("restGifs".into(), serde_json::Value::Array(rest_gifs.into_iter().map(serde_json::Value::String).collect()));
            if !crawl_gifs.is_empty() {
                char_obj.insert("crawlGifs".into(), serde_json::Value::Array(crawl_gifs.into_iter().map(serde_json::Value::String).collect()));
            }
            if !angry_gifs.is_empty() {
                char_obj.insert("angryGifs".into(), serde_json::Value::Array(angry_gifs.into_iter().map(serde_json::Value::String).collect()));
            }
            if !shy_gifs.is_empty() {
                char_obj.insert("shyGifs".into(), serde_json::Value::Array(shy_gifs.into_iter().map(serde_json::Value::String).collect()));
            }
            if !mini_actions.is_empty() {
                char_obj.insert("miniActions".into(), serde_json::Value::Object(mini_actions));
            }
            if !large_actions.is_empty() {
                char_obj.insert("largeActions".into(), serde_json::Value::Object(large_actions));
            }
            results.push(serde_json::Value::Object(char_obj));
        }
    }

    // Scan built-in assets
    // On Windows, WebView2 maps custom schemes to http://<scheme>.localhost/
    // instead of <scheme>://localhost/. Use the platform-correct URL prefix.
    let builtin_prefix = if cfg!(debug_assertions) {
        "/assets/builtin"
    } else if cfg!(target_os = "windows") {
        "http://localasset.localhost"
    } else {
        "localasset://localhost"
    };
    if let Ok(builtin_dir) = builtin_assets_dir(&app) {
        log::info!("[scan_characters] scanning builtin: {} (exists={})", builtin_dir.display(), builtin_dir.exists());
        scan_dir(&builtin_dir, builtin_prefix, true, &ip_map, &mut results);
        log::info!("[scan_characters] found {} characters after builtin scan", results.len());
    }

    // Scan custom assets
    let custom_prefix = if cfg!(debug_assertions) {
        "/assets/custom"
    } else if cfg!(target_os = "windows") {
        "http://customasset.localhost"
    } else {
        "customasset://localhost"
    };
    if let Ok(custom_dir) = custom_assets_dir(&app) {
        log::info!("[scan_characters] scanning custom: {} (exists={})", custom_dir.display(), custom_dir.exists());
        scan_dir(&custom_dir, custom_prefix, false, &ip_map, &mut results);
    }

    let mut response = serde_json::Map::new();
    response.insert("characters".into(), serde_json::Value::Array(results));
    if let Some(defaults) = config_defaults {
        response.insert("defaults".into(), serde_json::Value::Object(defaults));
    }
    Ok(serde_json::Value::Object(response))
}

#[tauri::command]
async fn save_character_gif(
    app: tauri::AppHandle,
    char_name: String,
    file_name: String,
    subfolder: String,
    data_url: String,
) -> Result<(), String> {
    use base64::Engine;

    if char_name.contains("..") || char_name.contains('/') || char_name.contains('\\') {
        return Err("invalid character name".into());
    }
    if file_name.contains("..") || file_name.contains('/') || file_name.contains('\\') {
        return Err("invalid file name".into());
    }

    let base = custom_assets_dir(&app)?;
    let mut target = base.join(&char_name);
    if !subfolder.is_empty() {
        if subfolder.contains("..") {
            return Err("invalid subfolder".into());
        }
        target = target.join(&subfolder);
    }
    tokio::fs::create_dir_all(&target)
        .await
        .map_err(|e| format!("create dir: {}", e))?;

    let b64 = data_url
        .find(",")
        .map(|i| &data_url[i + 1..])
        .unwrap_or(&data_url);

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .map_err(|e| format!("base64 decode: {}", e))?;

    let filepath = target.join(&file_name);
    tokio::fs::write(&filepath, &bytes)
        .await
        .map_err(|e| format!("write file: {}", e))?;

    Ok(())
}

#[tauri::command]
async fn delete_character_assets(app: tauri::AppHandle, name: String) -> Result<(), String> {
    if name.contains("..") || name.contains('/') || name.contains('\\') {
        return Err("invalid name".into());
    }
    let base = custom_assets_dir(&app)?;
    let target = base.join(&name);
    if target.exists() {
        tokio::fs::remove_dir_all(&target)
            .await
            .map_err(|e| format!("delete: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
async fn delete_character_gif(app: tauri::AppHandle, char_name: String, subfolder: String, file_name: String) -> Result<(), String> {
    if char_name.contains("..") || char_name.contains('/') || char_name.contains('\\') {
        return Err("invalid name".into());
    }
    if subfolder.contains("..") || file_name.contains("..") || file_name.contains('/') || file_name.contains('\\') {
        return Err("invalid path".into());
    }
    let base = custom_assets_dir(&app)?;
    let target = base.join(&char_name).join(&subfolder).join(&file_name);
    if target.exists() {
        tokio::fs::remove_file(&target)
            .await
            .map_err(|e| format!("delete gif: {}", e))?;
    }
    Ok(())
}

#[tauri::command]
async fn get_agents(mode: Option<String>, url: Option<String>, token: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<Vec<AgentInfo>, String> {
    log::info!("[get_agents] mode={:?} ssh_host={:?}", mode, ssh_host);
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            let dirs = ssh_exec(sh, su, "ls -1 $HOME/.openclaw/agents/ 2>/dev/null").await?;
            let mut agents: Vec<AgentInfo> = Vec::new();
            for id in dirs.lines().filter(|l| !l.trim().is_empty()) {
                let id = id.trim().to_string();
                let config_path = format!("$HOME/.openclaw/agents/{}/agent.json", id);
                let (name, emoji) = match ssh_read_file(sh, su, &config_path).await {
                    Ok(c) => {
                        let val: serde_json::Value = serde_json::from_str(&c).unwrap_or_default();
                        (
                            val.get("identityName").or_else(|| val.get("identity_name")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                            val.get("identityEmoji").or_else(|| val.get("identity_emoji")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                        )
                    }
                    Err(_) => (None, None),
                };
                agents.push(AgentInfo { id, identity_name: name, identity_emoji: emoji });
            }
            return Ok(agents);
        }
        // Gateway API fallback
        let url = url.as_deref().unwrap_or("");
        let token = token.as_deref().unwrap_or("");
        let result = invoke_tool(url, token, "agents_list", serde_json::json!({})).await?;
        let r = result.get("result").unwrap_or(&result);
        let agents_arr = r.pointer("/details/agents").and_then(|v| v.as_array())
            .or_else(|| r.as_array());
        let agents: Vec<AgentInfo> = if let Some(arr) = agents_arr {
            arr.iter().filter_map(|v| {
                let id = v["id"].as_str()?.to_string();
                Some(AgentInfo {
                    id,
                    identity_name: v.get("identityName").or_else(|| v.get("identity_name")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                    identity_emoji: v.get("identityEmoji").or_else(|| v.get("identity_emoji")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                })
            }).collect()
        } else if let Some(map) = r.as_object() {
            map.iter()
                .filter(|(_, v)| v.is_object())
                .map(|(id, val)| {
                    AgentInfo {
                        id: id.clone(),
                        identity_name: val.get("identityName").or_else(|| val.get("identity_name")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                        identity_emoji: val.get("identityEmoji").or_else(|| val.get("identity_emoji")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                    }
                }).collect()
        } else {
            return Err(format!("unexpected agents_list format: {}", r));
        };
        return Ok(agents);
    }

    // === local mode ===
    // On Windows, read ~/.openclaw/agents/ directly (no CLI dependency).
    // On macOS/Linux, use the original `openclaw agents list --json` CLI.
    #[cfg(windows)]
    {
        let home = home_dir_string();
        let agents_dir = PathBuf::from(&home).join(".openclaw").join("agents");
        log::info!("[get_agents] local mode, agents_dir={:?}, exists={}", agents_dir, agents_dir.exists());

        let entries = std::fs::read_dir(&agents_dir)
            .map_err(|e| { log::error!("[get_agents] read_dir failed: {}", e); format!("read agents dir: {}", e) })?;

        let mut agents: Vec<AgentInfo> = Vec::new();
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.is_dir() { continue; }
            let id = entry.file_name().to_string_lossy().to_string();
            let config_path = path.join("agent.json");
            let (name, emoji) = if config_path.exists() {
                match std::fs::read_to_string(&config_path) {
                    Ok(c) => {
                        let val: serde_json::Value = serde_json::from_str(&c).unwrap_or_default();
                        (
                            val.get("identityName").or_else(|| val.get("identity_name")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                            val.get("identityEmoji").or_else(|| val.get("identity_emoji")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                        )
                    }
                    Err(_) => (None, None),
                }
            } else {
                (None, None)
            };
            agents.push(AgentInfo { id, identity_name: name, identity_emoji: emoji });
        }
        Ok(agents)
    }
    #[cfg(not(windows))]
    {
        let output = tokio::process::Command::new("openclaw")
            .args(["agents", "list", "--json"])
            .output()
            .await
            .map_err(|e| format!("openclaw agents list: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("openclaw agents list failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let json_start = stdout.find('[').ok_or("no JSON array in agents output")?;
        let json_end = stdout.rfind(']').ok_or("no closing bracket")? + 1;
        let agents: Vec<AgentInfo> =
            serde_json::from_str(&stdout[json_start..json_end]).map_err(|e| e.to_string())?;
        Ok(agents)
    }
}

#[tauri::command]
async fn get_health(mode: Option<String>, url: Option<String>, token: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<HealthResult, String> {
    log::info!("[get_health] mode={:?} ssh_host={:?}", mode, ssh_host);
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            // Single SSH command: read sessions.json + tail each session file per agent
            let cmd = r#"for d in $HOME/.openclaw/agents/*/; do id=$(basename "$d"); sj="$d/sessions/sessions.json"; echo "AGENT:$id"; if [ -f "$sj" ]; then echo "META_START"; cat "$sj"; echo ""; echo "META_END"; fi; for f in "$d"sessions/*.jsonl; do [ -f "$f" ] || continue; echo "TAIL:$(basename "$f")"; tail -5 "$f"; echo "END_TAIL"; done; echo "END_AGENT"; done"#;
            let output = ssh_exec(sh, su, cmd).await.unwrap_or_default();

            let mut agents = Vec::new();
            let mut current_id: Option<String> = None;
            let mut meta_buf = String::new();
            let mut in_meta = false;
            // filename → tail lines
            let mut tails: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
            let mut current_tail_file: Option<String> = None;
            let mut tail_lines: Vec<String> = Vec::new();

            for line in output.lines() {
                if let Some(id) = line.strip_prefix("AGENT:") {
                    // Finalize previous agent
                    if let Some(prev_id) = current_id.take() {
                        let agent = build_agent_health_from_meta(&prev_id, &meta_buf, &tails);
                        agents.push(agent);
                    }
                    current_id = Some(id.to_string());
                    meta_buf.clear();
                    tails.clear();
                    in_meta = false;
                } else if line == "META_START" {
                    in_meta = true;
                    meta_buf.clear();
                } else if line == "META_END" {
                    in_meta = false;
                } else if in_meta {
                    meta_buf.push_str(line);
                    meta_buf.push('\n');
                } else if let Some(fname) = line.strip_prefix("TAIL:") {
                    if let Some(prev_file) = current_tail_file.take() {
                        tails.insert(prev_file, std::mem::take(&mut tail_lines));
                    }
                    current_tail_file = Some(fname.to_string());
                    tail_lines.clear();
                } else if line == "END_TAIL" {
                    if let Some(prev_file) = current_tail_file.take() {
                        tails.insert(prev_file, std::mem::take(&mut tail_lines));
                    }
                } else if line == "END_AGENT" {
                    if let Some(prev_file) = current_tail_file.take() {
                        tails.insert(prev_file, std::mem::take(&mut tail_lines));
                    }
                    if let Some(prev_id) = current_id.take() {
                        let agent = build_agent_health_from_meta(&prev_id, &meta_buf, &tails);
                        agents.push(agent);
                    }
                    meta_buf.clear();
                    tails.clear();
                } else if current_tail_file.is_some() {
                    tail_lines.push(line.to_string());
                }
            }
            // Handle last agent if no END_AGENT
            if let Some(prev_file) = current_tail_file.take() {
                tails.insert(prev_file, tail_lines);
            }
            if let Some(prev_id) = current_id {
                let agent = build_agent_health_from_meta(&prev_id, &meta_buf, &tails);
                agents.push(agent);
            }
            return Ok(HealthResult { agents, gateway_alive: true });
        }
        // Gateway API fallback
        let url = url.as_deref().unwrap_or("");
        let token = token.as_deref().unwrap_or("");
        let result = invoke_tool(url, token, "sessions_list", serde_json::json!({"activeMinutes": 5})).await?;
        let sessions = extract_sessions(&result);
        let mut agent_active: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
        for s in &sessions {
            let agent_id = s["agentId"].as_str()
                .or_else(|| s["key"].as_str().and_then(|k| k.split(':').nth(1)))
                .unwrap_or("main").to_string();
            let session_key = s["key"].as_str().unwrap_or("");
            let active = if !session_key.is_empty() {
                is_remote_session_active(url, token, session_key, s).await
            } else {
                is_session_active(s)
            };
            let entry = agent_active.entry(agent_id).or_insert(false);
            if active { *entry = true; }
        }
        let agents = agent_active.into_iter().map(|(agent_id, active)| AgentHealth { agent_id, active, sessions: vec![] }).collect();
        return Ok(HealthResult { agents, gateway_alive: true });
    }

    // === local mode — content-based detection with session-level data ===
    let home = home_dir_string();
    let agents_dir = std::path::PathBuf::from(&home).join(".openclaw").join("agents");

    // If the OpenClaw gateway process is not running, every session's "active"
    // state in the JSONL files is stale — the gateway was killed mid-turn and
    // never wrote a final inactive message. Force all agents to inactive.
    let gateway_alive = is_openclaw_gateway_alive();
    log::info!("[get_health] local mode, gateway_alive={}", gateway_alive);

    let mut agents = Vec::new();
    let Ok(entries) = std::fs::read_dir(&agents_dir) else {
        return Err("read agents dir".into());
    };
    for entry in entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
        let agent_id = entry.file_name().to_string_lossy().to_string();
        let agent_dir = entry.path();
        let sessions_dir = agent_dir.join("sessions");
        let meta_path = sessions_dir.join("sessions.json");

        // Try to read sessions.json and build per-session health
        if meta_path.exists() {
            if let Ok(meta_str) = std::fs::read_to_string(&meta_path) {
                // Build tails map: basename → last 5 lines
                let mut tails: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
                if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                    for fe in rd.filter_map(|e| e.ok()) {
                        let p = fe.path();
                        if p.extension().map_or(true, |ext| ext != "jsonl") { continue; }
                        #[cfg(windows)]
                        let lines = tail_lines_from_file(&p, 5);
                        #[cfg(not(windows))]
                        let lines = {
                            let out = tokio::process::Command::new("tail")
                                .args(["-5", &p.to_string_lossy()])
                                .output().await.ok();
                            out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.to_string()).collect::<Vec<_>>())
                                .unwrap_or_default()
                        };
                        if !lines.is_empty() {
                            if let Some(fname) = p.file_name() {
                                tails.insert(fname.to_string_lossy().to_string(), lines);
                            }
                        }
                    }
                }
                let mut agent = build_agent_health_from_meta(&agent_id, &meta_str, &tails);
                // Gateway dead → all sessions are stale, force everything inactive
                if !gateway_alive {
                    agent.active = false;
                    for s in &mut agent.sessions { s.active = false; }
                }
                agents.push(agent);
                continue;
            }
        }

        // Fallback: no sessions.json, check most recent file only
        let latest = std::fs::read_dir(&sessions_dir).ok()
            .and_then(|rd| rd.filter_map(|e| e.ok())
                .filter(|e| e.path().extension().map_or(false, |ext| ext == "jsonl"))
                .max_by_key(|e| e.metadata().ok().and_then(|m| m.modified().ok())));
        let active = if let Some(f) = latest {
            #[cfg(windows)]
            let lines = tail_lines_from_file(&f.path(), 5);
            #[cfg(not(windows))]
            let lines = {
                let out = tokio::process::Command::new("tail")
                    .args(["-5", &f.path().to_string_lossy()])
                    .output().await.ok();
                out.map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.to_string()).collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            // Only trust JSONL content if gateway is still running
            gateway_alive && check_agent_active_from_lines(&lines)
        } else { false };
        agents.push(AgentHealth { agent_id, active, sessions: vec![] });
    }

    Ok(HealthResult { agents, gateway_alive })
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ToolCallStat {
    pub name: String,
    pub count: usize,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct RecentAction {
    /// "tool" or "text"
    #[serde(rename = "type")]
    pub action_type: String,
    /// tool name (for tool) or text snippet (for text)
    pub summary: String,
    pub detail: Option<String>,
    pub timestamp: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct AgentMetrics {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub active: bool,
    #[serde(rename = "currentModel")]
    pub current_model: Option<String>,
    #[serde(rename = "thinkingLevel")]
    pub thinking_level: Option<String>,
    #[serde(rename = "activeSessionCount")]
    pub active_session_count: usize,
    #[serde(rename = "currentTask")]
    pub current_task: Option<String>,
    #[serde(rename = "currentTool")]
    pub current_tool: Option<String>,
    #[serde(rename = "totalTokens")]
    pub total_tokens: u64,
    #[serde(rename = "inputTokens")]
    pub input_tokens: u64,
    #[serde(rename = "outputTokens")]
    pub output_tokens: u64,
    #[serde(rename = "cacheReadTokens")]
    pub cache_read_tokens: u64,
    #[serde(rename = "cacheWriteTokens")]
    pub cache_write_tokens: u64,
    #[serde(rename = "totalCost")]
    pub total_cost: f64,
    #[serde(rename = "toolCalls")]
    pub tool_calls: Vec<ToolCallStat>,
    #[serde(rename = "recentActions")]
    pub recent_actions: Vec<RecentAction>,
    #[serde(rename = "errorCount")]
    pub error_count: usize,
    #[serde(rename = "messageCount")]
    pub message_count: usize,
    #[serde(rename = "sessionStart")]
    pub session_start: Option<String>,
    #[serde(rename = "lastActivity")]
    pub last_activity: Option<String>,
    pub channel: Option<String>,
}

/// Extract the actual user message from openclaw's metadata-wrapped format.
/// Handles both direct messages and queued messages.
/// Formats:
///   - `Conversation info...\n[message_id: xxx]\nSender: actual message`
///   - `[Queued messages...]\n---\nQueued #N\n...\n[message_id: xxx]\nSender: msg\n---\nQueued #M\n...`
///   - `[timestamp] message` (simple format)
fn extract_user_message(text: &str) -> Option<String> {
    // For queued messages, extract the last queued message's content
    if text.starts_with("[Queued messages") {
        // Find the last "[message_id: ...]" line and take the line after it
        let mut last_msg: Option<String> = None;
        let lines: Vec<&str> = text.lines().collect();
        for (i, line) in lines.iter().enumerate() {
            if line.starts_with("[message_id:") {
                // Next line is "sender: actual message"
                if let Some(next) = lines.get(i + 1) {
                    // Strip "Sender: " prefix if present
                    let content = if let Some(pos) = next.find(": ") {
                        &next[pos + 2..]
                    } else {
                        next
                    };
                    if !content.trim().is_empty() {
                        last_msg = Some(content.trim().to_string());
                    }
                }
            }
        }
        return last_msg.map(|m| truncate_str(&m, 100));
    }

    // For regular messages with metadata wrapper
    let lines: Vec<&str> = text.lines().collect();
    for (i, line) in lines.iter().enumerate() {
        if line.starts_with("[message_id:") {
            // Next line is "sender: actual message"
            if let Some(next) = lines.get(i + 1) {
                let content = if let Some(pos) = next.find(": ") {
                    &next[pos + 2..]
                } else {
                    next
                };
                if !content.trim().is_empty() {
                    return Some(truncate_str(content.trim(), 100));
                }
            }
        }
    }

    // Simple format: "[timestamp] message"
    if text.starts_with('[') {
        if let Some(end) = text.find(']') {
            let after = text[end + 1..].trim();
            if !after.is_empty() {
                return Some(truncate_str(after, 100));
            }
        }
    }

    // Fallback: first non-empty line
    text.lines()
        .find(|l| !l.trim().is_empty())
        .map(|l| truncate_str(l.trim(), 100))
}

fn truncate_str(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        // Truncate at char boundary
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}...", &s[..end])
    }
}

#[tauri::command]
async fn get_agent_metrics(agent_id: String, mode: Option<String>, url: Option<String>, token: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<AgentMetrics, String> {
    log::info!("[get_agent_metrics] agent_id={} mode={:?} ssh_host={:?}", agent_id, mode, ssh_host);
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            let active = ssh_is_agent_active(sh, su, &agent_id).await;

            let mut metrics = AgentMetrics {
                agent_id: agent_id.clone(),
                active,
                current_model: None,
                thinking_level: None,
                active_session_count: 0,
                current_task: None,
                current_tool: None,
                total_tokens: 0,
                input_tokens: 0,
                output_tokens: 0,
                cache_read_tokens: 0,
                cache_write_tokens: 0,
                total_cost: 0.0,
                tool_calls: vec![],
                recent_actions: vec![],
                error_count: 0,
                message_count: 0,
                session_start: None,
                last_activity: None,
                channel: None,
            };

            let sess_path = remote_sessions_json_path(&agent_id);
            let sess_content = match ssh_read_file(sh, su, &sess_path).await {
                Ok(c) => c,
                Err(_) => return Ok(metrics),
            };
            let sess_map: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&sess_content).unwrap_or_default();

            metrics.active_session_count = sess_map.len();

            let best_entry = sess_map.values()
                .max_by_key(|v| v["updatedAt"].as_u64().unwrap_or(0));
            if let Some(entry) = best_entry {
                metrics.channel = entry["origin"]["surface"].as_str().map(|s| s.to_string());
                metrics.current_model = entry["model"].as_str().map(|s| s.to_string());
            }

            let mut best_session: Option<(String, u64)> = None;
            for val in sess_map.values() {
                if let (Some(file), Some(updated)) = (val["sessionFile"].as_str(), val["updatedAt"].as_u64()) {
                    if best_session.as_ref().map_or(true, |(_, t)| updated > *t) {
                        best_session = Some((file.to_string(), updated));
                    }
                }
            }

            let session_file = match best_session {
                Some((f, _)) => f,
                None => return Ok(metrics),
            };

            let content = match ssh_read_file(sh, su, &session_file).await {
                Ok(c) => { log::info!("[get_agent_metrics] SSH read session file OK, len={}", c.len()); c }
                Err(e) => { log::error!("[get_agent_metrics] SSH read session file failed: {}", e); return Ok(metrics); }
            };

            let mut tool_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
            let mut last_user_text: Option<String> = None;
            let mut last_tool_name: Option<String> = None;
            let mut last_timestamp: Option<String> = None;
            let mut recent_actions: Vec<RecentAction> = vec![];
            let mut current_msg_timestamp: Option<String> = None;

            for line in content.lines() {
                let val: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                let event_type = val["type"].as_str().unwrap_or("");
                if let Some(ts) = val["timestamp"].as_str() {
                    last_timestamp = Some(ts.to_string());
                }
                match event_type {
                    "session" => { metrics.session_start = val["timestamp"].as_str().map(|s| s.to_string()); }
                    "model_change" => { metrics.current_model = val["modelId"].as_str().map(|s| s.to_string()); }
                    "thinking_level_change" => { metrics.thinking_level = val["thinkingLevel"].as_str().map(|s| s.to_string()); }
                    "message" => {
                        let msg = &val["message"];
                        let role = msg["role"].as_str().unwrap_or("");
                        current_msg_timestamp = val["timestamp"].as_str().map(|s| s.to_string());
                        if role == "user" {
                            if let Some(content_arr) = msg["content"].as_array() {
                                for item in content_arr {
                                    if item["type"].as_str() == Some("text") {
                                        if let Some(text) = item["text"].as_str() {
                                            last_user_text = extract_user_message(text);
                                        }
                                    }
                                }
                            }
                            metrics.message_count += 1;
                        } else if role == "assistant" {
                            if let Some(usage) = msg["usage"].as_object() {
                                metrics.input_tokens += usage.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
                                metrics.output_tokens += usage.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
                                metrics.cache_read_tokens += usage.get("cacheRead").and_then(|v| v.as_u64()).unwrap_or(0);
                                metrics.cache_write_tokens += usage.get("cacheWrite").and_then(|v| v.as_u64()).unwrap_or(0);
                                metrics.total_tokens += usage.get("totalTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                                if let Some(cost) = usage.get("cost").and_then(|c| c["total"].as_f64()) {
                                    metrics.total_cost += cost;
                                }
                            }
                            if let Some(content_arr) = msg["content"].as_array() {
                                for item in content_arr {
                                    match item["type"].as_str() {
                                        Some("toolCall") => {
                                            if let Some(name) = item["name"].as_str() {
                                                *tool_counts.entry(name.to_string()).or_insert(0) += 1;
                                                last_tool_name = Some(name.to_string());
                                                let detail = item["input"].as_object().map(|obj| {
                                                    obj.iter().map(|(k, v)| {
                                                        let val_str = match v.as_str() {
                                                            Some(s) => truncate_str(s, 300),
                                                            None => { let j = v.to_string(); truncate_str(&j, 100) }
                                                        };
                                                        format!("{}: {}", k, val_str)
                                                    }).collect::<Vec<_>>().join("\n")
                                                }).filter(|s| !s.is_empty());
                                                recent_actions.push(RecentAction {
                                                    action_type: "tool".to_string(),
                                                    summary: name.to_string(),
                                                    detail,
                                                    timestamp: current_msg_timestamp.clone(),
                                                });
                                            }
                                        }
                                        Some("text") => {
                                            if let Some(text) = item["text"].as_str() {
                                                let trimmed = text.trim();
                                                if !trimmed.is_empty() {
                                                    let summary = truncate_str(trimmed, 60);
                                                    let detail = if trimmed.len() > 60 {
                                                        Some(truncate_str(trimmed, 500))
                                                    } else { None };
                                                    recent_actions.push(RecentAction {
                                                        action_type: "text".to_string(),
                                                        summary,
                                                        detail,
                                                        timestamp: current_msg_timestamp.clone(),
                                                    });
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                            metrics.message_count += 1;
                        }
                    }
                    "custom" => {
                        if val["customType"].as_str().map_or(false, |t| t.contains("error")) {
                            metrics.error_count += 1;
                        }
                    }
                    _ => {}
                }
            }

            metrics.current_task = last_user_text;
            metrics.current_tool = last_tool_name;
            metrics.last_activity = last_timestamp;

            let len = recent_actions.len();
            if len > 3 { metrics.recent_actions = recent_actions[len - 3..].to_vec(); }
            else { metrics.recent_actions = recent_actions; }
            metrics.recent_actions.reverse();

            let mut tool_vec: Vec<ToolCallStat> = tool_counts.into_iter()
                .map(|(name, count)| ToolCallStat { name, count }).collect();
            tool_vec.sort_by(|a, b| b.count.cmp(&a.count));
            metrics.tool_calls = tool_vec;

            log::info!("[get_agent_metrics] SSH result: active={} recent_actions={} tool_calls={} message_count={} current_task={:?}",
                metrics.active, metrics.recent_actions.len(), metrics.tool_calls.len(), metrics.message_count, metrics.current_task);
            return Ok(metrics);
        }
        // Gateway API fallback
        let url = url.as_deref().unwrap_or("");
        let tok = token.as_deref().unwrap_or("");
        let result = invoke_tool(url, tok, "sessions_list", serde_json::json!({"agentId": agent_id, "activeMinutes": 60})).await?;
        let sessions = extract_sessions(&result);
        let active_count = sessions.iter().filter(|s| is_session_active(s)).count();
        let total_tokens: u64 = sessions.iter().map(|s| s["totalTokens"].as_u64().unwrap_or(0)).sum();
        let model = sessions.iter().find_map(|s| s["model"].as_str().map(|s| s.to_string()));
        let channel = sessions.iter().find_map(|s| s["channel"].as_str().map(|s| s.to_string()));
        let last_updated = sessions.iter().filter_map(|s| s["updatedAt"].as_u64()).max();
        let last_activity = last_updated.map(|ms| {
            let secs = (ms / 1000) as i64;
            chrono::DateTime::from_timestamp(secs, 0)
                .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                .unwrap_or_default()
        });
        let mut input_tokens: u64 = 0;
        let mut output_tokens: u64 = 0;
        let mut current_task: Option<String> = None;
        let default_key = format!("agent:{}:main", agent_id);
        let session_key = sessions.first()
            .and_then(|s| s["key"].as_str())
            .unwrap_or(&default_key);
        if let Ok(status_result) = invoke_tool(url, tok, "session_status", serde_json::json!({"sessionKey": session_key})).await {
            let sr = status_result.get("result").unwrap_or(&status_result);
            let det = sr.get("details").unwrap_or(sr);
            if let Some(text) = det["statusText"].as_str() {
                for line in text.lines() {
                    if line.contains("Tokens:") {
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        for (i, p) in parts.iter().enumerate() {
                            if *p == "in" && i > 0 {
                                input_tokens = parts[i-1].replace(",", "").replace("k", "000").parse().unwrap_or(0);
                            }
                            if *p == "out" && i > 0 {
                                output_tokens = parts[i-1].replace(",", "").replace("k", "000").parse().unwrap_or(0);
                            }
                        }
                    }
                    if line.contains("Queue:") {
                        let queue_part = line.split("Queue:").nth(1).unwrap_or("").trim();
                        if queue_part.starts_with("running") || queue_part.starts_with("thinking") || queue_part.starts_with("streaming") {
                            current_task = Some(queue_part.to_string());
                        }
                    }
                }
            }
        }
        let metrics = AgentMetrics {
            agent_id: agent_id.clone(),
            active: active_count > 0,
            current_model: model,
            thinking_level: None,
            active_session_count: active_count,
            current_task,
            current_tool: None,
            total_tokens,
            input_tokens,
            output_tokens,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            total_cost: 0.0,
            tool_calls: vec![],
            recent_actions: vec![],
            error_count: 0,
            message_count: sessions.len(),
            session_start: None,
            last_activity,
            channel,
        };
        return Ok(metrics);
    }

    // === local mode (original) ===
    let active_set = lsof_active_agents().await;
    let agent_dir = if agent_id.is_empty() { "main" } else { &agent_id };
    let active = active_set.contains(agent_dir);

    let mut metrics = AgentMetrics {
        agent_id: agent_id.clone(),
        active,
        current_model: None,
        thinking_level: None,
        active_session_count: 0,
        current_task: None,
        current_tool: None,
        total_tokens: 0,
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        total_cost: 0.0,
        tool_calls: vec![],
        recent_actions: vec![],
        error_count: 0,
        message_count: 0,
        session_start: None,
        last_activity: None,
        channel: None,
    };

    // Read sessions.json to find active sessions
    let sess_path = sessions_json_path(&agent_id);
    let sess_map: serde_json::Map<String, serde_json::Value> = match tokio::fs::read_to_string(&sess_path).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => return Ok(metrics),
    };

    metrics.active_session_count = sess_map.len();

    // Get model + channel from most recently updated session in sessions.json
    let best_entry = sess_map.values()
        .max_by_key(|v| v["updatedAt"].as_u64().unwrap_or(0));
    if let Some(entry) = best_entry {
        metrics.channel = entry["origin"]["surface"].as_str().map(|s| s.to_string());
        // Model is stored directly in sessions.json
        if metrics.current_model.is_none() {
            metrics.current_model = entry["model"].as_str().map(|s| s.to_string());
        }
    }

    // Find the most recently updated session file
    let mut best_session: Option<(String, u64)> = None;
    for val in sess_map.values() {
        if let (Some(file), Some(updated)) = (
            val["sessionFile"].as_str(),
            val["updatedAt"].as_u64(),
        ) {
            if best_session.as_ref().map_or(true, |(_, t)| updated > *t) {
                best_session = Some((file.to_string(), updated));
            }
        }
    }

    let session_file = match best_session {
        Some((f, _)) => f,
        None => return Ok(metrics),
    };

    // Parse the .jsonl file
    let content = match tokio::fs::read_to_string(&session_file).await {
        Ok(c) => c,
        Err(_) => return Ok(metrics),
    };

    let mut tool_counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut last_user_text: Option<String> = None;
    let mut last_tool_name: Option<String> = None;
    let mut last_timestamp: Option<String> = None;
    let mut recent_actions: Vec<RecentAction> = vec![];
    let mut current_msg_timestamp: Option<String> = None;

    for line in content.lines() {
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let event_type = val["type"].as_str().unwrap_or("");
        if let Some(ts) = val["timestamp"].as_str() {
            last_timestamp = Some(ts.to_string());
        }

        match event_type {
            "session" => {
                metrics.session_start = val["timestamp"].as_str().map(|s| s.to_string());
            }
            "model_change" => {
                metrics.current_model = val["modelId"].as_str().map(|s| s.to_string());
            }
            "thinking_level_change" => {
                metrics.thinking_level = val["thinkingLevel"].as_str().map(|s| s.to_string());
            }
            "message" => {
                let msg = &val["message"];
                let role = msg["role"].as_str().unwrap_or("");
                current_msg_timestamp = val["timestamp"].as_str().map(|s| s.to_string());

                if role == "user" {
                    if let Some(content_arr) = msg["content"].as_array() {
                        for item in content_arr {
                            if item["type"].as_str() == Some("text") {
                                if let Some(text) = item["text"].as_str() {
                                    last_user_text = extract_user_message(text);
                                }
                            }
                        }
                    }
                    metrics.message_count += 1;
                } else if role == "assistant" {
                    // Extract usage
                    if let Some(usage) = msg["usage"].as_object() {
                        metrics.input_tokens += usage.get("input").and_then(|v| v.as_u64()).unwrap_or(0);
                        metrics.output_tokens += usage.get("output").and_then(|v| v.as_u64()).unwrap_or(0);
                        metrics.cache_read_tokens += usage.get("cacheRead").and_then(|v| v.as_u64()).unwrap_or(0);
                        metrics.cache_write_tokens += usage.get("cacheWrite").and_then(|v| v.as_u64()).unwrap_or(0);
                        metrics.total_tokens += usage.get("totalTokens").and_then(|v| v.as_u64()).unwrap_or(0);
                        if let Some(cost) = usage.get("cost").and_then(|c| c["total"].as_f64()) {
                            metrics.total_cost += cost;
                        }
                    }

                    // Extract tool calls and text actions
                    if let Some(content_arr) = msg["content"].as_array() {
                        for item in content_arr {
                            match item["type"].as_str() {
                                Some("toolCall") => {
                                    if let Some(name) = item["name"].as_str() {
                                        *tool_counts.entry(name.to_string()).or_insert(0) += 1;
                                        last_tool_name = Some(name.to_string());

                                        let detail = item["input"].as_object().map(|obj| {
                                            let mut parts: Vec<String> = vec![];
                                            for (k, v) in obj.iter() {
                                                let val_str = match v.as_str() {
                                                    Some(s) => {
                                                        if s.len() > 300 {
                                                            let mut end = 300;
                                                            while end > 0 && !s.is_char_boundary(end) { end -= 1; }
                                                            format!("{}...", &s[..end])
                                                        } else { s.to_string() }
                                                    }
                                                    None => {
                                                        let j = v.to_string();
                                                        if j.len() > 100 {
                                                            let mut end = 100;
                                                            while end > 0 && !j.is_char_boundary(end) { end -= 1; }
                                                            format!("{}...", &j[..end])
                                                        } else { j }
                                                    }
                                                };
                                                parts.push(format!("{}: {}", k, val_str));
                                            }
                                            parts.join("\n")
                                        }).filter(|s| !s.is_empty());
                                        recent_actions.push(RecentAction {
                                            action_type: "tool".to_string(),
                                            summary: name.to_string(),
                                            detail,
                                            timestamp: current_msg_timestamp.clone(),
                                        });
                                    }
                                }
                                Some("text") => {
                                    if let Some(text) = item["text"].as_str() {
                                        let trimmed = text.trim();
                                        if !trimmed.is_empty() {
                                            let summary = if trimmed.len() > 60 {
                                                let mut end = 60;
                                                while end > 0 && !trimmed.is_char_boundary(end) { end -= 1; }
                                                format!("{}...", &trimmed[..end])
                                            } else { trimmed.to_string() };
                                            let detail = if trimmed.len() > 60 {
                                                let full = if trimmed.len() > 500 {
                                                    let mut end = 500;
                                                    while end > 0 && !trimmed.is_char_boundary(end) { end -= 1; }
                                                    format!("{}...", &trimmed[..end])
                                                } else { trimmed.to_string() };
                                                Some(full)
                                            } else { None };
                                            recent_actions.push(RecentAction {
                                                action_type: "text".to_string(),
                                                summary,
                                                detail,
                                                timestamp: current_msg_timestamp.clone(),
                                            });
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }

                    metrics.message_count += 1;
                }
            }
            "custom" => {
                if val["customType"].as_str().map_or(false, |t| t.contains("error")) {
                    metrics.error_count += 1;
                }
            }
            _ => {}
        }
    }

    metrics.current_task = last_user_text;
    metrics.current_tool = last_tool_name;
    metrics.last_activity = last_timestamp;

    // Keep only the last 3 actions (most recent first)
    let len = recent_actions.len();
    if len > 3 {
        metrics.recent_actions = recent_actions[len - 3..].to_vec();
    } else {
        metrics.recent_actions = recent_actions;
    }
    metrics.recent_actions.reverse();

    // Sort tool calls by count desc
    let mut tool_vec: Vec<ToolCallStat> = tool_counts
        .into_iter()
        .map(|(name, count)| ToolCallStat { name, count })
        .collect();
    tool_vec.sort_by(|a, b| b.count.cmp(&a.count));
    metrics.tool_calls = tool_vec;

    Ok(metrics)
}

#[tauri::command]
async fn interrupt_agent(agent_id: String, state: tauri::State<'_, ActiveAgentPid>) -> Result<String, String> {
    // Strategy 1: Send interrupt signal to the tracked openclaw agent subprocess (pet-window turns)
    let tracked_pid = *state.pid.lock().unwrap();
    if let Some(pid) = tracked_pid {
        #[cfg(unix)]
        let killed = unsafe { libc::kill(pid as i32, libc::SIGINT) == 0 };
        #[cfg(windows)]
        let killed = {
            // On Windows, use GenerateConsoleCtrlEvent to send Ctrl+C to the process group,
            // or TerminateProcess as a fallback.
            use windows::Win32::System::Console::GenerateConsoleCtrlEvent;
            use windows::Win32::System::Console::CTRL_BREAK_EVENT;
            unsafe { GenerateConsoleCtrlEvent(CTRL_BREAK_EVENT, pid).is_ok() }
        };
        if killed {
            return Ok(format!("已向 openclaw agent 进程 (pid={}) 发送中断信号", pid));
        }
    }

    // Strategy 2: WebSocket chat.abort (channel-based turns like Feishu/Telegram)
    let home = home_dir_string();

    // 1. Read gateway config
    let config_path = PathBuf::from(&home).join(".openclaw").join("openclaw.json");
    let config_str = tokio::fs::read_to_string(&config_path).await
        .map_err(|e| format!("读取 openclaw.json 失败: {}", e))?;
    let config: serde_json::Value = serde_json::from_str(&config_str)
        .map_err(|e| format!("解析 openclaw.json 失败: {}", e))?;
    let port = config["gateway"]["port"].as_u64().unwrap_or(18789) as u16;
    let token = config["gateway"]["auth"]["token"].as_str().unwrap_or("").to_string();
    if token.is_empty() {
        return Err("openclaw.json 中未找到 gateway token".into());
    }

    // 2. Find the ACTIVE session key.
    //    On macOS/Linux: use lsof to find which .jsonl file is currently held open.
    //    On Windows: use recently modified .jsonl files as a heuristic.
    let sess_path = sessions_json_path(&agent_id);
    let content = tokio::fs::read_to_string(&sess_path).await
        .map_err(|e| format!("读取 sessions.json 失败: {}", e))?;
    let sess_map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    // Get the set of currently active .jsonl file paths
    let open_jsonl_paths = lsof_open_jsonl_paths().await;

    // Match open/active .jsonl paths against sessionFile entries in sessions.json
    let session_key = sess_map.iter()
        .find(|(_, v)| {
            if let Some(sf) = v["sessionFile"].as_str() {
                // sessionFile may be exact path or may contain the uuid; check if any open path starts with or equals it
                open_jsonl_paths.iter().any(|p| {
                    p.starts_with(sf) || sf.starts_with(p.as_str())
                    // On Windows, also compare with backslash-normalized paths
                    || p.replace('\\', "/").starts_with(&sf.replace('\\', "/"))
                    || sf.replace('\\', "/").starts_with(&p.replace('\\', "/"))
                })
            } else {
                false
            }
        })
        .map(|(k, _)| k.clone())
        // Fallback: most recently updated session
        .or_else(|| {
            sess_map.iter()
                .max_by_key(|(_, v)| v["updatedAt"].as_u64().unwrap_or(0))
                .map(|(k, _)| k.clone())
        })
        .ok_or("没有找到活跃 session")?;

    // 3. WebSocket: wait for challenge → send connect → send chat.abort
    let script = format!(
        r#"const ws=new WebSocket('ws://127.0.0.1:{port}/');const t=setTimeout(()=>{{process.stderr.write('timeout');process.exit(1)}},6000);let ok=false;ws.onmessage=(e)=>{{const d=JSON.parse(e.data);if(d.event==='connect.challenge'){{ws.send(JSON.stringify({{type:'req',id:'c',method:'connect',params:{{auth:{{token:'{token}'}},minProtocol:3,maxProtocol:3,client:{{id:'gateway-client',platform:'darwin',mode:'backend',version:'0.1.0'}},role:'operator',scopes:['operator.admin'],caps:[]}}}}))}}else if(d.id==='c'&&d.ok&&!ok){{ok=true;ws.send(JSON.stringify({{type:'req',id:'a',method:'chat.abort',params:{{sessionKey:'{sk}',stopReason:'user'}}}}))}}else if(d.id==='c'&&!d.ok){{process.stderr.write(d.error?.message||'connect failed');clearTimeout(t);ws.close();process.exit(1)}}else if(d.id==='a'){{process.stdout.write(JSON.stringify(d.payload||d));clearTimeout(t);ws.close();process.exit(0)}}}};ws.onerror=(e)=>{{process.stderr.write(e.message||'ws error');process.exit(1)}};"#,
        port = port,
        token = token,
        sk = session_key,
    );

    let output = tokio::process::Command::new("node")
        .args(["-e", &script])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("node: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("打断失败: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let aborted = stdout.contains("\"aborted\":true");
    if aborted {
        Ok(format!("已打断 ({})", session_key))
    } else {
        Ok(format!("指令已发送，当前无活跃 run ({})", session_key))
    }
}


#[derive(Debug, Serialize, Deserialize)]
struct DailyCount {
    date: String,
    count: u32,
    tokens: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct AgentExtraInfo {
    skills: Vec<String>,
    cron_jobs: Vec<serde_json::Value>,
    daily_counts: Vec<DailyCount>,
}

#[tauri::command]
async fn get_agent_extra_info(agent_id: String, mode: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<AgentExtraInfo, String> {
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            let agent_dir = if agent_id.is_empty() { "main" } else { &agent_id };

            // Skills from remote sessions.json
            let sess_path = remote_sessions_json_path(&agent_id);
            let skills: Vec<String> = if let Ok(content) = ssh_read_file(sh, su, &sess_path).await {
                serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&content)
                    .ok()
                    .and_then(|map| {
                        map.into_values()
                            .max_by_key(|v| v["updatedAt"].as_u64().unwrap_or(0))
                            .and_then(|v| v["skillsSnapshot"]["skills"].as_array().cloned())
                            .map(|arr| arr.iter()
                                .filter_map(|s| s["name"].as_str().map(|n| n.to_string()))
                                .collect())
                    })
                    .unwrap_or_default()
            } else { vec![] };

            // Daily counts from remote .jsonl files
            // Use find+exec to avoid ARG_MAX with many files, and process server-side
            // to minimise SSH data transfer.
            let mut daily_calls: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
            let mut daily_tokens: std::collections::HashMap<String, u64> = std::collections::HashMap::new();

            // Server-side: extract "date calls tokens" summary per day using awk
            // Also output server's "today" to avoid timezone mismatch with local machine
            let summary_cmd = format!(
                concat!(
                    "find ~/.openclaw/agents/{}/sessions -name '*.jsonl' -exec cat {{}} + 2>/dev/null | ",
                    "awk '{{ ",
                    "  if (match($0, /\"timestamp\":\"([0-9]{{4}}-[0-9]{{2}}-[0-9]{{2}})/, a)) {{ d=a[1]; c[d]++ }} ",
                    "  if (match($0, /\"totalTokens\":([0-9]+)/, b) && d) t[d]+=b[1] ",
                    "}} END {{ for (d in c) print d, c[d], t[d]+0 }}' && echo \"SERVER_TODAY:$(date +%Y-%m-%d)\""
                ),
                agent_dir
            );
            log::info!("[get_agent_extra_info] running daily summary cmd for agent={}", agent_dir);
            let mut server_today: Option<String> = None;
            match ssh_exec(sh, su, &summary_cmd).await {
                Ok(summary) => {
                    for line in summary.lines() {
                        if let Some(date_str) = line.strip_prefix("SERVER_TODAY:") {
                            server_today = Some(date_str.trim().to_string());
                            continue;
                        }
                        let parts: Vec<&str> = line.split_whitespace().collect();
                        if parts.len() >= 3 {
                            let date = parts[0].to_string();
                            let calls: u32 = parts[1].parse().unwrap_or(0);
                            let tokens: u64 = parts[2].parse().unwrap_or(0);
                            daily_calls.insert(date.clone(), calls);
                            daily_tokens.insert(date, tokens);
                        }
                    }
                    log::info!("[get_agent_extra_info] parsed {} daily entries, server_today={:?}", daily_calls.len(), server_today);
                }
                Err(e) => {
                    log::warn!("[get_agent_extra_info] daily summary cmd failed: {}, trying fallback", e);
                    // Fallback: cat with find (no glob), limited output
                    let cat_cmd = format!(
                        "find ~/.openclaw/agents/{}/sessions -name '*.jsonl' -exec cat {{}} + 2>/dev/null | tail -n 30000",
                        agent_dir
                    );
                    if let Ok(content) = ssh_exec(sh, su, &cat_cmd).await {
                        let mut current_date: Option<String> = None;
                        for line in content.lines() {
                            if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) {
                                if let Some(ts) = obj["timestamp"].as_str() {
                                    if ts.len() >= 10 {
                                        current_date = Some(ts[..10].to_string());
                                        *daily_calls.entry(ts[..10].to_string()).or_insert(0) += 1;
                                    }
                                }
                                if obj["type"].as_str() == Some("message") {
                                    if let Some(total) = obj["message"]["usage"]["totalTokens"].as_u64() {
                                        if let Some(ref date) = current_date {
                                            *daily_tokens.entry(date.clone()).or_insert(0) += total;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            use chrono::{Local, NaiveDate, Duration};
            let today = server_today
                .as_deref()
                .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
                .unwrap_or_else(|| Local::now().date_naive());
            let daily_counts: Vec<DailyCount> = (0..14i64).rev().map(|i| {
                let date = (today - Duration::days(i)).format("%Y-%m-%d").to_string();
                let count = daily_calls.get(&date).copied().unwrap_or(0);
                let tokens = daily_tokens.get(&date).copied().unwrap_or(0);
                DailyCount { date, count, tokens }
            }).collect();

            return Ok(AgentExtraInfo { skills, cron_jobs: vec![], daily_counts });
        }
        return Ok(AgentExtraInfo { skills: vec![], cron_jobs: vec![], daily_counts: vec![] });
    }

    let home = home_dir_string();
    let agent_dir = if agent_id.is_empty() { "main" } else { &agent_id };

    // 1. Skills from sessions.json (most recently updated session)
    let skills: Vec<String> = if let Ok(content) = tokio::fs::read_to_string(sessions_json_path(&agent_id)).await {
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&content)
            .ok()
            .and_then(|map| {
                map.into_values()
                    .max_by_key(|v| v["updatedAt"].as_u64().unwrap_or(0))
                    .and_then(|v| v["skillsSnapshot"]["skills"].as_array().cloned())
                    .map(|arr| arr.iter()
                        .filter_map(|s| s["name"].as_str().map(|n| n.to_string()))
                        .collect())
            })
            .unwrap_or_default()
    } else { vec![] };

    // 2. Cron jobs filtered by agent
    let cron_jobs: Vec<serde_json::Value> = tokio::process::Command::new("openclaw")
        .args(["cron", "list", "--json"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output().await.ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| { let i = s.find('{')?; serde_json::from_str::<serde_json::Value>(&s[i..]).ok() })
        .and_then(|v| v["jobs"].as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter(|j| {
            let job_agent = j["agentId"].as_str().unwrap_or("main");
            let target = if agent_id.is_empty() { "main" } else { &agent_id };
            job_agent == target || (target == "main" && job_agent.is_empty())
        })
        .collect();

    // 3. Daily call counts + token usage — last 14 days from .jsonl files
    let sessions_dir = format!("{}/.openclaw/agents/{}/sessions", home, agent_dir);
    let mut daily_calls: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut daily_tokens: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
    if let Ok(mut dir) = tokio::fs::read_dir(&sessions_dir).await {
        while let Ok(Some(entry)) = dir.next_entry().await {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
            if let Ok(content) = tokio::fs::read_to_string(&path).await {
                let mut current_date: Option<String> = None;
                for line in content.lines() {
                    if let Ok(obj) = serde_json::from_str::<serde_json::Value>(line) {
                        if let Some(ts) = obj["timestamp"].as_str() {
                            if ts.len() >= 10 {
                                current_date = Some(ts[..10].to_string());
                                *daily_calls.entry(ts[..10].to_string()).or_insert(0) += 1;
                            }
                        }
                        // Accumulate tokens from assistant message usage
                        if obj["type"].as_str() == Some("message") {
                            if let Some(total) = obj["message"]["usage"]["totalTokens"].as_u64() {
                                if let Some(ref date) = current_date {
                                    *daily_tokens.entry(date.clone()).or_insert(0) += total;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    use chrono::{Local, Duration};
    let today = Local::now().date_naive();
    let daily_counts: Vec<DailyCount> = (0..14i64).rev().map(|i| {
        let date = (today - Duration::days(i)).format("%Y-%m-%d").to_string();
        let count = daily_calls.get(&date).copied().unwrap_or(0);
        let tokens = daily_tokens.get(&date).copied().unwrap_or(0);
        DailyCount { date, count, tokens }
    }).collect();

    Ok(AgentExtraInfo { skills, cron_jobs, daily_counts })
}

#[tauri::command]
async fn open_mini(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("mini") {
        // Reposition to collapsed position before showing
        #[cfg(target_os = "macos")]
        {
            let win_clone = win.clone();
            let _ = app.run_on_main_thread(move || {
                use objc2::runtime::{AnyClass, AnyObject};
                use objc2::msg_send;
                use objc2_foundation::{NSRect, NSPoint, NSSize};

                if let Ok(ns_win) = win_clone.ns_window() {
                    let obj = unsafe { &*(ns_win as *mut AnyObject) };
                    unsafe {
                        let _: () = msg_send![obj, setLevel: 27isize];
                        let behavior: usize = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 6);
                        let _: () = msg_send![obj, setCollectionBehavior: behavior];
                    }
                    let screen_info: Option<(f64, f64, f64, f64, f64)> = unsafe {
                        let cls = match AnyClass::get(c"NSScreen") {
                            Some(c) => c,
                            None => return,
                        };
                        let screens: *mut AnyObject = msg_send![cls, screens];
                        if screens.is_null() { return; }
                        let count: usize = msg_send![&*screens, count];
                        if count == 0 { return; }
                        let screen: *mut AnyObject = msg_send![&*screens, objectAtIndex: 0usize];
                        if screen.is_null() { return; }
                        let frame: NSRect = msg_send![&*screen, frame];
                        let notch_off = get_notch_offset(screen);
                        Some((frame.origin.x, frame.origin.y, frame.size.width, frame.size.height, notch_off))
                    };
                    if let Some((sx, sy, sw, sh, notch_off)) = screen_info {
                        let (win_w, win_h) = MINI_WINDOW_FRAME
                            .lock()
                            .ok()
                            .and_then(|g| *g)
                            .map(|(_, _, w, h)| (w, h))
                            .unwrap_or_else(|| collapsed_mascot_window_size(1.0));
                        let x = sx + sw / 2.0 + notch_off;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setFrame: frame, display: true];
                            let _: () = msg_send![obj, orderFrontRegardless];
                        }
                    }
                }
            });
        }
        #[cfg(target_os = "windows")]
        {
            // Reposition to top-center (simulating macOS notch position), DPI-aware
            if let Ok(Some(monitor)) = win.primary_monitor() {
                let scale = monitor.scale_factor();
                let sw = monitor.size().width as f64 / scale;
                let ui = win_ui_scale(&monitor);
                let (base_w, base_h) = MINI_WINDOW_FRAME
                    .lock()
                    .ok()
                    .and_then(|g| *g)
                    .map(|(_, _, w, h)| (w, h))
                    .unwrap_or_else(|| collapsed_mascot_window_size(1.0));
                let win_w = (base_w * ui).round();
                let win_h = (base_h * ui).round();
                let notch_off = (80.0 * ui).round();
                let x = sw / 2.0 + notch_off;
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
            }
            if !FULLSCREEN_HIDING.load(std::sync::atomic::Ordering::SeqCst) {
                win.show().map_err(|e| e.to_string())?;
                win.set_focus().map_err(|e| e.to_string())?;
            }
        }
        return Ok(());
    }

    let builder = WebviewWindowBuilder::new(&app, "mini", WebviewUrl::App("index.html#/mini".into()))
        .title("oc-claw Mini")
        .inner_size(COLLAPSED_MASCOT_BASE_W, COLLAPSED_MASCOT_BASE_H)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(false)
        .accept_first_mouse(true); // single click from any app

    let win = builder.build().map_err(|e| e.to_string())?;

    // Use macOS native API to position at menu bar level (like notchi)
    // Must run on main thread for AppKit calls
    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        let _ = app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};

            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };

                unsafe {
                    let _: () = msg_send![obj, setLevel: 27isize];
                    let behavior: usize = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 6);
                    let _: () = msg_send![obj, setCollectionBehavior: behavior];
                }

                let screen_info: Option<(f64, f64, f64, f64, f64)> = unsafe {
                    let cls = match AnyClass::get(c"NSScreen") {
                        Some(c) => c,
                        None => return,
                    };
                    let screens: *mut AnyObject = msg_send![cls, screens];
                    if screens.is_null() { return; }
                    let count: usize = msg_send![&*screens, count];
                    if count == 0 { return; }
                    let screen: *mut AnyObject = msg_send![&*screens, objectAtIndex: 0usize];
                    if screen.is_null() { return; }
                    let frame: NSRect = msg_send![&*screen, frame];
                    let notch_off = get_notch_offset(screen);
                    Some((frame.origin.x, frame.origin.y, frame.size.width, frame.size.height, notch_off))
                };

                if let Some((sx, sy, sw, sh, notch_off)) = screen_info {
                    let (win_w, win_h) = collapsed_mascot_window_size(1.0);
                    let x = sx + sw / 2.0 + notch_off;
                    let y = sy + sh - win_h;
                    let frame = NSRect::new(
                        NSPoint::new(x, y),
                        NSSize::new(win_w, win_h),
                    );
                    unsafe {
                        let _: () = msg_send![obj, setFrame: frame, display: true];
                    }
                }

                unsafe {
                    let _: () = msg_send![obj, orderFrontRegardless];
                }
            }
        });
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows: position at top-center (simulating macOS notch position), DPI-aware
        if let Ok(Some(monitor)) = win.primary_monitor() {
            let scale = monitor.scale_factor();
            let sw = monitor.size().width as f64 / scale;
            let ui = win_ui_scale(&monitor);
            let (base_w, base_h) = collapsed_mascot_window_size(1.0);
            let win_w = (base_w * ui).round();
            let win_h = (base_h * ui).round();
            let notch_off = (80.0 * ui).round();
            let x = sw / 2.0 + notch_off;
            let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
            let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
        }
        if !FULLSCREEN_HIDING.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = win.show();
        }
    }

    Ok(())
}

#[tauri::command]
async fn close_mini(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("mini") {
        win.close().map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Compute collapsed mascot x position based on side preference.
fn collapsed_x(sx: f64, sw: f64, win_w: f64, position: &str, notch_offset: f64) -> f64 {
    if position == "left" {
        sx + sw / 2.0 - notch_offset - win_w
    } else {
        sx + sw / 2.0 + notch_offset
    }
}

const COLLAPSED_MASCOT_BASE_W: f64 = 60.0;
const COLLAPSED_MASCOT_BASE_H: f64 = 45.0;
const MASCOT_SCALE_MIN: f64 = 1.0;
const MASCOT_SCALE_MAX: f64 = 3.0;
const LARGE_MASCOT_SIZE_MULTIPLIER: f64 = 3.0;

fn sanitized_mascot_scale(scale: Option<f64>) -> f64 {
    let scale = scale.unwrap_or(1.0);
    if !scale.is_finite() {
        return 1.0;
    }
    scale.max(MASCOT_SCALE_MIN).min(MASCOT_SCALE_MAX)
}

fn collapsed_mascot_window_size(scale: f64) -> (f64, f64) {
    (COLLAPSED_MASCOT_BASE_W * scale, COLLAPSED_MASCOT_BASE_H * scale)
}

fn large_collapsed_mascot_window_size(scale: f64, large_scale: f64) -> (f64, f64) {
    let lms = if large_scale.is_finite() && large_scale >= 1.0 && large_scale <= 6.0 { large_scale } else { LARGE_MASCOT_SIZE_MULTIPLIER };
    let size = 43.0 * scale * lms;
    (size, size)
}

/// Compute a UI-scale multiplier for Windows based on the monitor's logical
/// resolution. Baseline is 1080 logical height (a typical 1080p or 4K@200%
/// display). On a 4K@150% display the logical height is 1440, giving
/// multiplier ≈ 1.33 so all window dimensions grow proportionally.
/// On macOS this is not needed — the system handles points uniformly.
#[cfg(target_os = "windows")]
fn win_ui_scale(monitor: &tauri::Monitor) -> f64 {
    let scale = monitor.scale_factor();
    let logical_h = monitor.size().height as f64 / scale;
    (logical_h / 1080.0).max(1.0)
}

/// Returns the HMONITOR of the fullscreen foreground window, or None if the
/// foreground window is not fullscreen.  Excludes desktop shell windows
/// (Progman, WorkerW, Shell_TrayWnd) which cover the full screen but are
/// not real fullscreen apps.
#[cfg(target_os = "windows")]
fn fullscreen_foreground_monitor() -> Option<windows::Win32::Graphics::Gdi::HMONITOR> {
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowRect, GetClassNameW,
    };
    use windows::Win32::Graphics::Gdi::{
        MonitorFromWindow, GetMonitorInfoW, MONITORINFO, MONITOR_DEFAULTTONEAREST,
    };
    use windows::Win32::Foundation::RECT;
    unsafe {
        let fg = GetForegroundWindow();
        if fg.0 == std::ptr::null_mut() {
            return None;
        }

        let mut class_buf = [0u16; 64];
        let len = GetClassNameW(fg, &mut class_buf) as usize;
        if len > 0 {
            let class_name = String::from_utf16_lossy(&class_buf[..len]);
            if class_name == "Progman"
                || class_name == "WorkerW"
                || class_name == "Shell_TrayWnd"
            {
                return None;
            }
        }

        let mut fg_rect = RECT::default();
        if GetWindowRect(fg, &mut fg_rect).is_err() {
            return None;
        }
        let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
        let mut mi = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if !GetMonitorInfoW(monitor, &mut mi).as_bool() {
            return None;
        }
        let mr = mi.rcMonitor;
        if fg_rect.left <= mr.left
            && fg_rect.top <= mr.top
            && fg_rect.right >= mr.right
            && fg_rect.bottom >= mr.bottom
        {
            Some(monitor)
        } else {
            None
        }
    }
}

#[tauri::command]
async fn get_ui_scale(app: tauri::AppHandle) -> Result<f64, String> {
    #[cfg(target_os = "windows")]
    {
        let win = app.get_webview_window("mini").ok_or("mini not found")?;
        if let Ok(Some(m)) = win.current_monitor() {
            return Ok(win_ui_scale(&m));
        }
    }
    Ok(1.0)
}

/// Get the notch half-width (distance from screen center to notch edge) using
/// macOS 12+ `auxiliaryTopRightArea` API. Falls back to 80pt for older systems
/// or screens without a notch (external displays, pre-notch Macs).
#[cfg(target_os = "macos")]
unsafe fn get_notch_offset(screen: *mut objc2::runtime::AnyObject) -> f64 {
    use objc2::msg_send;
    use objc2_foundation::NSRect;

    if screen.is_null() { return 80.0; }
    let sel = objc2::runtime::Sel::register(c"auxiliaryTopRightArea");
    let responds: bool = msg_send![&*screen, respondsToSelector: sel];
    if responds {
        let right_area: NSRect = msg_send![&*screen, auxiliaryTopRightArea];
        if right_area.size.width > 0.0 {
            let frame: NSRect = msg_send![&*screen, frame];
            let center_x = frame.origin.x + frame.size.width / 2.0;
            let half_w = right_area.origin.x - center_x;
            if half_w > 10.0 { return half_w; }
        }
    }
    80.0
}

/// Move the mini window by a delta (dx, dy in CSS/logical points).
/// dy is in screen coordinates (positive = downward), converted to macOS (positive = upward).
#[tauri::command]
async fn move_mini_by(app: tauri::AppHandle, dx: f64, dy: f64) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::AnyObject;
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};
            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
                let frame: NSRect = unsafe { msg_send![obj, frame] };
                let new_frame = NSRect::new(
                    NSPoint::new(frame.origin.x + dx, frame.origin.y - dy),
                    NSSize::new(frame.size.width, frame.size.height),
                );
                unsafe {
                    let _: () = msg_send![obj, setFrame: new_frame, display: true, animate: false];
                }
                if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                    *f = Some((new_frame.origin.x, new_frame.origin.y, new_frame.size.width, new_frame.size.height));
                }
                // Keep the pet-context restore frame in sync when dragging
                // while the context menu is open, so closing restores to the
                // new position instead of the stale pre-drag position.
                if let Ok(mut saved) = PET_MENU_RESTORE_FRAME.lock() {
                    if let Some(ref mut s) = *saved {
                        s.0 += dx;
                        s.1 -= dy; // macOS: screen y is bottom-up, dy is top-down
                    }
                }
            }
        }).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        // outer_position() returns PhysicalPosition; dx/dy are in logical (CSS) pixels.
        // Convert physical → logical before adding the delta.
        if let Ok(pos) = win.outer_position() {
            let scale = win.scale_factor().unwrap_or(1.0);
            let logical_x = pos.x as f64 / scale;
            let logical_y = pos.y as f64 / scale;
            let _ = win.set_position(tauri::LogicalPosition::new(logical_x + dx, logical_y + dy));
        }
    }
    Ok(())
}

/// Get the mini window's origin in logical coordinates.
/// macOS: bottom-left origin (NSWindow frame).
/// Windows: top-left origin (screen coordinates).
#[tauri::command]
async fn get_mini_origin(app: tauri::AppHandle) -> Result<(f64, f64), String> {
    let win = app.get_webview_window("mini").ok_or("mini not found")?;
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let win_clone = win.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::AnyObject;
            use objc2::msg_send;
            use objc2_foundation::NSRect;
            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
                let frame: NSRect = unsafe { msg_send![obj, frame] };
                let _ = tx.send((frame.origin.x, frame.origin.y));
            }
        }).map_err(|e| e.to_string())?;
        if let Ok(pos) = rx.recv_timeout(std::time::Duration::from_secs(1)) {
            return Ok(pos);
        }
    }
    #[cfg(target_os = "windows")]
    {
        // outer_position() returns PhysicalPosition; convert to logical for consistency.
        if let Ok(pos) = win.outer_position() {
            let scale = win.scale_factor().unwrap_or(1.0);
            return Ok((pos.x as f64 / scale, pos.y as f64 / scale));
        }
    }
    Err("failed to get origin".into())
}

/// Set the mini window's origin in logical coordinates.
/// macOS: bottom-left origin. Windows: top-left origin.
#[tauri::command]
async fn set_mini_origin(app: tauri::AppHandle, x: f64, y: f64) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini not found")?;
    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};
            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
                let frame: NSRect = unsafe { msg_send![obj, frame] };
                let screen_frame: NSRect = unsafe {
                    let screen: *mut AnyObject = msg_send![obj, screen];
                    if screen.is_null() {
                        let cls = match AnyClass::get(c"NSScreen") {
                            Some(c) => c,
                            None => return,
                        };
                        let main_screen: *mut AnyObject = msg_send![cls, mainScreen];
                        if main_screen.is_null() {
                            return;
                        }
                        msg_send![&*main_screen, frame]
                    } else {
                        msg_send![&*screen, frame]
                    }
                };

                let min_x = screen_frame.origin.x;
                let max_x = (screen_frame.origin.x + screen_frame.size.width - frame.size.width).max(min_x);
                let min_y = screen_frame.origin.y;
                let max_y = (screen_frame.origin.y + screen_frame.size.height - frame.size.height).max(min_y);
                let clamped_x = x.max(min_x).min(max_x);
                let clamped_y = y.max(min_y).min(max_y);
                let new_frame = NSRect::new(
                    NSPoint::new(clamped_x, clamped_y),
                    NSSize::new(frame.size.width, frame.size.height),
                );
                unsafe {
                    let _: () = msg_send![obj, setFrame: new_frame, display: true, animate: false];
                }
                if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                    *f = Some((new_frame.origin.x, new_frame.origin.y, new_frame.size.width, new_frame.size.height));
                }
            }
        }).map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
    }
    Ok(())
}

/// Kept as a compatibility no-op while macOS IME handling is fixed directly on
/// the underlying Wry webview class.
#[tauri::command]
async fn set_ime_mode(_app: tauri::AppHandle, _active: bool) -> Result<(), String> {
    Ok(())
}

/// Resize/reposition the mini window between collapsed (small, right of notch)
/// and expanded (larger, centered on notch) states.
#[tauri::command]
async fn set_mini_expanded(app: tauri::AppHandle, expanded: bool, position: Option<String>, efficiency: Option<bool>, max_height: Option<f64>, mascot_scale: Option<f64>, large_mascot: Option<bool>, keep_position: Option<bool>, large_mascot_scale: Option<f64>) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    let pos = position.unwrap_or_else(|| "right".to_string());
    let mascot_scale = sanitized_mascot_scale(mascot_scale);
    let large_mascot_scale = large_mascot_scale.unwrap_or(LARGE_MASCOT_SIZE_MULTIPLIER);

    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};

            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };

                let screen_info: Option<(f64, f64, f64, f64, f64)> = unsafe {
                    let screen: *mut AnyObject = msg_send![obj, screen];
                    if screen.is_null() {
                        let cls = match AnyClass::get(c"NSScreen") {
                            Some(c) => c,
                            None => return,
                        };
                        let main_screen: *mut AnyObject = msg_send![cls, mainScreen];
                        if main_screen.is_null() { return; }
                        let sf: NSRect = msg_send![&*main_screen, frame];
                        let notch_off = get_notch_offset(main_screen);
                        Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height, notch_off))
                    } else {
                        let sf: NSRect = msg_send![&*screen, frame];
                        let notch_off = get_notch_offset(screen);
                        Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height, notch_off))
                    }
                };

                if let Some((sx, sy, sw, sh, notch_off)) = screen_info {
                    // Cache screen geometry for the efficiency hover poll thread.
                    if let Ok(mut info) = NOTCH_SCREEN_INFO.lock() {
                        *info = Some((sx, sy, sw, sh, notch_off));
                    }
                    EFFICIENCY_EXPANDED.store(expanded, Ordering::SeqCst);

                    unsafe {
                        let _: () = msg_send![obj, setLevel: 27isize];
                    }
                    let (final_x, final_y, final_w, final_h) = if expanded {
                        let win_w = if efficiency.unwrap_or(false) { 600.0 } else { 500.0 };
                        let win_h = max_height.unwrap_or(350.0).max(200.0).min(500.0);
                        let x = sx + (sw - win_w) / 2.0;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                            let ns_app_cls = AnyClass::get(c"NSApplication").unwrap();
                            let ns_app: *mut AnyObject = msg_send![ns_app_cls, sharedApplication];
                            let _: () = msg_send![&*ns_app, activateIgnoringOtherApps: true];
                            let null: *mut AnyObject = std::ptr::null_mut();
                            let _: () = msg_send![obj, makeKeyAndOrderFront: null];
                        }
                        (x, y, win_w, win_h)
                    } else {
                        let (win_w, win_h) = if large_mascot.unwrap_or(false) {
                            large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale)
                        } else {
                            collapsed_mascot_window_size(mascot_scale)
                        };
                        let (x, y) = if keep_position.unwrap_or(false) {
                            let cur: NSRect = unsafe { msg_send![obj, frame] };
                            (cur.origin.x, cur.origin.y + cur.size.height - win_h)
                        } else if large_mascot.unwrap_or(false) {
                            let margin_x = 10.0;
                            let margin_y = 300.0;
                            (sx + sw - win_w - margin_x, sy + margin_y)
                        } else {
                            (collapsed_x(sx, sw, win_w, &pos, notch_off), sy + sh - win_h)
                        };
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                        (x, y, win_w, win_h)
                    };
                    // Cache the real window frame for the hover poll thread.
                    if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                        *f = Some((final_x, final_y, final_w, final_h));
                    }
                }
            }
        }).map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows: DPI-aware positioning and sizing.
        // Use monitor.position() to offset into the correct monitor in the virtual desktop.
        if let Ok(Some(monitor)) = win.current_monitor() {
            let scale = monitor.scale_factor();
            let mp = monitor.position();
            let mx = mp.x as f64 / scale;
            let my = mp.y as f64 / scale;
            let sw = monitor.size().width as f64 / scale;
            let ui = win_ui_scale(&monitor);
            if expanded {
                let base_w = if efficiency.unwrap_or(false) { 600.0 } else { 500.0 };
                let win_w = (base_w * ui).round();
                let win_h = (400.0 * ui).round();
                let x = mx + (sw - win_w) / 2.0;
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_position(tauri::LogicalPosition::new(x, my));
            } else {
                let (base_w, base_h) = if large_mascot.unwrap_or(false) {
                    large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale)
                } else {
                    collapsed_mascot_window_size(mascot_scale)
                };
                let win_w = (base_w * ui).round();
                let win_h = (base_h * ui).round();
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                if !keep_position.unwrap_or(false) {
                    if large_mascot.unwrap_or(false) {
                        // Large mascot defaults to bottom-right corner.
                        let sh = monitor.size().height as f64 / scale;
                        let margin = (10.0 * ui).round();
                        let x = mx + sw - win_w - margin;
                        let y = my + sh - win_h - margin;
                        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
                    } else {
                        let notch_off = (80.0 * ui).round();
                        let x = mx + if pos == "left" { sw / 2.0 - notch_off - win_w } else { sw / 2.0 + notch_off };
                        let _ = win.set_position(tauri::LogicalPosition::new(x, my));
                    }
                }
            }
        }
        if !FULLSCREEN_HIDING.load(std::sync::atomic::Ordering::SeqCst) {
            let _ = win.set_always_on_top(true);
        }
    }

    Ok(())
}

/// Start or stop cursor-position polling for efficiency-mode hover detection.
///
/// On macOS the mini window sits in the menu-bar / notch area. The system
/// menu bar intercepts mouse-move events, so the webview never receives
/// `mouseenter` / `mouseleave` DOM events there.  This command spawns a
/// lightweight background thread (50 ms poll) that reads `NSEvent.mouseLocation`
/// and compares it against the notch region (collapsed) or the panel region
/// (expanded).  It emits `"efficiency-hover"` events (`true` = entered,
/// `false` = left) so the frontend can open / close the panel.
#[tauri::command]
async fn set_efficiency_hover_tracking(app: tauri::AppHandle, active: bool) -> Result<(), String> {
    EFFICIENCY_HOVER_ACTIVE.store(active, Ordering::SeqCst);
    if active && !EFFICIENCY_HOVER_THREAD_ALIVE.load(Ordering::SeqCst) {
        let app2 = app.clone();
        std::thread::spawn(move || efficiency_hover_poll(app2));
    }
    Ok(())
}

/// Background polling loop for efficiency-mode hover.
/// Checks the cursor position against two regions:
///  - **Collapsed**: a wide strip around the notch (notch_off*2 + 200 px,
///    50 px tall at the top of the screen) — much wider than the actual
///    window so the user can approach from either side.
///  - **Expanded**: the panel area (500 × 400 px, top-center).
fn efficiency_hover_poll(app: tauri::AppHandle) {
    use std::time::{Duration, Instant};
    EFFICIENCY_HOVER_THREAD_ALIVE.store(true, Ordering::SeqCst);
    let mut was_inside = false;
    let mut last_enter_emit = Instant::now();
    while EFFICIENCY_HOVER_ACTIVE.load(Ordering::SeqCst) {
        let info = NOTCH_SCREEN_INFO.lock().ok().and_then(|g| *g);
        let sleep_ms = if let Some((sx, sy, sw, sh, notch_off)) = info {
            let cursor = macos_cursor_position();
            let is_expanded = EFFICIENCY_EXPANDED.load(Ordering::SeqCst);

            let inside = if is_expanded {
                if let Some((fx, fy, fw, fh)) = MINI_WINDOW_FRAME.lock().ok().and_then(|g| *g) {
                    cursor.0 >= fx && cursor.0 <= fx + fw
                        && cursor.1 >= fy && cursor.1 <= fy + fh
                } else {
                    false
                }
            } else {
                let rw = (notch_off * 2.0 + 10.0).max(80.0);
                let rh = MINI_WINDOW_FRAME
                    .lock()
                    .ok()
                    .and_then(|g| *g)
                    .map(|(_, _, _, fh)| fh.clamp(20.0, 28.0))
                    .unwrap_or(35.0);
                let rx = sx + (sw - rw) / 2.0;
                let ry = sy + sh - rh;
                cursor.0 >= rx && cursor.0 <= rx + rw
                    && cursor.1 >= ry && cursor.1 <= ry + rh
            };

            if inside && !was_inside {
                let _ = app.emit("efficiency-hover", true);
                last_enter_emit = Instant::now();
            } else if inside && was_inside && last_enter_emit.elapsed() > Duration::from_millis(300) {
                let _ = app.emit("efficiency-hover", true);
                last_enter_emit = Instant::now();
            } else if !inside && was_inside {
                let _ = app.emit("efficiency-hover", false);
            }
            was_inside = inside;

            // Adaptive polling: fast when cursor is near/inside the target,
            // slow when far away to save CPU/battery.
            if is_expanded || inside {
                30
            } else {
                let screen_top = sy + sh;
                let dist_from_top = screen_top - cursor.1;
                if dist_from_top < 200.0 {
                    50
                } else {
                    500
                }
            }
        } else {
            500
        };
        std::thread::sleep(Duration::from_millis(sleep_ms));
    }
    EFFICIENCY_HOVER_THREAD_ALIVE.store(false, Ordering::SeqCst);
}

/// Read the current mouse cursor position via `[NSEvent mouseLocation]`.
/// Returns (x, y) in macOS screen coordinates (bottom-left origin).
#[cfg(target_os = "macos")]
fn macos_cursor_position() -> (f64, f64) {
    unsafe {
        use objc2::msg_send;
        use objc2_foundation::NSPoint;
        if let Some(cls) = objc2::runtime::AnyClass::get(c"NSEvent") {
            let loc: NSPoint = msg_send![cls, mouseLocation];
            (loc.x, loc.y)
        } else {
            (0.0, 0.0)
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn macos_cursor_position() -> (f64, f64) {
    (0.0, 0.0)
}

/// Resize the expanded mini window height while keeping it top-aligned.
/// macOS: bottom-left origin, so adjust y to keep bottom aligned.
/// Windows: top-left origin, so just resize height.
#[tauri::command]
async fn resize_mini_height(app: tauri::AppHandle, height: f64, max_height: Option<f64>, animate: Option<bool>) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    let limit = max_height.unwrap_or(350.0).max(200.0).min(2000.0);
    // Scale height limits on Windows to match DPI-aware window sizes
    #[cfg(target_os = "windows")]
    let h = {
        let ui = if let Ok(Some(m)) = win.current_monitor() { win_ui_scale(&m) } else { 1.0 };
        (height * ui).round().max(45.0 * ui).min(limit * ui)
    };
    #[cfg(not(target_os = "windows"))]
    let h = height.max(45.0).min(limit);

    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};

            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
                let screen: *mut AnyObject = unsafe { msg_send![obj, screen] };
                let screen_ptr = if screen.is_null() {
                    let cls = match AnyClass::get(c"NSScreen") { Some(c) => c, None => return };
                    let ms: *mut AnyObject = unsafe { msg_send![cls, mainScreen] };
                    if ms.is_null() { return; }
                    ms
                } else { screen };
                let sf: NSRect = unsafe { msg_send![&*screen_ptr, frame] };
                let cur: NSRect = unsafe { msg_send![obj, frame] };
                let capped_h = h.min((sf.size.height * 0.75).max(200.0));
                let new_y = sf.origin.y + sf.size.height - capped_h;
                let new_frame = NSRect::new(
                    NSPoint::new(cur.origin.x, new_y),
                    NSSize::new(cur.size.width, capped_h),
                );
                unsafe {
                    let do_animate: bool = animate.unwrap_or(false);
                    let _: () = msg_send![obj, setFrame: new_frame, display: true, animate: do_animate];
                }
                if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                    *f = Some((cur.origin.x, new_y, cur.size.width, capped_h));
                }
            }
        }).map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows: keep top-left position, just change height
        if let Ok(size) = win.outer_size() {
            let scale = win.scale_factor().unwrap_or(1.0);
            let _ = win.set_size(tauri::LogicalSize::new(size.width as f64 / scale, h));
        }
    }

    Ok(())
}

/// Schedule restoring the NSWindow alpha to 1.0 after the webview has had
/// time to composite at the new frame size.  Uses GCD `dispatch_after_f` on
/// the main queue so the restore runs at a precise time without thread-spawn
/// overhead.  A generation counter (`PET_ALPHA_GEN`) prevents stale callbacks
/// from restoring alpha during a subsequent resize (fast double-clicks).
#[cfg(target_os = "macos")]
fn pet_context_schedule_restore_alpha(ns_win_ptr: *mut std::ffi::c_void) {
    extern "C" {
        // dispatch_get_main_queue() is a C macro; the real symbol is a global.
        #[link_name = "_dispatch_main_q"]
        static DISPATCH_MAIN_Q: std::ffi::c_void;
        fn dispatch_after_f(
            when: u64,
            queue: *const std::ffi::c_void,
            context: *mut std::ffi::c_void,
            work: extern "C" fn(*mut std::ffi::c_void),
        );
        fn dispatch_time(when: u64, delta: i64) -> u64;
    }

    /// Packed context passed through GCD void* pointer.
    struct RestoreCtx {
        ns_win: *mut std::ffi::c_void,
        gen: u64,
    }

    extern "C" fn restore_alpha(ctx_raw: *mut std::ffi::c_void) {
        let ctx = unsafe { Box::from_raw(ctx_raw as *mut RestoreCtx) };
        // Only restore if no newer resize has happened since we were scheduled.
        if PET_ALPHA_GEN.load(Ordering::SeqCst) != ctx.gen {
            return;
        }
        use objc2::msg_send;
        let obj = unsafe { &*(ctx.ns_win as *const objc2::runtime::AnyObject) };
        unsafe {
            let _: () = msg_send![obj, setAlphaValue: 1.0f64];
        }
    }

    let gen = PET_ALPHA_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let ctx = Box::new(RestoreCtx { ns_win: ns_win_ptr, gen });
    unsafe {
        // 34ms ≈ 2 frames at 60Hz — minimal delay for the webview to
        // finish compositing at the new window size.
        let when = dispatch_time(0, 34_000_000); // nanoseconds
        dispatch_after_f(
            when,
            &DISPATCH_MAIN_Q as *const std::ffi::c_void,
            Box::into_raw(ctx) as *mut std::ffi::c_void,
            restore_alpha,
        );
    }
}

/// Expand the mini window to pet-context size and start a cursor-position poll
/// that toggles `setIgnoresMouseEvents:` — the transparent area around the
/// mascot passes clicks through to the desktop. When the context menu is open
/// (`PET_CONTEXT_MENU_OPEN`), the entire window accepts clicks.
///
/// Pass `active: false` to stop the poll and shrink back to collapsed size.
#[tauri::command]
async fn set_pet_mode_window(
    app: tauri::AppHandle,
    active: bool,
    mascot_scale: Option<f64>,
    large_mascot_scale: Option<f64>,
) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    let mascot_scale = sanitized_mascot_scale(mascot_scale);
    let large_mascot_scale = large_mascot_scale.unwrap_or(LARGE_MASCOT_SIZE_MULTIPLIER);

    if active {
        // Expand window to menu-ready size (mascot area + padding for buttons).
        #[cfg(target_os = "macos")]
        {
            let win_clone = win.clone();
            app.run_on_main_thread(move || {
                use objc2::runtime::{AnyClass, AnyObject};
                use objc2::msg_send;
                use objc2_foundation::{NSRect, NSPoint, NSSize};
                if let Ok(ns_win) = win_clone.ns_window() {
                    let obj = unsafe { &*(ns_win as *mut AnyObject) };
                    let current: NSRect = unsafe { msg_send![obj, frame] };
                    let screen_info: Option<(f64, f64, f64, f64)> = unsafe {
                        let screen: *mut AnyObject = msg_send![obj, screen];
                        if screen.is_null() {
                            let cls = AnyClass::get(c"NSScreen");
                            cls.and_then(|c| {
                                let ms: *mut AnyObject = msg_send![c, mainScreen];
                                if ms.is_null() { None } else {
                                    let sf: NSRect = msg_send![&*ms, frame];
                                    Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height))
                                }
                            })
                        } else {
                            let sf: NSRect = msg_send![&*screen, frame];
                            Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height))
                        }
                    };
                    if let Some((sx, sy, sw, sh)) = screen_info {
                        let left_pad = 180.0;
                        let top_pad = 100.0;
                        let win_w = (current.size.width + left_pad).min(sw);
                        let win_h = (current.size.height + top_pad).min(sh);
                        // Keep bottom-right corner fixed (mascot stays there).
                        let mut x = current.origin.x + current.size.width - win_w;
                        let y = current.origin.y;
                        x = x.max(sx).min(sx + sw - win_w);
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            // Start with clicks passing through until the poll takes over.
                            let _: () = msg_send![obj, setIgnoresMouseEvents: true];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                            let _: () = msg_send![obj, setLevel: 27isize];
                            let _: () = msg_send![obj, orderFrontRegardless];
                        }
                        if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                            *f = Some((x, y, win_w, win_h));
                        }
                    }
                }
            }).map_err(|e| e.to_string())?;
        }

        // Start the click-through poll thread.
        PET_PASSTHROUGH_ACTIVE.store(true, Ordering::SeqCst);
        if !PET_PASSTHROUGH_THREAD_ALIVE.load(Ordering::SeqCst) {
            let app2 = app.clone();
            std::thread::spawn(move || pet_passthrough_poll(app2, mascot_scale, large_mascot_scale));
        }
    } else {
        // Stop the poll thread.
        PET_PASSTHROUGH_ACTIVE.store(false, Ordering::SeqCst);
        PET_CONTEXT_MENU_OPEN.store(false, Ordering::SeqCst);

        // Shrink back to collapsed mascot size and re-enable mouse events.
        #[cfg(target_os = "macos")]
        {
            let win_clone = win.clone();
            app.run_on_main_thread(move || {
                use objc2::runtime::AnyObject;
                use objc2::msg_send;
                use objc2_foundation::{NSRect, NSPoint, NSSize};
                if let Ok(ns_win) = win_clone.ns_window() {
                    let obj = unsafe { &*(ns_win as *mut AnyObject) };
                    let current: NSRect = unsafe { msg_send![obj, frame] };
                    let (win_w, win_h) = large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale);
                    // Collapse towards bottom-right corner.
                    let x = current.origin.x + current.size.width - win_w;
                    let y = current.origin.y;
                    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                    unsafe {
                        let _: () = msg_send![obj, setIgnoresMouseEvents: false];
                        let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                    }
                    if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                        *f = Some((x, y, win_w, win_h));
                    }
                }
            }).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Tell the pet-mode pass-through poll whether the context menu is open.
#[tauri::command]
async fn set_pet_context_menu(open: bool) -> Result<(), String> {
    PET_CONTEXT_MENU_OPEN.store(open, Ordering::SeqCst);
    Ok(())
}

/// Polling loop for pet-mode click pass-through. Checks cursor position every
/// 20ms. When the cursor is over the mascot (bottom-right of the expanded
/// window) or the context menu is open, `setIgnoresMouseEvents: false` so the
/// webview receives events. Otherwise `setIgnoresMouseEvents: true` so clicks
/// pass through to whatever is behind.
#[cfg(target_os = "macos")]
fn pet_passthrough_poll(app: tauri::AppHandle, mascot_scale: f64, large_mascot_scale: f64) {
    use std::time::Duration;
    PET_PASSTHROUGH_THREAD_ALIVE.store(true, Ordering::SeqCst);
    let mut was_interactive = false;
    // Full visual size of the mascot in CSS points.
    let (mascot_w, mascot_h) = large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale);
    // Match the frontend hitbox (LARGE_MASCOT_HITBOX_WIDTH_MULTIPLIER / 3 and
    // LARGE_MASCOT_HITBOX_HEIGHT_MULTIPLIER / 3 of the visual size, centered).
    let hit_w = mascot_w * (1.8 / 3.0);
    let hit_h = mascot_h * (2.5 / 3.0);
    let inset_x = (mascot_w - hit_w) / 2.0;
    let inset_y = (mascot_h - hit_h) / 2.0;

    while PET_PASSTHROUGH_ACTIVE.load(Ordering::SeqCst) {
        let menu_open = PET_CONTEXT_MENU_OPEN.load(Ordering::SeqCst);
        let frame = MINI_WINDOW_FRAME.lock().ok().and_then(|g| *g);

        let should_be_interactive = if menu_open {
            true
        } else if let Some((fx, fy, fw, _fh)) = frame {
            let cursor = macos_cursor_position();
            // Mascot visual area is at the bottom-right corner of the window.
            // The hitbox is a centered sub-region within that visual area.
            let mascot_left = fx + fw - mascot_w;
            let mascot_bottom = fy;
            let hit_left = mascot_left + inset_x;
            let hit_right = mascot_left + mascot_w - inset_x;
            let hit_bottom = mascot_bottom + inset_y;
            let hit_top = mascot_bottom + mascot_h - inset_y;
            cursor.0 >= hit_left && cursor.0 <= hit_right
                && cursor.1 >= hit_bottom && cursor.1 <= hit_top
        } else {
            false
        };

        if should_be_interactive != was_interactive {
            let app1 = app.clone();
            let app2 = app.clone();
            let val = should_be_interactive;
            let _ = app1.run_on_main_thread(move || {
                if let Some(win) = app2.get_webview_window("mini") {
                    if let Ok(ns_win) = win.ns_window() {
                        use objc2::msg_send;
                        let obj = unsafe { &*(ns_win as *mut objc2::runtime::AnyObject) };
                        unsafe {
                            let _: () = msg_send![obj, setIgnoresMouseEvents: !val];
                        }
                    }
                }
            });
            was_interactive = should_be_interactive;
        }

        std::thread::sleep(Duration::from_millis(20));
    }

    // Ensure events are re-enabled when the thread exits.
    let app_exit = app.clone();
    let _ = app.run_on_main_thread(move || {
        if let Some(win) = app_exit.get_webview_window("mini") {
            if let Ok(ns_win) = win.ns_window() {
                use objc2::msg_send;
                let obj = unsafe { &*(ns_win as *mut objc2::runtime::AnyObject) };
                unsafe {
                    let _: () = msg_send![obj, setIgnoresMouseEvents: false];
                }
            }
        }
    });
    PET_PASSTHROUGH_THREAD_ALIVE.store(false, Ordering::SeqCst);
}

/// Resize the mini window to 3/4 of screen, centered, with normal window level.
/// Used for settings/update modal mode. Pass `restore: true` to go back to mini mode.
#[tauri::command]
async fn set_mini_size(
    app: tauri::AppHandle,
    restore: bool,
    position: Option<String>,
    keep_on_top: Option<bool>,
    pet_context: Option<bool>,
    mascot_scale: Option<f64>,
    large_mascot: Option<bool>,
    large_mascot_scale: Option<f64>,
) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    let pos = position.unwrap_or_else(|| "right".to_string());
    let want_top = keep_on_top.unwrap_or(restore);
    let is_pet_context = pet_context.unwrap_or(false);
    let mascot_scale = sanitized_mascot_scale(mascot_scale);
    let large_mascot_scale = large_mascot_scale.unwrap_or(LARGE_MASCOT_SIZE_MULTIPLIER);

    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};

            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };

                let screen_info: Option<(f64, f64, f64, f64, f64)> = unsafe {
                    let screen: *mut AnyObject = msg_send![obj, screen];
                    if screen.is_null() {
                        let cls = match AnyClass::get(c"NSScreen") {
                            Some(c) => c,
                            None => return,
                        };
                        let main_screen: *mut AnyObject = msg_send![cls, mainScreen];
                        if main_screen.is_null() { return; }
                        let sf: NSRect = msg_send![&*main_screen, frame];
                        let notch_off = get_notch_offset(main_screen);
                        Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height, notch_off))
                    } else {
                        let sf: NSRect = msg_send![&*screen, frame];
                        let notch_off = get_notch_offset(screen);
                        Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height, notch_off))
                    }
                };

                if let Some((sx, sy, sw, sh, notch_off)) = screen_info {
                    // Keep the hover poll's screen geometry cache fresh even when
                    // the mini window is temporarily resized into settings/update mode.
                    if let Ok(mut info) = NOTCH_SCREEN_INFO.lock() {
                        *info = Some((sx, sy, sw, sh, notch_off));
                    }
                    if is_pet_context {
                        if restore {
                            let current: NSRect = unsafe { msg_send![obj, frame] };
                            if let Ok(mut saved) = PET_MENU_RESTORE_FRAME.lock() {
                                if let Some((x, y, win_w, win_h)) = *saved {
                                    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                                    // Hide window before shrink to avoid compositor
                                    // flashing the old large-frame content at the
                                    // wrong position inside the smaller window.
                                    unsafe {
                                        let _: () = msg_send![obj, setAlphaValue: 0.0f64];
                                        let _: () = msg_send![obj, setLevel: if want_top { 27isize } else { 0isize }];
                                        let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                                        if want_top {
                                            let _: () = msg_send![obj, orderFrontRegardless];
                                        }
                                    }
                                    *saved = None;
                                    if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                                        *f = Some((x, y, win_w, win_h));
                                    }
                                    // Restore alpha after the webview repaints at
                                    // the new size.  dispatch_after on the main
                                    // queue with a
                                    // short delay lets the compositor
                                    // finish compositing the new frame.
                                    pet_context_schedule_restore_alpha(ns_win as *mut std::ffi::c_void);
                                    return;
                                }
                            }
                            // Fallback: if save frame is missing (e.g. race/double close),
                            // still collapse around current center instead of jumping to
                            // default corner placement.
                            let (target_w, target_h) = if large_mascot.unwrap_or(false) {
                                large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale)
                            } else {
                                collapsed_mascot_window_size(mascot_scale)
                            };
                            let mut x = current.origin.x + (current.size.width - target_w) / 2.0;
                            let mut y = current.origin.y + (current.size.height - target_h) / 2.0;
                            x = x.max(sx).min(sx + sw - target_w);
                            y = y.max(sy).min(sy + sh - target_h);
                            let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(target_w, target_h));
                            unsafe {
                                let _: () = msg_send![obj, setAlphaValue: 0.0f64];
                                let _: () = msg_send![obj, setLevel: if want_top { 27isize } else { 0isize }];
                                let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                                if want_top {
                                    let _: () = msg_send![obj, orderFrontRegardless];
                                }
                            }
                            if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                                *f = Some((x, y, target_w, target_h));
                            }
                            pet_context_schedule_restore_alpha(ns_win as *mut std::ffi::c_void);
                            return;
                        } else {
                            let current: NSRect = unsafe { msg_send![obj, frame] };
                            if let Ok(mut saved) = PET_MENU_RESTORE_FRAME.lock() {
                                *saved = Some((current.origin.x, current.origin.y, current.size.width, current.size.height));
                            }
                            // Expand LEFT and UP, keeping the bottom-right corner
                            // of the window fixed (macOS: origin.x+width, origin.y).
                            // The mascot stays at bottom-right via CSS absolute pos.
                            let left_pad = 180.0;
                            let top_pad = 100.0;
                            let win_w = (current.size.width + left_pad).min(sw);
                            let win_h = (current.size.height + top_pad).min(sh);
                            let mut x = current.origin.x + current.size.width - win_w;
                            let mut y = current.origin.y;
                            x = x.max(sx).min(sx + sw - win_w);
                            y = y.max(sy).min(sy + sh - win_h);
                            let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                            // Hide → resize → delayed restore, same as the
                            // shrink path, to prevent the old small-window
                            // content from flashing at the top-left of the
                            // newly expanded frame.
                            unsafe {
                                let _: () = msg_send![obj, setAlphaValue: 0.0f64];
                                let _: () = msg_send![obj, setLevel: if want_top { 27isize } else { 0isize }];
                                let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                                if want_top {
                                    let _: () = msg_send![obj, orderFrontRegardless];
                                }
                            }
                            if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                                *f = Some((x, y, win_w, win_h));
                            }
                            pet_context_schedule_restore_alpha(ns_win as *mut std::ffi::c_void);
                            return;
                        }
                    }
                    if restore {
                        let (win_w, win_h) = if large_mascot.unwrap_or(false) {
                            large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale)
                        } else {
                            collapsed_mascot_window_size(mascot_scale)
                        };
                        let (x, y) = if large_mascot.unwrap_or(false) {
                            let margin = 10.0;
                            (sx + sw - win_w - margin, sy + margin)
                        } else {
                            (collapsed_x(sx, sw, win_w, &pos, notch_off), sy + sh - win_h)
                        };
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setLevel: if want_top { 27isize } else { 0isize }];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                            if want_top {
                                let _: () = msg_send![obj, orderFrontRegardless];
                            }
                        }
                        // Restoring from settings/update mode returns the widget to
                        // the collapsed notch state, so the hover poll must switch
                        // back to collapsed-region detection immediately.
                        EFFICIENCY_EXPANDED.store(false, Ordering::SeqCst);
                        if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                            *f = Some((x, y, win_w, win_h));
                        }
                    } else {
                        let win_w = (sw * 0.85).round();
                        let win_h = (sh * 0.85).round();
                        let x = sx + (sw - win_w) / 2.0;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setLevel: if want_top { 27isize } else { 0isize }];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                            if want_top {
                                let _: () = msg_send![obj, orderFrontRegardless];
                            }
                        }
                        // Settings/update mode is not the normal expanded panel.
                        // Clear the expanded hover state so a stale panel frame does
                        // not survive after the window is later restored.
                        EFFICIENCY_EXPANDED.store(false, Ordering::SeqCst);
                        if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                            *f = Some((x, y, win_w, win_h));
                        }
                    }
                }
            }
        }).map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        if let Ok(Some(monitor)) = win.current_monitor() {
            let scale = monitor.scale_factor();
            let mp = monitor.position();
            let mx = mp.x as f64 / scale;
            let my = mp.y as f64 / scale;
            let sw = monitor.size().width as f64 / scale;
            let sh = monitor.size().height as f64 / scale;
            let ui = win_ui_scale(&monitor);
            if restore {
                let (base_w, base_h) = if large_mascot.unwrap_or(false) {
                    large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale)
                } else {
                    collapsed_mascot_window_size(mascot_scale)
                };
                let win_w = (base_w * ui).round();
                let win_h = (base_h * ui).round();
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_always_on_top(want_top && !FULLSCREEN_HIDING.load(std::sync::atomic::Ordering::SeqCst));
                if large_mascot.unwrap_or(false) {
                    let margin = (10.0 * ui).round();
                    let x = mx + sw - win_w - margin;
                    let y = my + sh - win_h - margin;
                    let _ = win.set_position(tauri::LogicalPosition::new(x, y));
                } else {
                    let notch_off = (80.0 * ui).round();
                    let x = mx + if pos == "left" { sw / 2.0 - notch_off - win_w } else { sw / 2.0 + notch_off };
                    let _ = win.set_position(tauri::LogicalPosition::new(x, my));
                }
            } else {
                let win_w = (sw * 0.85).round();
                let win_h = (sh * 0.85).round();
                let x = mx + (sw - win_w) / 2.0;
                let _ = win.set_always_on_top(want_top && !FULLSCREEN_HIDING.load(std::sync::atomic::Ordering::SeqCst));
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_position(tauri::LogicalPosition::new(x, my));
            }
        }
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct MiniSessionInfo {
    pub key: String,
    #[serde(rename = "agentId")]
    pub agent_id: String,
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub label: String,
    pub channel: Option<String>,
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    pub active: bool,
    #[serde(rename = "lastUserMsg")]
    pub last_user_msg: Option<String>,
    #[serde(rename = "lastAssistantMsg")]
    pub last_assistant_msg: Option<String>,
    #[serde(rename = "sessionFile", default, skip_serializing_if = "Option::is_none")]
    pub session_file: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ChatMessage {
    pub role: String,
    pub text: String,
    pub timestamp: Option<String>,
}

/// Extract the actual user message from raw text, stripping all system/channel noise.
fn clean_user_message(text: &str) -> String {
    // Skip system startup messages entirely
    if text.starts_with("A new session was started") { return String::new(); }

    let mut s = text.to_string();

    // Queued messages: extract the last actual message from the queue
    if s.starts_with("[Queued messages") || s.starts_with("Queued #") {
        // Find the last "Queued #N" block and process it
        if let Some(idx) = s.rfind("Queued #") {
            s = s[idx..].to_string();
            // Skip the "Queued #N" line
            if let Some(nl) = s.find('\n') {
                s = s[nl + 1..].to_string();
            }
        }
        // Now s contains the last queued message content, fall through to normal cleaning
    }

    // Strip channel metadata blocks and extract actual message.
    // Formats:
    //   1) With [message_id:...] line → actual message is after "Name: msg"
    //   2) Without [message_id:] but has ``` blocks → actual message is after last ```
    if s.contains("(untrusted metadata)") || s.contains("Conversation info (untrusted metadata)") || s.contains("[message_id:") {
        if let Some(idx) = s.rfind("[message_id:") {
            if let Some(nl) = s[idx..].find('\n') {
                let after = s[idx + nl + 1..].trim();
                // Format: "Name: actual message" or just "actual message"
                if let Some(colon) = after.find(": ") {
                    let name_part = &after[..colon];
                    if name_part.len() < 40 && !name_part.contains('\n') {
                        s = after[colon + 2..].to_string();
                    } else {
                        s = after.to_string();
                    }
                } else {
                    s = after.to_string();
                }
            }
        } else {
            // Has metadata but no [message_id:], extract after last ``` block
            if let Some(idx) = s.rfind("```\n") {
                s = s[idx + 4..].trim().to_string();
            }
        }
    }

    // Strip [media attached: ...] prefix - keep text after it if any
    if s.starts_with("[media attached:") {
        if let Some(end) = s.find("]\n") {
            s = s[end + 2..].to_string();
        } else if let Some(end) = s.find(']') {
            s = s[end + 1..].trim().to_string();
        }
    }

    // Strip system prompt prefix
    if let Some(idx) = s.find("\n\nHuman: ") {
        s = s[idx + 9..].to_string();
    }

    // Strip all [[...]] markers anywhere in text (e.g. [[reply_to_current]])
    while let Some(start) = s.find("[[") {
        if let Some(end) = s[start..].find("]]") {
            s = format!("{}{}", &s[..start], &s[start + end + 2..]);
        } else { break; }
    }

    // Strip timestamp prefix like "[Mon 2026-03-16 01:58 GMT+8] "
    {
        let trimmed = s.trim_start();
        if trimmed.starts_with('[') {
            if let Some(end) = trimmed.find("] ") {
                let bracket_content = &trimmed[1..end];
                // Check if it looks like a timestamp (contains digits and GMT/UTC or day names)
                if bracket_content.len() < 50
                    && (bracket_content.contains("GMT") || bracket_content.contains("UTC")
                        || bracket_content.contains("Mon") || bracket_content.contains("Tue")
                        || bracket_content.contains("Wed") || bracket_content.contains("Thu")
                        || bracket_content.contains("Fri") || bracket_content.contains("Sat")
                        || bracket_content.contains("Sun"))
                {
                    s = trimmed[end + 2..].to_string();
                }
            }
        }
    }

    // Strip "Current time: ..." lines and everything after
    if let Some(idx) = s.find("\nCurrent time:") {
        s = s[..idx].to_string();
    }
    if let Some(idx) = s.find("Current time:") {
        if idx == 0 { return String::new(); }
        s = s[..idx].to_string();
    }

    // Strip cron prefix like "[cron:xxx 喝水提醒] "
    if s.starts_with("[cron:") {
        if let Some(end) = s.find("] ") {
            s = s[end + 2..].to_string();
        }
    }

    // Strip "Return your summary as plain text..." suffix
    if let Some(idx) = s.find("\nReturn your summary") {
        s = s[..idx].to_string();
    }
    if let Some(idx) = s.find("Return your summary") {
        if idx == 0 { return String::new(); }
    }

    s.trim().to_string()
}

/// Strip all [[...]] markers from text.
fn strip_brackets(text: &str) -> String {
    let mut s = text.to_string();
    while let Some(start) = s.find("[[") {
        if let Some(end) = s[start..].find("]]") {
            s = format!("{}{}", &s[..start], &s[start + end + 2..]);
        } else { break; }
    }
    s.trim().to_string()
}

/// Extract last user + assistant message from a .jsonl session file (reads from end).
fn extract_last_messages(content: &str) -> (Option<String>, Option<String>) {
    let mut last_user: Option<String> = None;
    let mut last_assistant: Option<String> = None;
    for line in content.lines() {
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if val["type"].as_str() != Some("message") { continue; }
        let msg = &val["message"];
        let role = msg["role"].as_str().unwrap_or("");
        let text = if let Some(arr) = msg["content"].as_array() {
            arr.iter()
                .filter(|i| i["type"].as_str() == Some("text"))
                .filter_map(|i| i["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else if let Some(s) = msg["content"].as_str() {
            s.to_string()
        } else {
            continue;
        };
        if text.is_empty() { continue; }
        match role {
            "user" => {
                let cleaned = clean_user_message(&text);
                if cleaned.is_empty() { continue; }
                let truncated = if cleaned.chars().count() > 120 {
                    let s: String = cleaned.chars().take(120).collect();
                    format!("{}...", s)
                } else { cleaned };
                last_user = Some(truncated);
            }
            "assistant" => {
                let cleaned = strip_brackets(&text);
                if cleaned.is_empty() { continue; }
                let truncated = if cleaned.chars().count() > 120 {
                    let s: String = cleaned.chars().take(120).collect();
                    format!("{}...", s)
                } else { cleaned };
                last_assistant = Some(truncated);
            }
            _ => {}
        }
    }
    (last_user, last_assistant)
}

#[tauri::command]
async fn get_agent_sessions(agent_id: String, mode: Option<String>, url: Option<String>, token: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<Vec<MiniSessionInfo>, String> {
    log::info!("[get_agent_sessions] agent_id={} mode={:?} ssh_host={:?}", agent_id, mode, ssh_host);
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            // SSH: only read sessions.json metadata (1 SSH call).
            // Session file content is loaded lazily via get_session_preview.
            let sess_path = remote_sessions_json_path(&agent_id);
            log::info!("[get_agent_sessions] SSH reading metadata: {}", sess_path);
            let content = match ssh_read_file(sh, su, &sess_path).await {
                Ok(c) => { log::info!("[get_agent_sessions] SSH read OK, len={}", c.len()); c }
                Err(e) => { log::error!("[get_agent_sessions] SSH read failed: {}", e); return Err(format!("read remote sessions.json: {}", e)); }
            };
            let map: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&content).map_err(|e| { log::error!("[get_agent_sessions] parse failed: {}", e); e.to_string() })?;

            let mut sessions: Vec<MiniSessionInfo> = map.iter()
                .filter(|(key, val)| {
                    !key.contains(":cron:") && !val["sessionFile"].as_str().unwrap_or("").is_empty()
                })
                .map(|(key, val)| MiniSessionInfo {
                    key: key.clone(),
                    agent_id: agent_id.clone(),
                    session_id: val["sessionId"].as_str().unwrap_or(key).to_string(),
                    label: key.clone(),
                    channel: val["lastChannel"].as_str().map(|s| s.to_string()),
                    updated_at: val["updatedAt"].as_u64().unwrap_or(0),
                    active: false,
                    last_user_msg: None,
                    last_assistant_msg: None,
                    session_file: val["sessionFile"].as_str().map(|s| s.to_string()),
                })
                .collect();
            sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            sessions.truncate(5);
            log::info!("[get_agent_sessions] SSH metadata result: {} sessions (of {} total)", sessions.len(), map.len());
            return Ok(sessions);
        }
        // Gateway API fallback
        let url = url.as_deref().unwrap_or("");
        let token = token.as_deref().unwrap_or("");
        let result = invoke_tool(url, token, "sessions_list", serde_json::json!({"agentId": agent_id, "activeMinutes": 60})).await?;
        let arr = extract_sessions(&result);
        let mut sessions: Vec<MiniSessionInfo> = arr.iter().filter_map(|s| {
            let key = s["key"].as_str().or(s["sessionId"].as_str())?.to_string();
            if key.contains(":cron:") { return None; }
            Some(MiniSessionInfo {
                key: key.clone(),
                agent_id: s["agentId"].as_str()
                    .or_else(|| s["key"].as_str().and_then(|k| k.split(':').nth(1)))
                    .unwrap_or(&agent_id).to_string(),
                session_id: s["sessionId"].as_str().unwrap_or(&key).to_string(),
                label: key.clone(),
                channel: s["channel"].as_str().map(|s| s.to_string()),
                updated_at: s["updatedAt"].as_u64().unwrap_or(0),
                active: is_session_active(s),
                last_user_msg: s["lastUserMsg"].as_str().map(|s| s.to_string()),
                last_assistant_msg: s["lastAssistantMsg"].as_str().map(|s| s.to_string()),
                session_file: None,
            })
        }).collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sessions.truncate(20);
        return Ok(sessions);
    }

    // === local mode (original) ===
    let path = sessions_json_path(&agent_id);
    log::info!("[get_agent_sessions] local mode, path={:?}, exists={}", path, path.exists());
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| { log::error!("[get_agent_sessions] read sessions.json failed: {}", e); format!("read sessions.json: {}", e) })?;
    log::info!("[get_agent_sessions] sessions.json len={}, keys count={}", content.len(), content.matches('"').count() / 2);
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    log::info!("[get_agent_sessions] parsed {} top-level keys", map.len());

    let home = home_dir_string();
    let agent_dir = if agent_id.is_empty() { "main" } else { &agent_id };

    // Check which sessions are active
    // On macOS: original lsof-based detection scoped to agent dir
    // On Windows: cross-platform helper + content-based fallback
    #[cfg(not(windows))]
    let open_jsonl: std::collections::HashSet<String> = {
        let search_path = format!("{}/.openclaw/agents/{}", home, agent_dir);
        let lsof_bin = if std::path::Path::new("/usr/sbin/lsof").exists() { "/usr/sbin/lsof" } else { "lsof" };
        let lsof_stdout = tokio::process::Command::new(lsof_bin)
            .args(["+D", &search_path])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output().await
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        lsof_stdout.lines()
            .filter(|l| l.contains(".jsonl"))
            .filter_map(|l| l.split_whitespace().last().map(|s| s.to_string()))
            .collect()
    };
    #[cfg(windows)]
    let open_jsonl = lsof_open_jsonl_paths().await;

    let mut sessions: Vec<MiniSessionInfo> = Vec::new();
    let mut skipped_no_msg = 0u32;
    let mut skipped_cron = 0u32;
    let mut skipped_no_file = 0u32;
    let mut read_ok = 0u32;
    let mut read_err = 0u32;
    for (key, val) in map.iter() {
        let session_file_raw = val["sessionFile"].as_str().unwrap_or("").to_string();
        let session_id_str = val["sessionId"].as_str().unwrap_or(key.as_str());

        // If sessionFile is empty, try to infer from sessionId
        let session_file = if !session_file_raw.is_empty() {
            session_file_raw
        } else if !session_id_str.is_empty() {
            let sessions_dir = PathBuf::from(&home).join(".openclaw").join("agents").join(agent_dir).join("sessions");
            sessions_dir.join(format!("{}.jsonl", session_id_str)).to_string_lossy().to_string()
        } else {
            String::new()
        };

        if session_file.is_empty() { skipped_no_file += 1; continue; }

        // Active detection: macOS uses lsof path matching, Windows adds content-based fallback
        #[cfg(not(windows))]
        let is_active = open_jsonl.iter().any(|p| {
            p.starts_with(&session_file) || session_file.starts_with(p.as_str())
        });
        #[cfg(windows)]
        let is_active = {
            let mut active = open_jsonl.iter().any(|p| {
                p.starts_with(&session_file) || session_file.starts_with(p.as_str())
                || p.replace('\\', "/").starts_with(&session_file.replace('\\', "/"))
                || session_file.replace('\\', "/").starts_with(&p.replace('\\', "/"))
            });
            if !active {
                let lines = tail_lines_from_file(std::path::Path::new(&session_file), 5);
                active = check_agent_active_from_lines(&lines);
            }
            active
        };

        // Read last messages from .jsonl
        let (last_user, last_assistant) = match tokio::fs::read_to_string(&session_file).await {
            Ok(c) => { read_ok += 1; extract_last_messages(&c) }
            Err(_) => { read_err += 1; (None, None) }
        };

        // Skip sessions with no messages or cron task sessions
        if last_user.is_none() && last_assistant.is_none() { skipped_no_msg += 1; continue; }
        if key.contains(":cron:") { skipped_cron += 1; continue; }

        sessions.push(MiniSessionInfo {
            key: key.clone(),
            agent_id: agent_id.clone(),
            session_id: val["sessionId"].as_str().unwrap_or(key).to_string(),
            label: key.clone(),
            channel: val["lastChannel"].as_str().map(|s| s.to_string()),
            updated_at: val["updatedAt"].as_u64().unwrap_or(0),
            active: is_active,
            last_user_msg: last_user,
            last_assistant_msg: last_assistant,
            session_file: Some(session_file),
        });
    }

    log::info!("[get_agent_sessions] results: {} sessions, skipped: no_file={} read_err={} no_msg={} cron={}, read_ok={}", 
        sessions.len(), skipped_no_file, read_err, skipped_no_msg, skipped_cron, read_ok);
    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions.truncate(20);
    Ok(sessions)
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SessionPreview {
    pub active: bool,
    #[serde(rename = "lastUserMsg")]
    pub last_user_msg: Option<String>,
    #[serde(rename = "lastAssistantMsg")]
    pub last_assistant_msg: Option<String>,
}

#[tauri::command]
async fn get_session_preview(session_file: String, mode: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<SessionPreview, String> {
    log::info!("[get_session_preview] file={} mode={:?}", session_file, mode);
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            let escaped = session_file.replace('"', r#"\""#);
            let cmd = format!(
                "tail -50 \"{}\" 2>/dev/null",
                escaped
            );
            let output = ssh_exec(sh, su, &cmd).await
                .map_err(|e| { log::error!("[get_session_preview] SSH failed: {}", e); format!("session preview: {}", e) })?;

            let active = check_agent_active_from_lines(
                &output.lines().map(|l| l.to_string()).collect::<Vec<_>>()
            );
            let (last_user, last_assistant) = extract_last_messages(&output);
            log::info!("[get_session_preview] active={} has_user={} has_asst={}", active, last_user.is_some(), last_assistant.is_some());
            return Ok(SessionPreview { active, last_user_msg: last_user, last_assistant_msg: last_assistant });
        }
    }

    // Local mode
    let content = tokio::fs::read_to_string(&session_file).await
        .map_err(|e| format!("read session file: {}", e))?;
    let lines: Vec<String> = content.lines().map(|l| l.to_string()).collect();
    let tail: Vec<String> = lines.iter().rev().take(5).rev().cloned().collect();
    let active = check_agent_active_from_lines(&tail);
    let (last_user, last_assistant) = extract_last_messages(&content);
    Ok(SessionPreview { active, last_user_msg: last_user, last_assistant_msg: last_assistant })
}

#[tauri::command]
async fn get_session_messages(agent_id: String, session_key: String, mode: Option<String>, url: Option<String>, token: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<Vec<ChatMessage>, String> {
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            // SSH-based: read .jsonl session file like local mode
            let sess_path = remote_sessions_json_path(&agent_id);
            let content = ssh_read_file(sh, su, &sess_path).await
                .map_err(|e| format!("read remote sessions.json: {}", e))?;
            let map: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&content).map_err(|e| e.to_string())?;
            let session = map.get(&session_key).ok_or("session not found")?;
            let file = session["sessionFile"].as_str().ok_or("no sessionFile")?;
            let jsonl = ssh_read_file(sh, su, file).await
                .map_err(|e| format!("read remote session file: {}", e))?;

            let mut messages: Vec<ChatMessage> = vec![];
            for line in jsonl.lines() {
                let val: serde_json::Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };
                if val["type"].as_str() != Some("message") { continue; }
                let msg = &val["message"];
                let role = msg["role"].as_str().unwrap_or("");
                if role != "user" && role != "assistant" { continue; }
                let ts = val["timestamp"].as_str().map(|s| s.to_string());
                let text = if let Some(arr) = msg["content"].as_array() {
                    arr.iter()
                        .filter(|item| item["type"].as_str() == Some("text"))
                        .filter_map(|item| item["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("\n")
                } else if let Some(s) = msg["content"].as_str() {
                    s.to_string()
                } else { continue };
                if text.is_empty() { continue; }
                let clean_text = if role == "user" {
                    let cleaned = clean_user_message(&text);
                    if cleaned.is_empty() { continue; }
                    cleaned
                } else {
                    let cleaned = strip_brackets(&text);
                    if cleaned.is_empty() { continue; }
                    cleaned
                };
                messages.push(ChatMessage { role: role.to_string(), text: clean_text, timestamp: ts });
            }
            if messages.len() > 50 { messages = messages.split_off(messages.len() - 50); }
            return Ok(messages);
        }
        // Gateway API fallback
        let url = url.as_deref().unwrap_or("");
        let token = token.as_deref().unwrap_or("");
        let result = invoke_tool(url, token, "sessions_history", serde_json::json!({
            "sessionKey": session_key,
            "limit": 50,
            "includeTools": false
        })).await?;
        let r = result.get("result").unwrap_or(&result);
        let det = r.get("details").unwrap_or(r);
        let empty_arr = vec![];
        let messages_arr = det.get("messages").and_then(|v| v.as_array()).unwrap_or(&empty_arr);
        let mut messages: Vec<ChatMessage> = vec![];
        for msg in messages_arr {
            let role = msg["role"].as_str().unwrap_or("");
            if role != "user" && role != "assistant" { continue; }
            let content = if let Some(arr) = msg["content"].as_array() {
                arr.iter()
                    .filter(|item| item["type"].as_str() == Some("text"))
                    .filter_map(|item| item["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n")
            } else if let Some(s) = msg["content"].as_str() {
                s.to_string()
            } else { continue };
            if content.is_empty() { continue; }
            let ts = msg["timestamp"].as_u64().map(|ms| {
                chrono::DateTime::from_timestamp((ms / 1000) as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%dT%H:%M:%SZ").to_string())
                    .unwrap_or_default()
            });
            messages.push(ChatMessage { role: role.to_string(), text: content, timestamp: ts });
        }
        return Ok(messages);
    }

    let path = sessions_json_path(&agent_id);
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read sessions.json: {}", e))?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    let session = map.get(&session_key).ok_or("session not found")?;
    let file = session["sessionFile"].as_str().ok_or("no sessionFile")?;

    let jsonl = tokio::fs::read_to_string(file)
        .await
        .map_err(|e| format!("read session file: {}", e))?;

    let mut messages: Vec<ChatMessage> = vec![];
    for line in jsonl.lines() {
        let val: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if val["type"].as_str() != Some("message") { continue; }
        let msg = &val["message"];
        let role = msg["role"].as_str().unwrap_or("");
        if role != "user" && role != "assistant" { continue; }

        let ts = val["timestamp"].as_str().map(|s| s.to_string());

        // Extract text from content array
        let text = if let Some(arr) = msg["content"].as_array() {
            arr.iter()
                .filter(|item| item["type"].as_str() == Some("text"))
                .filter_map(|item| item["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        } else if let Some(s) = msg["content"].as_str() {
            s.to_string()
        } else {
            continue;
        };

        if text.is_empty() { continue; }

        let clean_text = if role == "user" {
            let cleaned = clean_user_message(&text);
            if cleaned.is_empty() { continue; }
            cleaned
        } else {
            let cleaned = strip_brackets(&text);
            if cleaned.is_empty() { continue; }
            cleaned
        };

        messages.push(ChatMessage {
            role: role.to_string(),
            text: clean_text,
            timestamp: ts,
        });
    }

    // Return last 50 messages
    if messages.len() > 50 {
        messages = messages.split_off(messages.len() - 50);
    }
    Ok(messages)
}

/// Lightweight: returns set of "agentId:sessionKey" that are currently active.
/// Only does lsof + reads sessions.json (no .jsonl content parsing).
#[tauri::command]
async fn get_active_sessions(mode: Option<String>, url: Option<String>, token: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<Vec<String>, String> {
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            // Step 1: Single SSH command to read all sessions.json files
            let list_cmd = r#"for d in $HOME/.openclaw/agents/*/; do id=$(basename "$d"); sj="$d/sessions.json"; [ -f "$sj" ] || continue; echo "AGENT_SESSIONS:$id"; cat "$sj"; echo ""; echo "END_AGENT_SESSIONS"; done"#;
            let list_output = ssh_exec(sh, su, list_cmd).await.unwrap_or_default();
            log::info!("[get_active_sessions] remote step1 output len={}", list_output.len());

            // Parse: collect (agentId, sessionKey, sessionFile) tuples
            let mut to_check: Vec<(String, String, String)> = vec![];
            let mut current_agent: Option<String> = None;
            let mut json_buf = String::new();
            for line in list_output.lines() {
                if let Some(id) = line.strip_prefix("AGENT_SESSIONS:") {
                    if let Some(prev_id) = current_agent.take() {
                        if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json_buf) {
                            for (key, val) in map.iter() {
                                if let Some(sf) = val["sessionFile"].as_str() {
                                    if !sf.is_empty() { to_check.push((prev_id.clone(), key.clone(), sf.to_string())); }
                                }
                            }
                        }
                    }
                    current_agent = Some(id.to_string());
                    json_buf.clear();
                } else if line == "END_AGENT_SESSIONS" {
                    if let Some(prev_id) = current_agent.take() {
                        if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&json_buf) {
                            for (key, val) in map.iter() {
                                if let Some(sf) = val["sessionFile"].as_str() {
                                    if !sf.is_empty() { to_check.push((prev_id.clone(), key.clone(), sf.to_string())); }
                                }
                            }
                        }
                    }
                    json_buf.clear();
                } else {
                    json_buf.push_str(line);
                    json_buf.push('\n');
                }
            }

            log::info!("[get_active_sessions] remote parsed {} sessions to check", to_check.len());
            if to_check.is_empty() { return Ok(vec![]); }

            // Step 2: Single SSH command to tail all session files
            let check_parts: Vec<String> = to_check.iter().map(|(aid, key, sf)| {
                format!("echo 'SESSION:{}:{}'; tail -5 '{}' 2>/dev/null; echo 'END_SESSION'", aid, key, sf)
            }).collect();
            let check_cmd = check_parts.join("\n");
            let check_output = ssh_exec(sh, su, &check_cmd).await.unwrap_or_default();

            // Parse: check each session's tail for activity
            let mut active_keys: Vec<String> = vec![];
            let mut current_session: Option<String> = None;
            let mut lines_buf: Vec<String> = Vec::new();
            for line in check_output.lines() {
                if let Some(rest) = line.strip_prefix("SESSION:") {
                    if let Some(prev_key) = current_session.take() {
                        if check_agent_active_from_lines(&lines_buf) {
                            active_keys.push(prev_key);
                        }
                    }
                    current_session = Some(rest.to_string());
                    lines_buf.clear();
                } else if line == "END_SESSION" {
                    if let Some(prev_key) = current_session.take() {
                        if check_agent_active_from_lines(&lines_buf) {
                            active_keys.push(prev_key);
                        }
                    }
                    lines_buf.clear();
                } else {
                    lines_buf.push(line.to_string());
                }
            }
            log::info!("[get_active_sessions] remote result: {:?}", active_keys);
            return Ok(active_keys);
        }
        // Gateway API fallback
        let url = url.as_deref().unwrap_or("");
        let token = token.as_deref().unwrap_or("");
        let result = invoke_tool(url, token, "sessions_list", serde_json::json!({"activeMinutes": 5})).await?;
        let sessions = extract_sessions(&result);
        let mut keys: Vec<String> = vec![];
        for s in &sessions {
            let session_key = match s["key"].as_str() {
                Some(k) => k,
                None => continue,
            };
            let agent_id = s["agentId"].as_str()
                .or_else(|| session_key.split(':').nth(1))
                .unwrap_or("main");
            if is_remote_session_active(url, token, session_key, s).await {
                keys.push(format!("{}:{}", agent_id, session_key));
            }
        }
        return Ok(keys);
    }

    // === local mode ===
    // Use both lsof (process-based) and content-based detection for reliability.
    // lsof works well for processes that hold files open (e.g. Claude Code),
    // but OC gateway may write-and-close, so we fall back to content-based check.
    let open_paths = lsof_open_jsonl_paths().await;

    let home = home_dir_string();
    let agents_dir = std::path::PathBuf::from(&home).join(".openclaw").join("agents");
    let mut active_keys: Vec<String> = vec![];

    let Ok(entries) = std::fs::read_dir(&agents_dir) else { return Ok(vec![]); };
    for entry in entries.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()) {
        let agent_id = entry.file_name().to_string_lossy().to_string();
        let sess_path = sessions_json_path(&agent_id);
        let Ok(content) = tokio::fs::read_to_string(&sess_path).await else { continue; };
        let Ok(map): Result<serde_json::Map<String, serde_json::Value>, _> = serde_json::from_str(&content) else { continue; };

        for (key, val) in map.iter() {
            let session_file = val["sessionFile"].as_str().unwrap_or("");
            let session_id = val["sessionId"].as_str().unwrap_or("");
            let file_path = if !session_file.is_empty() {
                session_file.to_string()
            } else if !session_id.is_empty() {
                format!("{}/.openclaw/agents/{}/sessions/{}.jsonl", home, agent_id, session_id)
            } else { continue; };

            // Check 1: lsof detects file held open by a process
            let lsof_active = open_paths.iter().any(|p| p.starts_with(&file_path) || file_path.starts_with(p.as_str()));
            if lsof_active {
                active_keys.push(format!("{}:{}", agent_id, key));
                continue;
            }
            // Check 2: content-based — read last 5 lines for efficiency
            #[cfg(windows)]
            let lines = tail_lines_from_file(std::path::Path::new(&file_path), 5);
            #[cfg(not(windows))]
            let lines = {
                tokio::process::Command::new("tail")
                    .args(["-5", &file_path])
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::null())
                    .output().await.ok()
                    .map(|o| String::from_utf8_lossy(&o.stdout).lines().map(|l| l.to_string()).collect::<Vec<_>>())
                    .unwrap_or_default()
            };
            if check_agent_active_from_lines(&lines) {
                active_keys.push(format!("{}:{}", agent_id, key));
            }
        }
    }
    Ok(active_keys)
}

#[tauri::command]
async fn open_detail_panel(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(win) = app.get_webview_window("detail") {
        win.show().map_err(|e| e.to_string())?;
        win.set_focus().map_err(|e| e.to_string())?;
        return Ok(());
    }

    let win = WebviewWindowBuilder::new(&app, "detail", WebviewUrl::App("index.html#/detail".into()))
        .title("oc-claw - Detail")
        .inner_size(480.0, 600.0)
        .decorations(true)
        .resizable(true)
        .center()
        .build()
        .map_err(|e| e.to_string())?;
    let _ = win.maximize();

    Ok(())
}

/// Proxy a POST request to bypass CORS restrictions in the webview.
#[tauri::command]
async fn proxy_post(url: String, body: String) -> Result<String, String> {
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("request failed: {}", e))?;
    let status = resp.status().as_u16();
    let text = resp.text().await.map_err(|e| format!("read body: {}", e))?;
    if status >= 400 {
        return Err(format!("HTTP {}: {}", status, text));
    }
    Ok(text)
}

// ─── Play system sound ───
#[tauri::command]
async fn play_sound(name: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use objc2::runtime::{AnyClass, AnyObject};
        use objc2::msg_send;
        let name_clone = name.clone();
        std::thread::spawn(move || {
            unsafe {
                let cls = match AnyClass::get(c"NSSound") {
                    Some(c) => c,
                    None => return,
                };
                let ns_string_cls = AnyClass::get(c"NSString").unwrap();
                let c_str = std::ffi::CString::new(name_clone.as_bytes()).unwrap();
                let ns_name: *mut AnyObject = msg_send![ns_string_cls, stringWithUTF8String: c_str.as_ptr()];
                let sound: *mut AnyObject = msg_send![cls, soundNamed: ns_name];
                if !sound.is_null() {
                    let _: () = msg_send![&*sound, play];
                }
            }
        });
    }
    #[cfg(target_os = "windows")]
    {
        // Map macOS system sound names to Windows equivalents.
        // Windows PlaySound uses registry aliases: SystemAsterisk, SystemExclamation, etc.
        let win_sound = match name.as_str() {
            "Blow" | "Basso" | "Funk" | "Sosumi" => "SystemExclamation",
            "Bottle" | "Pop" | "Purr" | "Tink" => "SystemAsterisk",
            "Glass" | "Ping" => "SystemDefault",
            "Hero" | "Morse" | "Submarine" => "SystemNotification",
            "Frog" => "SystemQuestion",
            _ => "SystemDefault",
        };
        let sound_name = win_sound.to_string();
        std::thread::spawn(move || {
            use windows::Win32::Media::Audio::{PlaySoundW, SND_ALIAS, SND_ASYNC};
            use windows::core::PCWSTR;
            let wide: Vec<u16> = sound_name.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                let _ = PlaySoundW(PCWSTR(wide.as_ptr()), None, SND_ALIAS | SND_ASYNC);
            }
        });
    }
    Ok(())
}

// ─── Claude Code session state ───

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ClaudeSession {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub cwd: String,
    pub status: String, // processing, waiting, idle, tool_running, compacting, stopped
    pub tool: Option<String>,
    #[serde(rename = "toolInput")]
    pub tool_input: Option<String>,
    #[serde(rename = "userPrompt")]
    pub user_prompt: Option<String>,
    pub interactive: bool,
    #[serde(rename = "updatedAt")]
    pub updated_at: u64,
    /// Derived from Claude's own status field: true when status != "waiting_for_input"
    #[serde(rename = "isProcessing")]
    pub is_processing: bool,
    /// PID of the Claude Code process that owns this session.
    /// Used to detect Ctrl+C exits: if the PID is dead and status is "waiting",
    /// the session is stale and should be cleared.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    /// Number of sub-agents (Agent tool) still running in the background.
    /// Incremented on PreToolUse(Agent), decremented on SubagentStop.
    /// Sound only plays on Stop when this reaches 0 (all agents done).
    #[serde(skip)]
    pub pending_agents: u32,
    /// Raw permission_suggestions JSON from the PermissionRequest hook event.
    #[serde(rename = "permissionSuggestions", skip_serializing_if = "Option::is_none")]
    pub permission_suggestions: Option<serde_json::Value>,
    /// AI's last response text (truncated), forwarded from the Stop hook event.
    /// Shown in the efficiency-mode completion reminder popup.
    #[serde(rename = "lastResponse", skip_serializing_if = "Option::is_none")]
    pub last_response: Option<String>,
    /// Whether this session's terminal tab is currently the active/focused tab.
    /// Set dynamically in `get_claude_sessions` — not persisted.
    #[serde(rename = "isActiveTab")]
    pub is_active_tab: bool,
    /// Source of this session: "cc" (Claude Code), "codex" (Codex), or "cursor" (Cursor IDE).
    pub source: String,
    /// Ghostty terminal `id` captured when the session is first seen.
    /// Used by `jump_to_claude_terminal` to select the exact tab instead
    /// of relying on CWD/title matching which is ambiguous.
    #[serde(skip)]
    pub terminal_id: Option<String>,
    /// Host terminal app name (e.g. "Ghostty", "Cursor", "iTerm2").
    /// Captured once at session creation via process chain walk.
    #[serde(skip)]
    pub host_terminal: Option<String>,
    /// Bound Cursor extension port for this session.
    /// Unlike `pid`, this is stable for the lifetime of a Cursor window.
    /// We resolve it from the session cwd/workspace and reuse it on click.
    #[serde(skip)]
    pub cursor_port: Option<u16>,
    /// Workspace root matched to the bound Cursor window.
    /// Stored so we can revalidate the binding when the session cwd changes.
    #[serde(skip)]
    pub cursor_workspace_root: Option<String>,
    /// Human-readable workspace name reported by the Cursor extension.
    /// Used to raise the correct Cursor window on macOS before focusing content.
    #[serde(skip)]
    pub cursor_workspace_name: Option<String>,
    /// Native window handle (hex string) from the Cursor extension.
    /// Uniquely identifies a Cursor window even when multiple windows
    /// share the same workspace root.
    #[serde(skip)]
    pub cursor_native_handle: Option<String>,
}

type PendingPermissions = Arc<Mutex<HashMap<String, std::sync::mpsc::Sender<String>>>>;

struct ClaudeState {
    sessions: Arc<Mutex<HashMap<String, ClaudeSession>>>,
    pending_permissions: PendingPermissions,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct CursorWindowMeta {
    port: u16,
    #[serde(default)]
    focused: bool,
    #[serde(default, rename = "workspaceName")]
    workspace_name: String,
    #[serde(default, rename = "workspaceRoots")]
    workspace_roots: Vec<String>,
    #[serde(default, rename = "nativeHandle")]
    native_handle: Option<String>,
}

#[derive(Debug, Clone)]
struct CursorWindowBinding {
    port: u16,
    workspace_root: String,
    workspace_name: String,
    native_handle: Option<String>,
}

/// Compute the JSONL session file path (matching notchi's ConversationParser.sessionFilePath)
fn claude_session_file_path(session_id: &str, cwd: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    // On Windows, Claude Code replaces all of / \ : . with "-" when computing
    // the project directory name (e.g. G:\Desktop\code → G--Desktop-code).
    // The colon after the drive letter (G:) must also be replaced.
    #[cfg(windows)]
    let project_dir = cwd.replace('/', "-").replace('\\', "-").replace(':', "-").replace('.', "-");
    #[cfg(not(windows))]
    let project_dir = cwd.replace('/', "-").replace('.', "-");
    home.join(".claude").join("projects").join(project_dir).join(format!("{}.jsonl", session_id))
}

fn collect_jsonl_files_recursive(root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(v) => v,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                out.push(path);
            }
        }
    }
    out
}

fn collect_claude_project_jsonl_files() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let claude_projects = home.join(".claude").join("projects");
    if !claude_projects.exists() {
        return Vec::new();
    }
    let mut out = Vec::new();
    if let Ok(project_dirs) = std::fs::read_dir(claude_projects) {
        for project_entry in project_dirs.flatten() {
            let project_dir = project_entry.path();
            if !project_dir.is_dir() {
                continue;
            }
            if let Ok(files) = std::fs::read_dir(project_dir) {
                for file_entry in files.flatten() {
                    let path = file_entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl") {
                        out.push(path);
                    }
                }
            }
        }
    }
    out
}

fn collect_codex_session_jsonl_files() -> Vec<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let codex_sessions = home.join(".Codex").join("sessions");
    collect_jsonl_files_recursive(&codex_sessions)
}

fn find_claude_session_file(session_id: &str) -> Option<PathBuf> {
    let target = format!("{}.jsonl", session_id);
    for path in collect_claude_project_jsonl_files() {
        if path.file_name().and_then(|n| n.to_str()) == Some(target.as_str()) {
            return Some(path);
        }
    }
    None
}

fn find_codex_session_file(session_id: &str) -> Option<PathBuf> {
    // Codex stores sessions as:
    //   ~/.Codex/sessions/YYYY/MM/DD/rollout-<timestamp>-<session_id>.jsonl
    // so we cannot derive the path from cwd; we must scan for a filename
    // containing the session id.
    for path in collect_codex_session_jsonl_files() {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.ends_with(".jsonl") && name.contains(session_id) {
            return Some(path);
        }
    }
    None
}

fn resolve_session_jsonl_path(session_id: &str, cwd: Option<&str>) -> Option<PathBuf> {
    // Prefer Claude's deterministic path when cwd is known, then fall back to
    // directory scans. This keeps existing behavior fast while adding Codex
    // compatibility.
    if let Some(cwd_str) = cwd {
        if !cwd_str.is_empty() {
            let by_cwd = claude_session_file_path(session_id, cwd_str);
            if by_cwd.exists() {
                return Some(by_cwd);
            }
        }
    }
    find_claude_session_file(session_id).or_else(|| find_codex_session_file(session_id))
}

/// Check if a JSONL file indicates an interrupted session
fn check_interrupted(path: &std::path::Path) -> bool {
    if let Ok(content) = std::fs::read_to_string(path) {
        // Determine interruption from the latest meaningful event.
        // This supports both Claude and Codex transcript formats.
        for line in content.lines().rev().take(120) {
            // Codex: explicit turn abort marker.
            if line.contains("\"type\":\"event_msg\"") && line.contains("\"type\":\"turn_aborted\"") {
                return true;
            }
            // Codex: tool call rejected by user (skip/deny).
            if line.contains("\"type\":\"function_call_output\"") {
                if line.contains("rejected by user")
                    || line.contains("Rejected(\\\"rejected by user\\\")")
                {
                    return true;
                }
                // A non-rejection function output means older interruption markers
                // no longer represent current state.
                return false;
            }
            // Any newer user message supersedes older interruption markers.
            if line.contains("\"type\":\"event_msg\"") && line.contains("\"type\":\"user_message\"") {
                return false;
            }
            if line.contains("\"type\":\"user\"") {
                return line.contains("[Request interrupted by user")
                    || line.contains("<turn_aborted>");
            }
        }
    }
    false
}

/// Determine whether a Stop event represents a user-aborted turn rather than a
/// normal completion. Cursor stop hooks expose a completion status, while some
/// clients only reveal the interruption via transcript markers.
fn stop_event_was_interrupted(event: &serde_json::Value, session_source: &str, claude_status: &str) -> bool {
    let status = claude_status.trim().to_ascii_lowercase();
    if session_source == "cursor" {
        if status == "completed" {
            return false;
        }
        if matches!(status.as_str(), "interrupted" | "cancelled" | "canceled" | "aborted" | "stopped") {
            return true;
        }
    }

    let stop_message = event.get("lastResponse")
        .or_else(|| event.get("last_assistant_message"))
        .or_else(|| event.get("codex_last_assistant_message"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if stop_message.contains("[Request interrupted by user")
        || stop_message.contains("<turn_aborted>")
        || stop_message.contains("turn_aborted")
        || stop_message.contains("rejected by user")
    {
        return true;
    }

    event.get("transcript_path")
        .and_then(|v| v.as_str())
        .filter(|p| !p.is_empty())
        .map(|p| check_interrupted(std::path::Path::new(p)))
        .unwrap_or(false)
}

// --- Session File Watcher (matching notchi's NotchiStateMachine) ---
use notify::{Watcher, RecursiveMode};

/// Global registry of active file watchers, keyed by session ID
static SESSION_WATCHERS: std::sync::LazyLock<Mutex<HashMap<String, notify::RecommendedWatcher>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

/// Debounce interval matching notchi's syncDebounce (100ms)
const WATCHER_DEBOUNCE_MS: u64 = 200;

fn start_session_file_watcher(
    session_id: String,
    jsonl_path: PathBuf,
    sessions: Arc<Mutex<HashMap<String, ClaudeSession>>>,
    app: tauri::AppHandle,
) {
    // Stop existing watcher for this session
    stop_session_file_watcher(&session_id);

    let sid = session_id.clone();
    let path_for_handler = jsonl_path.clone();

    // Record initial file size (to detect compact truncation)
    let initial_size = std::fs::metadata(&jsonl_path).map(|m| m.len()).unwrap_or(0);
    let last_size = Arc::new(Mutex::new(initial_size));

    let watcher_result = notify::recommended_watcher(move |res: Result<notify::Event, notify::Error>| {
        if let Ok(event) = res {
            // Only care about modifications
            if !event.kind.is_modify() { return; }

            let sessions2 = sessions.clone();
            let app2 = app.clone();
            let sid2 = sid.clone();
            let path2 = path_for_handler.clone();
            let last_size2 = last_size.clone();

            // Debounce: spawn a thread that waits before processing
            std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(WATCHER_DEBOUNCE_MS));

                let new_size = std::fs::metadata(&path2).map(|m| m.len()).unwrap_or(0);
                let mut prev = last_size2.lock().unwrap();
                *prev = new_size;

                let mut sessions_guard = sessions2.lock().unwrap();
                let session = match sessions_guard.get_mut(&sid2) {
                    Some(s) => s,
                    None => return,
                };

                let mut changed = false;

                // Interruption detection: active/waiting but file shows interrupted
                if matches!(session.status.as_str(), "processing" | "tool_running" | "waiting") {
                    if check_interrupted(&path2) {
                        log::info!("File watcher: interrupted session {}", sid2);
                        session.status = "stopped".to_string();
                        session.is_processing = false;
                        session.tool = None;
                        session.tool_input = None;
                        session.permission_suggestions = None;
                        changed = true;
                    }
                }


                if changed {
                    session.updated_at = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
                    let _ = app2.emit("claude-session-update", &sid2);
                    // Don't emit claude-task-complete here — the Stop hook event
                    // already emits it. This avoids double sound playback.
                }
            });
        }
    });

    match watcher_result {
        Ok(mut watcher) => {
            if let Err(e) = watcher.watch(&jsonl_path, RecursiveMode::NonRecursive) {
                log::error!("Failed to watch session file {:?}: {}", jsonl_path, e);
                return;
            }
            log::info!("Started file watcher for session {} at {:?}", session_id, jsonl_path);
            SESSION_WATCHERS.lock().unwrap().insert(session_id, watcher);
        }
        Err(e) => {
            log::error!("Failed to create file watcher: {}", e);
        }
    }
}

fn stop_session_file_watcher(session_id: &str) {
    if let Some(_watcher) = SESSION_WATCHERS.lock().unwrap().remove(session_id) {
        log::info!("Stopped file watcher for session {}", session_id);
        // Watcher is dropped, which stops it
    }
}

/// Check whether a process with the given PID is still alive.
/// Uses kill(pid, 0) on Unix — a zero-cost syscall that checks existence
/// without sending any signal. On Windows, uses OpenProcess.
fn is_pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        // kill(pid, 0) returns 0 if the process exists and we have permission
        // to signal it; returns -1 with ESRCH if the process doesn't exist.
        unsafe { libc::kill(pid as i32, 0) == 0 }
    }
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
        use windows::Win32::Foundation::CloseHandle;
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid);
            match handle {
                Ok(h) => { let _ = CloseHandle(h); true }
                Err(_) => false,
            }
        }
    }
}

/// Get the terminal ID of Ghostty's currently focused tab, if Ghostty is frontmost.
/// Returns None if Ghostty is not running or not frontmost.
#[cfg(target_os = "macos")]
fn get_active_ghostty_terminal_id() -> Option<String> {
    let script = r#"
        if not (application "Ghostty" is running) then return ""
        tell application "System Events"
            set fp to name of first application process whose frontmost is true
        end tell
        if fp is not "Ghostty" then return ""
        tell application "Ghostty"
            try
                return id of first terminal of selected tab of front window as text
            end try
        end tell
        return ""
    "#;
    let output = std::process::Command::new("osascript")
        .arg("-e").arg(script)
        .output().ok()?;
    let tid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tid.is_empty() { None } else { Some(tid) }
}

#[cfg(not(target_os = "macos"))]
fn get_active_ghostty_terminal_id() -> Option<String> { None }

/// Returns the short name of the frontmost application (macOS only).
/// Used to suppress completion popups when the user is already looking
/// at the relevant app (Cursor, Codex, etc.).
#[cfg(target_os = "macos")]
fn get_frontmost_app_name() -> String {
    let script = r#"
        set appName to short name of (info for (path to frontmost application))
        return appName
    "#;
    let output = std::process::Command::new("osascript")
        .arg("-e").arg(script)
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(not(target_os = "macos"))]
fn get_frontmost_app_name() -> String { String::new() }

fn is_cursor_frontmost_app(name: &str) -> bool {
    name == "Cursor" || name == "oc-claw"
}

fn is_codex_frontmost_app(name: &str) -> bool {
    if name == "oc-claw" || name == "Code" || name == "Visual Studio Code" {
        return true;
    }
    let lowered = name.to_ascii_lowercase();
    lowered == "codex" || lowered.contains("codex")
}

fn is_codex_host_terminal(name: &str) -> bool {
    name == "Code" || name == "Visual Studio Code" || name.eq_ignore_ascii_case("codex")
}

/// Check if the frontmost app matches the host terminal name.
/// `host_terminal` comes from process-chain detection (e.g. "Terminal",
/// "iTerm2", "Warp") while `frontmost` is the short app name from
/// NSWorkspace (e.g. "Terminal", "iTerm2", "Warp").
/// Also handles "oc-claw" (our own panel can steal focus).
fn frontmost_matches_host_terminal(frontmost: &str, host_terminal: &str) -> bool {
    if frontmost == "oc-claw" {
        return true;
    }
    if frontmost.eq_ignore_ascii_case(host_terminal) {
        return true;
    }
    // macOS Terminal.app reports as "Terminal" in both NSWorkspace and ps
    if host_terminal == "Apple_Terminal" && frontmost == "Terminal" {
        return true;
    }
    false
}


#[tauri::command]
async fn get_claude_sessions(state: tauri::State<'_, ClaudeState>) -> Result<Vec<ClaudeSession>, String> {
    // Stale session guard: if the CC process was killed (Ctrl+C / SIGKILL)
    // without sending a follow-up hook event, any active status (waiting,
    // processing, tool_running, compacting) would get stuck forever.
    // Check PID liveness for all non-terminal statuses and clear to "stopped".
    // Uses per-session PID tracking + kill(pid, 0) — a zero-cost syscall.
    //
    // Cursor sessions use a different strategy: Cursor's hook processes are
    // short-lived (one per event), so $PPID dies immediately after each hook.
    // Instead of PID-alive checks, use a timeout: if no event arrives within
    // 120s, assume Cursor has stopped working.
    {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;
        let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        for session in sessions.values_mut() {
            let dominated = matches!(session.status.as_str(),
                "waiting" | "processing" | "tool_running" | "compacting");
            if !dominated { continue; }

            if session.source == "cursor" || session.source == "codex" {
                // Cursor/Codex: timeout-based staleness (120s without any event update).
                // Hook PPIDs are not always stable enough for PID-alive checks.
                let age_ms = now_ms.saturating_sub(session.updated_at);
                if age_ms > 120_000 {
                    log::info!(
                        "[get_claude_sessions] {} session {} stale ({}ms since last event), clearing {}",
                        session.source,
                        session.session_id,
                        age_ms,
                        session.status
                    );
                    session.status = "stopped".to_string();
                    session.pending_agents = 0;
                }
            } else {
                // CC: PID-alive check
                if let Some(pid) = session.pid {
                    if !is_pid_alive(pid) {
                        log::info!("[get_claude_sessions] CC pid {} dead, clearing {} for {}", pid, session.status, session.session_id);
                        session.status = "stopped".to_string();
                        session.pending_agents = 0;
                    }
                }
                // No pid recorded → can't verify, keep current status (CC is
                // likely using an older hook that doesn't send pid)
            }
        }
    }

    let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    let active_tid = get_active_ghostty_terminal_id();
    let mut list: Vec<ClaudeSession> = sessions.values()
        .filter(|s| !s.cwd.is_empty())
        .filter(|s| !is_codex_internal_utility_session(s))
        .cloned()
        .collect();
    // Mark sessions' active tab:
    // - Ghostty: match by terminal ID
    // - CC running inside Cursor's integrated terminal: check if Cursor is frontmost
    // - Cursor IDE sessions: set at Stop time in process_claude_event
    // - Codex standalone app: check if Codex/Code is frontmost
    let frontmost = get_frontmost_app_name();
    let cursor_is_active = is_cursor_frontmost_app(&frontmost);
    let codex_is_active = is_codex_frontmost_app(&frontmost);
    let is_ghostty = |s: &ClaudeSession| -> bool {
        matches!(s.host_terminal.as_deref(), Some("Ghostty" | "ghostty"))
    };
    if let Some(ref tid) = active_tid {
        for s in &mut list {
            if s.source != "cursor" && is_ghostty(s) {
                s.is_active_tab = s.terminal_id.as_deref() == Some(tid.as_str());
            }
        }
    }
    for s in &mut list {
        if s.source == "cursor" {
            continue;
        }
        if s.is_active_tab {
            continue;
        }
        if s.source == "codex" {
            s.is_active_tab = codex_is_active;
        } else if let Some(ht) = s.host_terminal.as_deref() {
            if ht == "Cursor" {
                s.is_active_tab = cursor_is_active;
            } else if is_codex_host_terminal(ht) {
                s.is_active_tab = codex_is_active;
            } else if !is_ghostty(s) {
                s.is_active_tab = frontmost_matches_host_terminal(&frontmost, ht);
            }
        }
    }
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(list)
}

#[tauri::command]
async fn remove_claude_session(session_id: String, state: tauri::State<'_, ClaudeState>) -> Result<(), String> {
    let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    sessions.remove(&session_id);
    Ok(())
}

/// Resolve a pending PermissionRequest for a Claude Code session.
/// `decision` is one of: "deny", "allow_once", "allow_all", "auto_approve"
/// The response JSON is sent back to the blocking hook script via the channel.
#[tauri::command]
async fn resolve_claude_permission(
    session_id: String,
    decision: String,
    state: tauri::State<'_, ClaudeState>,
) -> Result<(), String> {
    let tool_name = {
        let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        sessions.get(&session_id).and_then(|s| s.tool.clone())
    };

    let response_json = match decision.as_str() {
        "deny" => {
            serde_json::json!({
                "continue": true,
                "suppressOutput": true,
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "deny" }
                }
            }).to_string()
        }
        "allow_once" => {
            serde_json::json!({
                "continue": true,
                "suppressOutput": true,
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" }
                }
            }).to_string()
        }
        "allow_all" => {
            let rules = if let Some(name) = &tool_name {
                serde_json::json!([{ "toolName": name }])
            } else {
                serde_json::json!([])
            };
            serde_json::json!({
                "continue": true,
                "suppressOutput": true,
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedPermissions": [{
                            "type": "addRules",
                            "destination": "session",
                            "rules": rules,
                            "behavior": "allow"
                        }]
                    }
                }
            }).to_string()
        }
        "auto_approve" => {
            serde_json::json!({
                "continue": true,
                "suppressOutput": true,
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": {
                        "behavior": "allow",
                        "updatedPermissions": [{
                            "type": "setMode",
                            "destination": "session",
                            "mode": "bypassPermissions"
                        }]
                    }
                }
            }).to_string()
        }
        _ => return Err(format!("Unknown decision: {}", decision)),
    };

    let tx = {
        let mut map = state.pending_permissions.lock().map_err(|e| e.to_string())?;
        map.remove(&session_id)
    };

    if let Some(tx) = tx {
        tx.send(response_json).map_err(|_| "Failed to send permission response".to_string())?;
        log::info!("[resolve_permission] sent '{}' for session={}", decision, &session_id[..session_id.len().min(8)]);
    } else {
        log::warn!("[resolve_permission] no pending permission for session={}", &session_id[..session_id.len().min(8)]);
    }

    Ok(())
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ClaudeDailyStats {
    date: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    cache_write_tokens: u64,
    messages: u64,
    sessions: u64,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
struct ClaudeStats {
    #[serde(rename = "totalInputTokens")]
    total_input_tokens: u64,
    #[serde(rename = "totalOutputTokens")]
    total_output_tokens: u64,
    #[serde(rename = "totalCacheReadTokens")]
    total_cache_read_tokens: u64,
    #[serde(rename = "totalCacheWriteTokens")]
    total_cache_write_tokens: u64,
    #[serde(rename = "totalMessages")]
    total_messages: u64,
    #[serde(rename = "totalSessions")]
    total_sessions: u64,
    #[serde(rename = "dailyStats")]
    daily_stats: Vec<ClaudeDailyStats>,
    model: String,
}

#[tauri::command]
async fn get_claude_stats(source: Option<String>) -> Result<ClaudeStats, String> {
    let source = source.unwrap_or_default().to_ascii_lowercase();
    let jsonl_files = match source.as_str() {
        "codex" => collect_codex_session_jsonl_files(),
        // Cursor hook transcripts are currently parsed through Claude-style JSONL.
        // Keep Cursor aligned with the Claude parser until a dedicated Cursor
        // transcript index is introduced.
        "cursor" | "cc" | "claude" => collect_claude_project_jsonl_files(),
        _ => {
            let mut files = collect_claude_project_jsonl_files();
            files.extend(collect_codex_session_jsonl_files());
            files
        }
    };
    if jsonl_files.is_empty() {
        return Ok(ClaudeStats {
            total_input_tokens: 0, total_output_tokens: 0,
            total_cache_read_tokens: 0, total_cache_write_tokens: 0,
            total_messages: 0, total_sessions: 0,
            daily_stats: vec![], model: String::new(),
        });
    }

    let mut daily_map: std::collections::BTreeMap<String, ClaudeDailyStats> = std::collections::BTreeMap::new();
    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_cache_write = 0u64;
    let mut total_messages = 0u64;
    let mut total_sessions = 0u64;
    let mut model = String::new();

    // Only count last 14 days
    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::days(14);
    let cutoff_str = cutoff.format("%Y-%m-%d").to_string();

    for path in jsonl_files {
        // Use file modification time to skip old files quickly.
        let modified_day = path.metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .map(|t| {
                let dt: chrono::DateTime<chrono::Utc> = t.into();
                dt
            });
        if let Some(modified) = modified_day {
            if modified < cutoff {
                continue;
            }
        }

        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let mut session_counted = false;
        let mut session_day: Option<String> = None;

        // Codex logs cumulative token totals on each token_count event.
        // We convert cumulative totals into per-event deltas to avoid
        // double-counting repeated snapshots.
        let mut prev_codex_total_input: Option<u64> = None;
        let mut prev_codex_total_output: Option<u64> = None;
        let mut prev_codex_total_cached_input: Option<u64> = None;

        for line in content.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let parsed: serde_json::Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => continue,
            };

            let line_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

            // Claude Code format: assistant entries carry usage directly.
            if line_type == "assistant" {
                let msg = match parsed.get("message") {
                    Some(m) => m,
                    None => continue,
                };
                let usage = match msg.get("usage") {
                    Some(u) => u,
                    None => continue,
                };

                let date = parsed.get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|ts| ts.get(..10))
                    .unwrap_or("")
                    .to_string();
                if date < cutoff_str {
                    continue;
                }

                let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let output = usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let cache_read = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let cache_write = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);

                if model.is_empty() {
                    if let Some(m) = msg.get("model").and_then(|v| v.as_str()) {
                        model = m.to_string();
                    }
                }

                total_input += input;
                total_output += output;
                total_cache_read += cache_read;
                total_cache_write += cache_write;
                total_messages += 1;

                if !session_counted {
                    session_counted = true;
                    total_sessions += 1;
                }
                if session_day.is_none() && !date.is_empty() {
                    session_day = Some(date.clone());
                }

                if !date.is_empty() {
                    let entry = daily_map.entry(date.clone()).or_insert_with(|| ClaudeDailyStats {
                        date: date.clone(),
                        input_tokens: 0, output_tokens: 0,
                        cache_read_tokens: 0, cache_write_tokens: 0,
                        messages: 0, sessions: 0,
                    });
                    entry.input_tokens += input;
                    entry.output_tokens += output;
                    entry.cache_read_tokens += cache_read;
                    entry.cache_write_tokens += cache_write;
                    entry.messages += 1;
                }
                continue;
            }

            // Codex format metadata.
            if line_type == "session_meta" && model.is_empty() {
                if let Some(m) = parsed.get("payload")
                    .and_then(|p| p.get("model"))
                    .and_then(|v| v.as_str()) {
                    model = m.to_string();
                } else if let Some(provider) = parsed.get("payload")
                    .and_then(|p| p.get("model_provider"))
                    .and_then(|v| v.as_str()) {
                    model = provider.to_string();
                }
                continue;
            }

            // Codex format usage: event_msg -> payload.type=token_count -> info.total_token_usage.
            if line_type == "event_msg" && parsed.get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|v| v.as_str()) == Some("token_count") {
                let total_usage = match parsed.get("payload")
                    .and_then(|p| p.get("info"))
                    .and_then(|i| i.get("total_token_usage")) {
                    Some(v) => v,
                    None => continue,
                };

                let total_input_now = total_usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let total_output_now = total_usage.get("output_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
                let total_cached_now = total_usage.get("cached_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);

                let delta_input = match prev_codex_total_input {
                    Some(prev) => total_input_now.saturating_sub(prev),
                    None => total_input_now,
                };
                let delta_output = match prev_codex_total_output {
                    Some(prev) => total_output_now.saturating_sub(prev),
                    None => total_output_now,
                };
                let delta_cached = match prev_codex_total_cached_input {
                    Some(prev) => total_cached_now.saturating_sub(prev),
                    None => total_cached_now,
                };

                prev_codex_total_input = Some(total_input_now);
                prev_codex_total_output = Some(total_output_now);
                prev_codex_total_cached_input = Some(total_cached_now);

                // Same cumulative snapshot can be emitted multiple times.
                if delta_input == 0 && delta_output == 0 && delta_cached == 0 {
                    continue;
                }

                let date = parsed.get("timestamp")
                    .and_then(|v| v.as_str())
                    .and_then(|ts| ts.get(..10))
                    .unwrap_or("")
                    .to_string();
                if date < cutoff_str {
                    continue;
                }

                total_input += delta_input;
                total_output += delta_output;
                total_cache_read += delta_cached;
                total_messages += 1;

                if !session_counted {
                    session_counted = true;
                    total_sessions += 1;
                }
                if session_day.is_none() && !date.is_empty() {
                    session_day = Some(date.clone());
                }

                if !date.is_empty() {
                    let entry = daily_map.entry(date.clone()).or_insert_with(|| ClaudeDailyStats {
                        date: date.clone(),
                        input_tokens: 0, output_tokens: 0,
                        cache_read_tokens: 0, cache_write_tokens: 0,
                        messages: 0, sessions: 0,
                    });
                    entry.input_tokens += delta_input;
                    entry.output_tokens += delta_output;
                    entry.cache_read_tokens += delta_cached;
                    entry.messages += 1;
                }
            }
        }

        // Count one session per day.
        if session_counted {
            let day = session_day.or_else(|| modified_day.map(|d| d.format("%Y-%m-%d").to_string()));
            if let Some(day_str) = day {
                let entry = daily_map.entry(day_str.clone()).or_insert_with(|| ClaudeDailyStats {
                    date: day_str.clone(),
                    input_tokens: 0, output_tokens: 0,
                    cache_read_tokens: 0, cache_write_tokens: 0,
                    messages: 0, sessions: 0,
                });
                entry.sessions += 1;
            }
        }
    }

    // Fill in missing days in the 14-day range
    let mut daily_stats: Vec<ClaudeDailyStats> = Vec::new();
    for i in (0..14).rev() {
        let day = (now - chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
        if let Some(entry) = daily_map.remove(&day) {
            daily_stats.push(entry);
        } else {
            daily_stats.push(ClaudeDailyStats {
                date: day, input_tokens: 0, output_tokens: 0,
                cache_read_tokens: 0, cache_write_tokens: 0,
                messages: 0, sessions: 0,
            });
        }
    }

    Ok(ClaudeStats {
        total_input_tokens: total_input,
        total_output_tokens: total_output,
        total_cache_read_tokens: total_cache_read,
        total_cache_write_tokens: total_cache_write,
        total_messages: total_messages,
        total_sessions: total_sessions,
        daily_stats,
        model,
    })
}

#[tauri::command]
async fn open_url(url: String) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    { std::process::Command::new("open").arg(&url).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "windows")]
    { std::process::Command::new("cmd").args(["/C", "start", "", &url]).spawn().map_err(|e| e.to_string())?; }
    #[cfg(target_os = "linux")]
    { std::process::Command::new("xdg-open").arg(&url).spawn().map_err(|e| e.to_string())?; }
    Ok(())
}

/// Activate a macOS app by its name (e.g. "Feishu", "Telegram", "Lark").
#[tauri::command]
async fn activate_app(app_name: String) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(r#"tell application "{}" to activate"#, app_name);
        std::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(|e| e.to_string())?;
        Ok(format!("Activated {}", app_name))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err(format!("activate_app not supported on this platform"))
    }
}

fn cwd_matches_workspace_root(cwd: &str, workspace_root: &str) -> bool {
    if cwd.is_empty() || workspace_root.is_empty() {
        return false;
    }
    if cwd == workspace_root {
        return true;
    }
    cwd.strip_prefix(workspace_root)
        .map(|rest| rest.starts_with('/') || rest.starts_with('\\'))
        .unwrap_or(false)
}

fn cursor_workspace_name_from_path(path_str: &str) -> String {
    std::path::Path::new(path_str)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string()
}

fn read_local_http_response(port: u16, request: String) -> Option<(u16, String)> {
    use std::io::{Read, Write};
    use std::net::{Shutdown, SocketAddr, TcpStream};

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(120))
        .ok()?;
    let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
    let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(200)));
    stream.write_all(request.as_bytes()).ok()?;
    let _ = stream.shutdown(Shutdown::Write);

    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).ok()?;
    if buf.is_empty() {
        return None;
    }

    let response = String::from_utf8_lossy(&buf);
    let (headers, body) = response.split_once("\r\n\r\n")?;
    let status = headers.lines().next()?.split_whitespace().nth(1)?.parse::<u16>().ok()?;

    let is_chunked = headers.to_ascii_lowercase().contains("transfer-encoding: chunked");
    let decoded_body = if is_chunked {
        decode_chunked_body(body)
    } else {
        body.to_string()
    };

    Some((status, decoded_body))
}

fn decode_chunked_body(raw: &str) -> String {
    let mut result = String::new();
    let mut remaining = raw;
    loop {
        let remaining_trimmed = remaining.trim_start_matches("\r\n");
        let (size_str, rest) = match remaining_trimmed.split_once("\r\n") {
            Some(pair) => pair,
            None => break,
        };
        let chunk_size = match usize::from_str_radix(size_str.trim(), 16) {
            Ok(s) => s,
            Err(_) => break,
        };
        if chunk_size == 0 {
            break;
        }
        let chunk_data: String = rest.chars().take(chunk_size).collect();
        result.push_str(&chunk_data);
        remaining = &rest[chunk_data.len().min(rest.len())..];
    }
    result
}

fn get_cursor_window_meta(port: u16) -> Option<CursorWindowMeta> {
    let request = format!(
        "GET /window-meta HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
    );
    let (status, body) = read_local_http_response(port, request)?;
    if status != 200 {
        return None;
    }
    serde_json::from_str::<CursorWindowMeta>(&body).ok()
}

fn post_cursor_window_action(port: u16, path: &str, body: &str) -> bool {
    let request = format!(
        "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body,
    );
    read_local_http_response(port, request)
        .map(|(status, _)| (200..300).contains(&status))
        .unwrap_or(false)
}

fn resolve_cursor_window_binding(
    cwd: &str,
    existing_port: Option<u16>,
    existing_native_handle: Option<&str>,
) -> Option<CursorWindowBinding> {
    #[derive(Debug)]
    struct Candidate {
        port: u16,
        workspace_root: String,
        workspace_name: String,
        native_handle: Option<String>,
        score: usize,
        focused: bool,
        keep_existing: bool,
        handle_match: bool,
    }

    log::info!("[cursor_bind_resolve] cwd={} existing_port={:?} existing_handle={:?}",
        cwd, existing_port, existing_native_handle);

    let mut candidates: Vec<Candidate> = Vec::new();
    for port in 23456..=23460u16 {
        let meta = match get_cursor_window_meta(port) {
            Some(meta) => meta,
            None => continue,
        };

        log::info!("[cursor_bind_resolve] port={} meta: focused={} workspace_name={} roots={:?} nativeHandle={:?}",
            meta.port, meta.focused, meta.workspace_name, meta.workspace_roots, meta.native_handle);

        let mut best_root: Option<String> = None;
        let mut best_score: usize = 0;
        for root in &meta.workspace_roots {
            if cwd_matches_workspace_root(cwd, root) {
                let score = root.len();
                if score >= best_score {
                    best_score = score;
                    best_root = Some(root.clone());
                }
            }
        }

        if let Some(workspace_root) = best_root {
            let handle_match = match (existing_native_handle, &meta.native_handle) {
                (Some(existing), Some(current)) => existing == current,
                _ => false,
            };
            candidates.push(Candidate {
                port: meta.port,
                workspace_root,
                workspace_name: if meta.workspace_name.is_empty() {
                    cursor_workspace_name_from_path(cwd)
                } else {
                    meta.workspace_name
                },
                native_handle: meta.native_handle,
                score: best_score,
                focused: meta.focused,
                keep_existing: existing_port == Some(meta.port),
                handle_match,
            });
        }
    }

    log::info!("[cursor_bind_resolve] {} candidates: {:?}",
        candidates.len(), candidates.iter().map(|c| format!("port={} score={} focused={} handle_match={} keep_existing={} handle={:?}",
            c.port, c.score, c.focused, c.handle_match, c.keep_existing, c.native_handle)).collect::<Vec<_>>());

    // If we have a native handle match, that wins unconditionally.
    if let Some(idx) = candidates.iter().position(|c| c.handle_match) {
        let c = &candidates[idx];
        log::info!("[cursor_bind_resolve] → native handle match: port={}", c.port);
        return Some(CursorWindowBinding {
            port: c.port,
            workspace_root: c.workspace_root.clone(),
            workspace_name: c.workspace_name.clone(),
            native_handle: c.native_handle.clone(),
        });
    }

    // Stick with existing bound port if still valid.
    if let Some(ep) = existing_port {
        if let Some(c) = candidates.iter().find(|c| c.port == ep) {
            log::info!("[cursor_bind_resolve] → keeping existing port={}", ep);
            return Some(CursorWindowBinding {
                port: c.port,
                workspace_root: c.workspace_root.clone(),
                workspace_name: c.workspace_name.clone(),
                native_handle: c.native_handle.clone(),
            });
        }
    }

    candidates.sort_by(|a, b| {
        b.score.cmp(&a.score)
            .then_with(|| b.focused.cmp(&a.focused))
            .then_with(|| a.port.cmp(&b.port))
    });

    let best = candidates.first()?;
    log::info!("[cursor_bind_resolve] → best candidate: port={} score={} focused={}", best.port, best.score, best.focused);

    Some(CursorWindowBinding {
        port: best.port,
        workspace_root: best.workspace_root.clone(),
        workspace_name: best.workspace_name.clone(),
        native_handle: best.native_handle.clone(),
    })
}

#[cfg(target_os = "macos")]
fn check_accessibility_permission() -> bool {
    extern "C" {
        fn AXIsProcessTrusted() -> bool;
    }
    unsafe { AXIsProcessTrusted() }
}

#[cfg(not(target_os = "macos"))]
fn check_accessibility_permission() -> bool {
    true
}

#[tauri::command]
async fn check_ax_permission() -> Result<bool, String> {
    Ok(check_accessibility_permission())
}

#[tauri::command]
async fn request_ax_permission() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        use std::ffi::c_void;

        #[link(name = "CoreFoundation", kind = "framework")]
        extern "C" {
            fn CFStringCreateWithCString(alloc: *const c_void, c_str: *const u8, encoding: u32) -> *const c_void;
            fn CFDictionaryCreate(
                alloc: *const c_void, keys: *const *const c_void, values: *const *const c_void,
                count: isize, key_cbs: *const c_void, val_cbs: *const c_void,
            ) -> *const c_void;
            fn CFRelease(cf: *const c_void);
            static kCFTypeDictionaryKeyCallBacks: c_void;
            static kCFTypeDictionaryValueCallBacks: c_void;
            static kCFBooleanTrue: *const c_void;
        }

        #[link(name = "ApplicationServices", kind = "framework")]
        extern "C" {
            fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
        }

        unsafe {
            let key = CFStringCreateWithCString(
                std::ptr::null(),
                b"AXTrustedCheckOptionPrompt\0".as_ptr(),
                0x08000100, // kCFStringEncodingUTF8
            );
            let keys = [key];
            let values = [kCFBooleanTrue];
            let dict = CFDictionaryCreate(
                std::ptr::null(), keys.as_ptr(), values.as_ptr(), 1,
                &kCFTypeDictionaryKeyCallBacks, &kCFTypeDictionaryValueCallBacks,
            );
            AXIsProcessTrustedWithOptions(dict);
            CFRelease(dict);
            CFRelease(key);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn activate_cursor_workspace_window(workspace_name: &str) {
    let ax_ok = check_accessibility_permission();
    let escaped_workspace = workspace_name.replace('\\', "\\\\").replace('"', "\\\"");

    let script = if escaped_workspace.is_empty() {
        r#"tell application "Cursor" to activate"#.to_string()
    } else if ax_ok {
        format!(
            r#"tell application "System Events"
    set cursorProcs to every process whose name is "Cursor"
    if (count of cursorProcs) is 0 then
        tell application "Cursor" to activate
        return
    end if
    set cursorProc to item 1 of cursorProcs
    set matched to false
    repeat with w in windows of cursorProc
        try
            if name of w contains "{workspace}" then
                perform action "AXRaise" of w
                set frontmost of cursorProc to true
                set matched to true
                exit repeat
            end if
        end try
    end repeat
    if not matched then
        set frontmost of cursorProc to true
    end if
end tell"#,
            workspace = escaped_workspace,
        )
    } else {
        // No AX permission — use Cursor's own AppleScript dictionary
        // to find and raise the matching window by index, which does
        // not require System Events / Accessibility permission.
        format!(
            r#"tell application "Cursor"
    activate
    set matched to false
    repeat with i from 1 to count of windows
        if name of window i contains "{workspace}" then
            set index of window i to 1
            set matched to true
            exit repeat
        end if
    end repeat
end tell"#,
            workspace = escaped_workspace,
        )
    };

    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output();
}

/// Focus the Cursor terminal tab for a given session.
/// Cursor hook payloads do not contain a stable terminal pid. The pid we see in
/// events changes from one hook invocation to the next, so it is not reliable
/// for jump-back. Instead we bind each session to a specific Cursor window by:
/// 1. Matching the session cwd against window metadata exposed by the extension
///    (`/window-meta` on ports 23456-23460).
/// 2. Reusing that bound port on click so we target one Cursor window instead
///    of broadcasting to all windows and hoping the right one wins.
/// 3. Raising the matching Cursor window on macOS by workspace name.
#[tauri::command]
async fn focus_cursor_terminal(session_id: String, state: tauri::State<'_, ClaudeState>) -> Result<String, String> {
    log::info!("[focus_cursor] called for session={}", &session_id[..session_id.len().min(8)]);

    let ax_ok = check_accessibility_permission();
    log::info!("[focus_cursor] accessibility_permission={}", ax_ok);

    let (cwd, existing_port, existing_workspace_root, existing_workspace_name, existing_native_handle) = {
        let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        match sessions.get(&session_id) {
            Some(s) => (
                s.cwd.clone(),
                s.cursor_port,
                s.cursor_workspace_root.clone(),
                s.cursor_workspace_name.clone(),
                s.cursor_native_handle.clone(),
            ),
            None => (String::new(), None, None, None, None),
        }
    };

    let resolved_binding = if !cwd.is_empty() {
        resolve_cursor_window_binding(&cwd, existing_port, existing_native_handle.as_deref())
    } else {
        None
    };

    if let Some(binding) = &resolved_binding {
        if let Ok(mut sessions) = state.sessions.lock() {
            if let Some(session) = sessions.get_mut(&session_id) {
                session.cursor_port = Some(binding.port);
                session.cursor_workspace_root = Some(binding.workspace_root.clone());
                session.cursor_workspace_name = Some(binding.workspace_name.clone());
                session.cursor_native_handle = binding.native_handle.clone();
            }
        }
    }

    let port = resolved_binding.as_ref().map(|b| b.port).or(existing_port);
    let workspace_name = resolved_binding.as_ref().map(|b| b.workspace_name.clone())
        .or(existing_workspace_name)
        .or_else(|| existing_workspace_root.as_deref().map(cursor_workspace_name_from_path))
        .or_else(|| (!cwd.is_empty()).then(|| cursor_workspace_name_from_path(&cwd)))
        .unwrap_or_default();

    log::info!("[focus_cursor] session={} cwd={} port={:?} workspace_name={}",
        &session_id[..session_id.len().min(8)], cwd, port, workspace_name);

    #[cfg(target_os = "macos")]
    activate_cursor_workspace_window(&workspace_name);

    if let Some(port) = port {
        let focused = post_cursor_window_action(port, "/focus-window", "{}");
        log::info!("[focus_cursor] POST /focus-window to port {} → {}", port, focused);
        if focused {
            return Ok(format!("Focused Cursor window on port {}", port));
        }
        return Ok(format!("Activated Cursor window but /focus-window failed on port {}", port));
    }

    #[cfg(target_os = "macos")]
    activate_cursor_workspace_window(&workspace_name);

    Ok("Activated Cursor without a bound window".to_string())
}

/// Jump to the terminal running a Claude Code session.
/// Walks the parent process chain from the given PID to identify the terminal app,
/// then uses AppleScript (macOS) to activate and focus the matching window.
#[tauri::command]
async fn jump_to_claude_terminal(session_id: String, state: tauri::State<'_, ClaudeState>) -> Result<String, String> {
    let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    let session = sessions.get(&session_id).ok_or("Session not found")?;
    let cwd = session.cwd.clone();
    let terminal_id = session.terminal_id.clone();
    let pid = session.pid;
    let source = session.source.clone();
    drop(sessions);

    #[cfg(target_os = "macos")]
    {
        let try_activate_app = |app_name: &str| -> bool {
            let script = format!(r#"tell application "{}" to activate"#, app_name.replace('"', "\\\""));
            if std::process::Command::new("osascript")
                .args(["-e", &script])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return true;
            }
            std::process::Command::new("open")
                .args(["-a", app_name])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
        };

        // Codex sessions should jump to the Codex app directly.
        // Do not route through Ghostty first; that causes the "terminal flash"
        // and may still require manual dock clicks to bring Codex frontmost.
        if source == "codex" {
            for app_name in ["Codex", "Code"] {
                if try_activate_app(app_name) {
                    return Ok(format!("Activated {}", app_name));
                }
            }
            // If Codex app activation fails (e.g. not installed as app bundle),
            // continue with terminal-based fallback paths below.
        }

        // Fast path: if we have a Ghostty terminal ID from hooks, jump directly
        // to that tab without depending on PID ancestry checks.
        if let Some(tid_raw) = terminal_id.as_deref() {
            if !tid_raw.is_empty() {
                let escaped_tid = tid_raw.replace('\\', "\\\\").replace('"', "\\\"");
                let script = format!(
                    r#"tell application "Ghostty"
    if not (it is running) then return ""
    set targetWindow to missing value
    set targetTab to missing value
    set targetTerminal to missing value
    repeat with aWindow in windows
        repeat with aTab in tabs of aWindow
            repeat with aTerminal in terminals of aTab
                try
                    if (id of aTerminal as text) is "{tid}" then
                        set targetWindow to aWindow
                        set targetTab to aTab
                        set targetTerminal to aTerminal
                        exit repeat
                    end if
                end try
            end repeat
            if targetTerminal is not missing value then exit repeat
        end repeat
        if targetTerminal is not missing value then exit repeat
    end repeat
    if targetTerminal is missing value then return ""
    activate
    delay 0.05
    if targetTab is not missing value then
        select tab targetTab
        delay 0.05
    end if
    if targetWindow is not missing value then
        set index of targetWindow to 1
    end if
    focus targetTerminal
    return "matched"
end tell"#,
                    tid = escaped_tid,
                );
                if let Ok(out) = std::process::Command::new("osascript").args(["-e", &script]).output() {
                    let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if out.status.success() && stdout == "matched" {
                        return Ok("Jumped to Ghostty".to_string());
                    }
                }

                // Some Ghostty builds may format terminal IDs slightly differently.
                // Retry with a prefix contains-match to avoid false negatives.
                let tid_prefix = &tid_raw[..tid_raw.len().min(8)];
                if !tid_prefix.is_empty() {
                    let escaped_prefix = tid_prefix.replace('\\', "\\\\").replace('"', "\\\"");
                    let fallback_script = format!(
                        r#"tell application "Ghostty"
    if not (it is running) then return ""
    set targetWindow to missing value
    set targetTab to missing value
    set targetTerminal to missing value
    repeat with aWindow in windows
        repeat with aTab in tabs of aWindow
            repeat with aTerminal in terminals of aTab
                try
                    if (id of aTerminal as text) contains "{prefix}" then
                        set targetWindow to aWindow
                        set targetTab to aTab
                        set targetTerminal to aTerminal
                        exit repeat
                    end if
                end try
            end repeat
            if targetTerminal is not missing value then exit repeat
        end repeat
        if targetTerminal is not missing value then exit repeat
    end repeat
    if targetTerminal is missing value then return ""
    activate
    delay 0.05
    if targetTab is not missing value then
        select tab targetTab
        delay 0.05
    end if
    if targetWindow is not missing value then
        set index of targetWindow to 1
    end if
    focus targetTerminal
    return "matched"
end tell"#,
                        prefix = escaped_prefix,
                    );
                    if let Ok(out) = std::process::Command::new("osascript").args(["-e", &fallback_script]).output() {
                        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                        if out.status.success() && stdout == "matched" {
                            return Ok("Jumped to Ghostty".to_string());
                        }
                    }
                }
            }
        }

        let pid = if let Some(p) = pid {
            p
        } else if source == "codex" {
            for app_name in ["Codex", "Ghostty", "Cursor"] {
                if try_activate_app(app_name) {
                    return Ok(format!("Activated {}", app_name));
                }
            }
            return Err("No PID tracked for this Codex session".to_string());
        } else {
            return Err("No PID tracked for this session".to_string());
        };
        // Walk parent process chain to find the terminal application
        let terminal_app = find_terminal_app_for_pid(pid);

        let tty = get_tty_for_pid(pid);
        let escaped_cwd = cwd.replace('\\', "\\\\").replace('"', "\\\"");
        let escaped_tty = tty.as_deref().unwrap_or("").replace('\\', "\\\\").replace('"', "\\\"");
        let escaped_sid = session_id.replace('\\', "\\\\").replace('"', "\\\"");

        match terminal_app.as_deref() {
            Some("Ghostty" | "ghostty") => {
                // Matching strategy (most → least precise):
                // 0. Stored terminal `id` captured at session start
                // 1. Session ID substring in tab title
                // 2. Working directory (ambiguous if multiple tabs share CWD)
                //
                // IMPORTANT: do NOT `activate` before matching — that would
                // bring Ghostty to front showing whatever tab was last
                // selected, giving a wrong-tab flash.
                let escaped_tid = terminal_id.as_deref().unwrap_or("").replace('\\', "\\\\").replace('"', "\\\"");
                let script = format!(
                    r#"tell application "Ghostty"
    if not (it is running) then return ""

    set targetWindow to missing value
    set targetTab to missing value
    set targetTerminal to missing value

    -- Pass 0: match by stored terminal id (most precise)
    if "{tid}" is not "" then
        repeat with aWindow in windows
            repeat with aTab in tabs of aWindow
                repeat with aTerminal in terminals of aTab
                    try
                        if (id of aTerminal as text) is "{tid}" then
                            set targetWindow to aWindow
                            set targetTab to aTab
                            set targetTerminal to aTerminal
                            exit repeat
                        end if
                    end try
                end repeat
                if targetTerminal is not missing value then exit repeat
            end repeat
            if targetTerminal is not missing value then exit repeat
        end repeat
    end if

    -- Pass 1: match by session ID in tab title
    if targetTerminal is missing value and "{sid}" is not "" then
        repeat with aWindow in windows
            repeat with aTab in tabs of aWindow
                repeat with aTerminal in terminals of aTab
                    try
                        if (name of aTerminal as text) contains "{sid_prefix}" then
                            set targetWindow to aWindow
                            set targetTab to aTab
                            set targetTerminal to aTerminal
                            exit repeat
                        end if
                    end try
                end repeat
                if targetTerminal is not missing value then exit repeat
            end repeat
            if targetTerminal is not missing value then exit repeat
        end repeat
    end if

    -- Pass 2: match by working directory (least precise)
    if targetTerminal is missing value and "{cwd}" is not "" then
        repeat with aWindow in windows
            repeat with aTab in tabs of aWindow
                repeat with aTerminal in terminals of aTab
                    try
                        if (working directory of aTerminal as text) is "{cwd}" then
                            set targetWindow to aWindow
                            set targetTab to aTab
                            set targetTerminal to aTerminal
                            exit repeat
                        end if
                    end try
                end repeat
                if targetTerminal is not missing value then exit repeat
            end repeat
            if targetTerminal is not missing value then exit repeat
        end repeat
    end if

    if targetTerminal is missing value then return ""

    -- Activate AFTER matching so the correct tab is shown immediately.
    activate
    delay 0.05
    if targetTab is not missing value then
        select tab targetTab
        delay 0.05
    end if
    if targetWindow is not missing value then
        set index of targetWindow to 1
    end if
    focus targetTerminal
    return "matched"
end tell"#,
                    tid = escaped_tid,
                    sid = escaped_sid,
                    sid_prefix = &escaped_sid[..escaped_sid.len().min(12)],
                    cwd = escaped_cwd,
                );
                let _ = std::process::Command::new("osascript")
                    .args(["-e", &script])
                    .output();
                Ok("Jumped to Ghostty".to_string())
            }
            Some("iTerm" | "iTerm2" | "iterm2") => {
                if !escaped_tty.is_empty() {
                    let script = format!(
                        r#"tell application "iTerm2"
    activate
    repeat with aWindow in windows
        repeat with aTab in tabs of aWindow
            repeat with aSession in sessions of aTab
                if tty of aSession is "{tty}" then
                    select aSession
                    tell aWindow to select
                    return "found"
                end if
            end repeat
        end repeat
    end repeat
end tell"#,
                        tty = escaped_tty
                    );
                    let _ = std::process::Command::new("osascript")
                        .args(["-e", &script])
                        .output();
                } else {
                    let _ = std::process::Command::new("osascript")
                        .args(["-e", r#"tell application "iTerm2" to activate"#])
                        .output();
                }
                Ok("Jumped to iTerm2".to_string())
            }
            Some("Terminal" | "Apple_Terminal") => {
                if !escaped_tty.is_empty() {
                    let script = format!(
                        r#"tell application "Terminal"
    activate
    repeat with aWindow in windows
        repeat with aTab in tabs of aWindow
            if tty of aTab is "{tty}" then
                set selected tab of aWindow to aTab
                set index of aWindow to 1
                return "found"
            end if
        end repeat
    end repeat
end tell"#,
                        tty = escaped_tty
                    );
                    let _ = std::process::Command::new("osascript")
                        .args(["-e", &script])
                        .output();
                } else {
                    let _ = std::process::Command::new("osascript")
                        .args(["-e", r#"tell application "Terminal" to activate"#])
                        .output();
                }
                Ok("Jumped to Terminal.app".to_string())
            }
            Some("Cursor") => {
                let _ = std::process::Command::new("open")
                    .args(["-a", "Cursor"])
                    .output();
                Ok("Jumped to Cursor".to_string())
            }
            Some(app_name) => {
                let script = format!(
                    r#"tell application "{}" to activate"#,
                    app_name.replace('"', "\\\"")
                );
                let _ = std::process::Command::new("osascript")
                    .args(["-e", &script])
                    .output();
                Ok(format!("Jumped to {}", app_name))
            }
            None => {
                if source == "codex" {
                    for app_name in ["Codex", "Ghostty", "Cursor"] {
                        if try_activate_app(app_name) {
                            return Ok(format!("Activated {}", app_name));
                        }
                    }
                }
                if !cwd.is_empty() {
                    let _ = std::process::Command::new("open").arg(&cwd).spawn();
                }
                Err("Could not identify the terminal application".to_string())
            }
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        // On Windows/Linux, try to open the working directory
        if !cwd.is_empty() {
            let _ = std::process::Command::new("open").arg(&cwd).spawn();
        }
        Ok("Opened working directory".to_string())
    }
}

/// Walk the parent process chain to find the terminal app name.
/// Returns the process name of the first recognized terminal emulator.
#[cfg(target_os = "macos")]
fn find_terminal_app_for_pid(pid: u32) -> Option<String> {
    let known_terminals = [
        "Ghostty", "ghostty",
        "iTerm2", "iterm2",
        "Terminal", "Apple_Terminal",
        "WezTerm", "wezterm-gui",
        "Warp", "warp",
        "kitty",
        "Alacritty", "alacritty",
        "kaku",
        "Cursor",
        "Codex", "codex",
    ];

    let mut current_pid = pid;
    for _ in 0..20 {
        let output = std::process::Command::new("ps")
            .args(["-p", &current_pid.to_string(), "-o", "ppid=,comm="])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if line.is_empty() { return None; }

        let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
        if parts.len() < 2 { return None; }

        let ppid: u32 = parts[0].trim().parse().ok()?;
        let comm = parts[1].trim();
        // Extract basename from full path
        let name = comm.rsplit('/').next().unwrap_or(comm);

        if known_terminals.iter().any(|t| name.eq_ignore_ascii_case(t)) {
            return Some(name.to_string());
        }

        if ppid <= 1 { return None; }
        current_pid = ppid;
    }
    None
}

/// Get the TTY device path for a given PID.
#[cfg(target_os = "macos")]
fn get_tty_for_pid(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "tty="])
        .output()
        .ok()?;
    let tty = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if tty.is_empty() || tty == "??" { return None; }
    // Normalize: ps outputs like "ttys003", convert to "/dev/ttys003"
    if tty.starts_with("/dev/") {
        Some(tty)
    } else {
        Some(format!("/dev/{}", tty))
    }
}

/// Check for updates by fetching the version manifest from the official website.
/// The manifest is a static JSON file hosted on Vercel at /update/latest.json,
/// which is manually updated on each release — giving us full control over
/// when users see an update prompt (independent of GitHub Releases).
///
/// Expected manifest format:
///   {
///     "version": "1.6.0",
///     "notes": "...",
///     "platforms": {
///       "macos":   { "url": "https://github.com/.../oc-claw_1.6.0_aarch64.dmg" },
///       "windows": { "url": "https://github.com/.../oc-claw_1.6.0_x64-setup.exe" }
///     }
///   }
///
/// Legacy format (single "url" field) is still supported for backward compatibility.
fn normalize_lang_tag(lang: &str) -> String {
    lang.trim().to_lowercase().replace('_', "-")
}

fn pick_localized_notes(notes_i18n: &serde_json::Value, lang: Option<&str>) -> Option<String> {
    let obj = notes_i18n.as_object()?;
    let mut keys: Vec<String> = Vec::new();
    if let Some(raw) = lang {
        let normalized = normalize_lang_tag(raw);
        if !normalized.is_empty() {
            keys.push(normalized.clone());
            if let Some((prefix, _)) = normalized.split_once('-') {
                if !prefix.is_empty() {
                    keys.push(prefix.to_string());
                }
            }
        }
    }
    keys.push("en".to_string());
    keys.push("zh".to_string());
    for key in keys {
        if let Some(value) = obj.get(&key).and_then(|v| v.as_str()) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    for value in obj.values() {
        if let Some(text) = value.as_str() {
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[tauri::command]
async fn check_for_update(app: tauri::AppHandle, lang: Option<String>) -> Result<serde_json::Value, String> {
    let current = app.config().version.clone().unwrap_or_default();

    let update_url = if cfg!(debug_assertions) {
        "http://[::1]:4321/update/latest.json"
    } else {
        "https://www.oc-claw.ai/update/latest.json"
    };
    log::info!("[update] checking {} (current={})", update_url, current);
    let mut client_builder = reqwest::Client::builder()
        .user_agent("oc-claw");
    if cfg!(debug_assertions) {
        client_builder = client_builder.no_proxy();
    }
    let client = client_builder
        .build()
        .map_err(|e| format!("client build error: {e}"))?;
    let resp = client
        .get(update_url)
        .send()
        .await
        .map_err(|e| { log::warn!("[update] fetch error: {e}"); format!("fetch error: {e}") })?;
    if !resp.status().is_success() {
        let msg = format!("update check failed: HTTP {}", resp.status());
        log::warn!("[update] {msg}");
        return Err(msg);
    }
    let json: serde_json::Value = resp.json().await
        .map_err(|e| { log::warn!("[update] json parse error: {e}"); format!("json parse error: {e}") })?;

    // Per-platform update: each platform has its own version and url under
    // json["platforms"]["<platform>"]["version"] and ["url"].
    // Falls back to legacy top-level json["version"] / json["url"] for compatibility.
    #[cfg(windows)]
    let platform_key = "windows";
    #[cfg(target_os = "macos")]
    let platform_key = "macos";
    #[cfg(not(any(windows, target_os = "macos")))]
    let platform_key = "linux";

    let platform = &json["platforms"][platform_key];
    let latest = platform["version"].as_str()
        .or_else(|| json["version"].as_str())
        .unwrap_or("");
    let url = platform["url"].as_str()
        .or_else(|| json["url"].as_str())
        .unwrap_or("");
    let notes = pick_localized_notes(&platform["notes_i18n"], lang.as_deref())
        .or_else(|| pick_localized_notes(&json["notes_i18n"], lang.as_deref()))
        .or_else(|| platform["notes"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .or_else(|| json["notes"].as_str().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_default();
    let has_update = version_cmp(latest, &current);
    log::info!("[update] platform={} latest={} current={} hasUpdate={}", platform_key, latest, current, has_update);

    Ok(serde_json::json!({
        "current": current,
        "latest": latest,
        "hasUpdate": has_update,
        "url": url,
        "notes": notes,
    }))
}

fn version_cmp(latest: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u32> {
        s.split('.').filter_map(|p| p.parse().ok()).collect()
    };
    let l = parse(latest);
    let c = parse(current);
    for i in 0..l.len().max(c.len()) {
        let lv = l.get(i).copied().unwrap_or(0);
        let cv = c.get(i).copied().unwrap_or(0);
        if lv > cv { return true; }
        if lv < cv { return false; }
    }
    false
}

fn format_update_bytes(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit_idx = 0usize;
    while value >= 1024.0 && unit_idx < UNITS.len() - 1 {
        value /= 1024.0;
        unit_idx += 1;
    }
    if unit_idx == 0 {
        format!("{} {}", bytes, UNITS[unit_idx])
    } else {
        format!("{value:.1} {}", UNITS[unit_idx])
    }
}

fn emit_update_progress(
    app: &tauri::AppHandle,
    stage: &str,
    progress: Option<u64>,
    downloaded_bytes: u64,
    total_bytes: Option<u64>,
    message: &str,
) {
    let _ = app.emit(
        "update-progress",
        serde_json::json!({
            "stage": stage,
            "progress": progress,
            "downloadedBytes": downloaded_bytes,
            "totalBytes": total_bytes,
            "message": message,
        }),
    );
}

/// Run the actual update: download the installer package, install, and relaunch.
/// On macOS: downloads DMG, runs a bash helper script to swap the .app bundle.
/// On Windows: downloads MSI/EXE, runs the installer silently.
/// The `dmg_url` is passed from the frontend (originally from the website manifest).
#[tauri::command]
async fn run_update(app: tauri::AppHandle, dmg_url: String) -> Result<(), String> {
    if dmg_url.is_empty() {
        return Err("No download URL provided".to_string());
    }
    let client = reqwest::Client::builder()
        .user_agent("oc-claw-updater")
        .build()
        .map_err(|e| format!("client build error: {e}"))?;
    emit_update_progress(&app, "preparing", Some(0), 0, None, "准备下载更新");
    let mut resp = client
        .get(&dmg_url)
        .send()
        .await
        .map_err(|e| format!("download request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("download failed: HTTP {}", resp.status()));
    }

    let total_bytes = resp.content_length();
    let stamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let work_dir = std::env::temp_dir().join(format!("oc-claw-update-{stamp}"));
    std::fs::create_dir_all(&work_dir).map_err(|e| format!("failed to create temp dir: {e}"))?;

    // Determine installer file extension based on URL and platform
    #[cfg(target_os = "macos")]
    let installer_filename = "oc-claw-update.dmg";
    #[cfg(target_os = "windows")]
    let installer_filename = if dmg_url.ends_with(".msi") { "oc-claw-update.msi" } else { "oc-claw-update.exe" };
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let installer_filename = "oc-claw-update";

    let dmg_path = work_dir.join(installer_filename);
    #[cfg(target_os = "macos")]
    let helper_path = work_dir.join("install-update.sh");
    let log_path = work_dir.join("install.log");

    let mut file = tokio::fs::File::create(&dmg_path)
        .await
        .map_err(|e| format!("failed to create temp file: {e}"))?;
    let mut downloaded_bytes = 0u64;
    let mut last_progress: Option<u64> = None;

    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| format!("download stream failed: {e}"))?
    {
        tokio::io::AsyncWriteExt::write_all(&mut file, &chunk)
            .await
            .map_err(|e| format!("failed to write temp file: {e}"))?;
        downloaded_bytes += chunk.len() as u64;
        let progress = total_bytes.map(|total| ((downloaded_bytes.saturating_mul(100)) / total.max(1)).min(100));
        if progress != last_progress {
            let message = if let Some(total) = total_bytes {
                format!(
                    "正在下载更新 {} / {}",
                    format_update_bytes(downloaded_bytes),
                    format_update_bytes(total)
                )
            } else {
                format!("正在下载更新 {}", format_update_bytes(downloaded_bytes))
            };
            emit_update_progress(&app, "downloading", progress, downloaded_bytes, total_bytes, &message);
            last_progress = progress;
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut file)
        .await
        .map_err(|e| format!("failed to flush temp file: {e}"))?;

    emit_update_progress(
        &app,
        "downloaded",
        Some(100),
        downloaded_bytes,
        total_bytes,
        "下载完成，准备安装更新",
    );

    // Platform-specific: spawn a detached installer helper
    #[cfg(target_os = "macos")]
    {
        // Spawn a detached helper that waits for the app to quit, then swaps the bundle.
        let script = format!(r#"#!/bin/bash
set -euo pipefail
PID="{pid}"
APP_BUNDLE="/Applications/oc-claw.app"
DMG_PATH="{dmg_path}"
LOG_PATH="{log_path}"
MOUNT_POINT=""

log() {{
  printf '[%s] %s\n' "$(date '+%Y-%m-%d %H:%M:%S')" "$1" >> "$LOG_PATH"
}}

cleanup() {{
  if [ -n "$MOUNT_POINT" ]; then
    hdiutil detach "$MOUNT_POINT" -quiet >/dev/null 2>&1 || true
  fi
}}

trap cleanup EXIT

log "Waiting for app pid $PID to exit"
for _ in $(seq 1 120); do
  if ! kill -0 "$PID" 2>/dev/null; then
    break
  fi
  sleep 0.5
done

if kill -0 "$PID" 2>/dev/null; then
  log "Timed out waiting for app to exit"
  exit 1
fi

if ! ATTACH_OUTPUT=$(hdiutil attach "$DMG_PATH" -nobrowse -readonly 2>&1); then
  log "$ATTACH_OUTPUT"
  exit 1
fi

MOUNT_POINT=$(printf '%s\n' "$ATTACH_OUTPUT" | awk 'match($0, /\/Volumes\/.*/) {{ print substr($0, RSTART); exit }}')
if [ -z "$MOUNT_POINT" ]; then
  log "Failed to determine DMG mount point"
  log "$ATTACH_OUTPUT"
  exit 1
fi

APP_PATH=""
for candidate in "$MOUNT_POINT"/*.app; do
  if [ -d "$candidate" ]; then
    APP_PATH="$candidate"
    break
  fi
done

if [ -z "$APP_PATH" ]; then
  log "No app bundle found in $MOUNT_POINT"
  /bin/ls -la "$MOUNT_POINT" >> "$LOG_PATH" 2>&1 || true
  exit 1
fi

log "Installing $APP_PATH"
rm -rf "$APP_BUNDLE"
ditto "$APP_PATH" "$APP_BUNDLE"
xattr -cr "$APP_BUNDLE" || true

log "Launching updated app"
open -n "$APP_BUNDLE"
"#,
            pid = std::process::id(),
            dmg_path = dmg_path.display(),
            log_path = log_path.display(),
        );
        std::fs::write(&helper_path, script).map_err(|e| format!("failed to write helper script: {e}"))?;
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&helper_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| format!("failed to chmod helper script: {e}"))?;
        }

        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("failed to open helper log: {e}"))?;
        let log_file_err = log_file
            .try_clone()
            .map_err(|e| format!("failed to clone helper log: {e}"))?;
        std::process::Command::new("bash")
            .arg(&helper_path)
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_file_err))
            .spawn()
            .map_err(|e| format!("failed to start installer helper: {e}"))?;
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows: spawn the downloaded installer (MSI or EXE) with silent flags.
        // The installer will handle replacing the old version and relaunching.
        let helper_path = work_dir.join("install-update.ps1");
        let script = format!(r#"
$ErrorActionPreference = 'Stop'
# NOTE: $pid is a read-only automatic variable in PowerShell (current process PID).
# Use $appPid instead to avoid "VariableNotWritable" errors.
$appPid = {pid}
$installerPath = '{installer_path}'
$logPath = '{log_path}'

function Log($msg) {{
    "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $msg" | Out-File -Append $logPath
}}

Log "Waiting for app pid $appPid to exit"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
while ($sw.Elapsed.TotalSeconds -lt 60) {{
    try {{
        $p = Get-Process -Id $appPid -ErrorAction SilentlyContinue
        if (-not $p) {{ break }}
    }} catch {{ break }}
    Start-Sleep -Milliseconds 500
}}

Log "Installing update from $installerPath"
if ($installerPath.EndsWith('.msi')) {{
    Start-Process msiexec.exe -ArgumentList '/i', "`"$installerPath`"", '/quiet', '/norestart' -Verb RunAs -Wait
}} else {{
    $installDir = $null
    foreach ($regPath in @(
        'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
        'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
    )) {{
        $entry = Get-ItemProperty $regPath -ErrorAction SilentlyContinue |
            Where-Object {{ $_.DisplayName -eq 'oc-claw' }} | Select-Object -First 1
        if ($entry -and $entry.InstallLocation) {{
            $installDir = $entry.InstallLocation.Trim('"')
            break
        }}
    }}
    $nsisArgs = @('/S')
    if ($installDir) {{ $nsisArgs += "/D=$installDir" }}
    Log "Running installer with args: $($nsisArgs -join ' ')"
    try {{
        Start-Process $installerPath -ArgumentList $nsisArgs -Verb RunAs -Wait
    }} catch {{
        Log "Installer failed (UAC denied or error): $_"
        exit 1
    }}
}}

Log "Launching updated app"
# Find install location from registry (user may have chosen a custom path).
# The executable is named oc_claw.exe (productName config produces this binary name).
$appPath = $null
foreach ($regPath in @(
    'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*',
    'HKCU:\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\*'
)) {{
    $entry = Get-ItemProperty $regPath -ErrorAction SilentlyContinue |
        Where-Object {{ $_.DisplayName -eq 'oc-claw' }} | Select-Object -First 1
    if ($entry -and $entry.InstallLocation) {{
        $loc = $entry.InstallLocation.Trim('"')
        $candidate = Join-Path $loc 'oc_claw.exe'
        if (Test-Path $candidate) {{
            $appPath = $candidate
            break
        }}
    }}
}}
if (-not $appPath) {{
    # Fallback: check common locations
    foreach ($dir in @("$env:LOCALAPPDATA\oc-claw", "$env:ProgramFiles\oc-claw", "H:\oc-claw")) {{
        $candidate = Join-Path $dir 'oc_claw.exe'
        if (Test-Path $candidate) {{ $appPath = $candidate; break }}
    }}
}}
if ($appPath) {{
    Log "Relaunching from $appPath"
    Start-Process $appPath
}} else {{
    Log "Warning: could not find oc_claw.exe to relaunch"
}}
"#,
            pid = std::process::id(),
            installer_path = dmg_path.display().to_string().replace('\\', "\\\\").replace('\'', "''"),
            log_path = log_path.display().to_string().replace('\\', "\\\\").replace('\'', "''"),
        );
        std::fs::write(&helper_path, &script).map_err(|e| format!("failed to write helper script: {e}"))?;

        let mut update_cmd = std::process::Command::new("powershell");
        update_cmd.args(["-ExecutionPolicy", "Bypass", "-File"])
            .arg(&helper_path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        hide_window_cmd(&mut update_cmd);
        update_cmd.spawn()
            .map_err(|e| format!("failed to start installer helper: {e}"))?;
    }

    emit_update_progress(
        &app,
        "ready_to_restart",
        Some(100),
        downloaded_bytes,
        total_bytes,
        "下载完成，即将退出应用并安装更新",
    );
    Ok(())
}

#[tauri::command]
async fn get_claude_conversation(session_id: String) -> Result<Vec<ChatMessage>, String> {
    let path = match resolve_session_jsonl_path(&session_id, None) {
        Some(p) => p,
        None => return Ok(vec![]),
    };

    let content = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let mut messages = Vec::new();
    let max_messages = 1000;

    // Scan from end, collecting up to max_messages actual chat messages
    for line in content.lines().rev() {
        if messages.len() >= max_messages { break; }
        if line.trim().is_empty() { continue; }
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = parsed.get("type").and_then(|v| v.as_str()).unwrap_or("");

        // Claude/OpenClaw-style records: type=assistant|user|human.
        if msg_type == "assistant" || msg_type == "user" || msg_type == "human" {
            if parsed.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false) {
                continue;
            }

            let role = if msg_type == "assistant" { "assistant" } else { "user" };
            let text = if let Some(s) = parsed.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
                s.to_string()
            } else if let Some(arr) = parsed.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
                arr.iter()
                    .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n")
            } else {
                continue;
            };

            if text.trim().is_empty() {
                continue;
            }
            if text.starts_with("<command-name>") || text.starts_with("[Request interrupted") {
                continue;
            }
            if text.starts_with("<task-notification>") || text.starts_with("<local-command") {
                continue;
            }

            let text = if text.starts_with("This session is being continued from a previous conversation") {
                "/compact".to_string()
            } else {
                text
            };
            let timestamp = parsed.get("timestamp").and_then(|t| t.as_str()).map(String::from);
            messages.push(ChatMessage { role: role.to_string(), text, timestamp });
            continue;
        }

        // Codex records: event_msg payload user_message / agent_message.
        if msg_type == "event_msg" {
            let payload_type = parsed.get("payload")
                .and_then(|p| p.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let role = match payload_type {
                "user_message" => "user",
                "agent_message" => "assistant",
                _ => continue,
            };
            let text = parsed.get("payload")
                .and_then(|p| p.get("message"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if text.trim().is_empty() {
                continue;
            }
            let timestamp = parsed.get("timestamp").and_then(|t| t.as_str()).map(String::from);
            messages.push(ChatMessage { role: role.to_string(), text, timestamp });
        }
    }

    messages.reverse();
    Ok(messages)
}

#[tauri::command]
async fn install_claude_hooks() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let claude_dir = home.join(".claude");
    let hooks_dir = claude_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).map_err(|e| e.to_string())?;

    // Write hook script — platform-specific
    #[cfg(unix)]
    let hook_path = hooks_dir.join("ooclaw-hook.sh");
    #[cfg(windows)]
    let hook_path = hooks_dir.join("ooclaw-hook.ps1");

    #[cfg(unix)]
    {
        let hook_script = r#"#!/bin/bash
# ooclaw Claude Code hook - forwards events to /tmp/ooclaw-claude.sock
SOCKET_PATH="/tmp/ooclaw-claude.sock"
[ -S "$SOCKET_PATH" ] || exit 0

# Detect non-interactive (claude -p / --print) sessions
IS_INTERACTIVE=true
for CHECK_PID in $PPID $(ps -o ppid= -p $PPID 2>/dev/null | tr -d ' '); do
    if ps -o args= -p "$CHECK_PID" 2>/dev/null | grep -qE '(^| )(-p|--print)( |$)'; then
        IS_INTERACTIVE=false
        break
    fi
done
export OOCLAW_INTERACTIVE=$IS_INTERACTIVE
# $PPID is the PID of the process that spawned this bash (i.e. Claude Code).
# Forwarded to oc-claw so it can detect when CC exits (Ctrl+C / SIGKILL)
# and clear stale "waiting" sessions.
export CC_PID=$PPID

# Capture Ghostty terminal ID once per CC session (cached per CC PID).
# The hook runs inside the CC terminal, so the focused tab is the right one.
_TID_CACHE="/tmp/ooclaw-tid-$PPID"
if [ -f "$_TID_CACHE" ]; then
    export GHOSTTY_TID=$(cat "$_TID_CACHE" 2>/dev/null)
else
    export GHOSTTY_TID=$(osascript -e 'try
tell application "Ghostty" to return id of first terminal of selected tab of front window as text
end try' 2>/dev/null || echo "")
    [ -n "$GHOSTTY_TID" ] && echo "$GHOSTTY_TID" > "$_TID_CACHE" 2>/dev/null
fi

/usr/bin/python3 -c "
import json, os, socket, sys

try:
    input_data = json.load(sys.stdin)
except:
    sys.exit(0)

hook_event = input_data.get('hook_event_name', '')

status_map = {
    'UserPromptSubmit': 'processing',
    'PreCompact': 'compacting',
    'SessionStart': 'waiting_for_input',
    'SessionEnd': 'ended',
    'PreToolUse': 'running_tool',
    'PostToolUse': 'processing',
    'PermissionRequest': 'waiting_for_input',
    'Stop': 'waiting_for_input',
    'SubagentStop': 'waiting_for_input',
}

output = {
    'sessionId': input_data.get('session_id', ''),
    'cwd': input_data.get('cwd', ''),
    'event': hook_event,
    'claudeStatus': input_data.get('status', status_map.get(hook_event, 'unknown')),
    'interactive': os.environ.get('OOCLAW_INTERACTIVE', 'true') == 'true',
    'pid': int(os.environ.get('CC_PID', '0')) or None,
}

# Ghostty terminal ID for precise tab jumping
_tid = os.environ.get('GHOSTTY_TID', '')
if _tid:
    output['terminalId'] = _tid

if hook_event == 'UserPromptSubmit':
    prompt = input_data.get('prompt', '')
    if prompt:
        output['userPrompt'] = prompt[:200]

tool = input_data.get('tool_name', '')
if tool:
    output['tool'] = tool

tool_input = input_data.get('tool_input', {})
if tool_input:
    # For Write/Edit, build a slim JSON with complete structure so the
    # frontend can parse it and show file name + numbered code lines.
    if tool in ('Write', 'Edit'):
        slim = {}
        if tool_input.get('file_path'):
            slim['file_path'] = tool_input['file_path']
        c = tool_input.get('content') or tool_input.get('new_string') or tool_input.get('old_string') or ''
        if c:
            slim['content'] = c[:5000]
        output['toolInput'] = json.dumps(slim)
    elif tool == 'Bash':
        slim = {}
        if tool_input.get('command'):
            slim['command'] = tool_input['command'][:500]
        if tool_input.get('description'):
            slim['description'] = tool_input['description'][:200]
        output['toolInput'] = json.dumps(slim)
    else:
        output['toolInput'] = json.dumps(tool_input)[:300]

if hook_event == 'Stop':
    msg = input_data.get('last_assistant_message', '')
    if msg:
        output['lastResponse'] = msg[:2000]

if hook_event == 'PermissionRequest':
    output['permission_suggestions'] = input_data.get('permission_suggestions', [])

try:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect('$SOCKET_PATH')
    sock.sendall(json.dumps(output).encode())
    if hook_event == 'PermissionRequest':
        sock.shutdown(socket.SHUT_WR)
        response = b''
        while True:
            chunk = sock.recv(4096)
            if not chunk:
                break
            response += chunk
        sock.close()
        if response:
            sys.stdout.write(response.decode('utf-8', errors='replace'))
            sys.stdout.flush()
    else:
        sock.close()
except:
    pass
"
"#;
        std::fs::write(&hook_path, hook_script).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    #[cfg(windows)]
    {
        // Windows hook: uses PowerShell directly (no .cmd wrapper).
        // Claude Code runs hooks via /usr/bin/bash (Git Bash) on Windows,
        // so .cmd files and backslash paths don't work. We write a .ps1 file
        // and register the command as "powershell.exe ... -File '<forward-slash-path>'"
        // in settings.json so bash can invoke it correctly.
        // Simplified hook: forward raw CC JSON directly to the TCP server.
        // Do NOT parse/reconstruct JSON in PowerShell — large payloads (Stop events
        // with last_assistant_message containing full response text) get truncated by
        // [Console]::In.ReadToEnd(), breaking ConvertFrom-Json. The Rust side accepts
        // both processed (sessionId, event) and raw CC field names (session_id, hook_event_name).
        // Forward raw CC JSON to TCP. Use explicit Socket.Shutdown(Send) to ensure
        // the server receives EOF immediately — TcpClient.Dispose()/Close() alone on
        // Windows may delay the FIN packet, causing the server's read to hang or timeout
        // with incomplete data.
        let ps1_script = r#"$ErrorActionPreference = 'SilentlyContinue'
[Console]::InputEncoding = [System.Text.Encoding]::UTF8
try {
    $raw = [Console]::In.ReadToEnd()
    if ([string]::IsNullOrWhiteSpace($raw)) { exit 0 }
    $ccPid = (Get-Process -Id $PID).Parent.Parent.Id
    if ($ccPid -and $raw.StartsWith('{')) {
        $raw = '{"pid":' + $ccPid + ',' + $raw.Substring(1)
    }
    $isPermission = $raw -match '"hook_event_name"\s*:\s*"PermissionRequest"'
    $client = [System.Net.Sockets.TcpClient]::new('127.0.0.1', 19283)
    $stream = $client.GetStream()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($raw)
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush()
    $client.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send)
    if ($isPermission) {
        $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8)
        $response = $reader.ReadToEnd()
        if ($response) {
            [Console]::Out.Write($response)
            [Console]::Out.Flush()
        }
        $reader.Close()
    }
    $client.Close()
} catch {}
"#;
        std::fs::write(&hook_path, ps1_script).map_err(|e| e.to_string())?;
    }

    // Update ~/.claude/settings.json to register hooks
    let settings_path = claude_dir.join("settings.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    // On Windows, Claude Code runs hooks via bash (Git Bash), so the command
    // must be bash-compatible. We call powershell.exe with forward-slash path.
    #[cfg(windows)]
    let hook_path_str = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -File '{}'",
        hook_path.to_string_lossy().replace('\\', "/")
    );
    #[cfg(not(windows))]
    let hook_path_str = hook_path.to_string_lossy().to_string();
    let hooks = settings.as_object_mut().ok_or("settings not object")?
        .entry("hooks").or_insert(serde_json::json!({}))
        .as_object_mut().ok_or("hooks not object")?;

    // Hook registration configs matching notchi's HookInstaller approach
    let hook_entry = serde_json::json!([{"type": "command", "command": hook_path_str}]);
    let without_matcher = vec![serde_json::json!({"hooks": hook_entry})];
    let with_matcher = vec![serde_json::json!({"matcher": "*", "hooks": hook_entry})];
    let pre_compact = vec![
        serde_json::json!({"matcher": "auto", "hooks": hook_entry}),
        serde_json::json!({"matcher": "manual", "hooks": hook_entry}),
    ];

    let hook_configs: Vec<(&str, &Vec<serde_json::Value>)> = vec![
        ("UserPromptSubmit", &without_matcher),
        ("PreToolUse", &with_matcher),
        ("PostToolUse", &with_matcher),
        ("PermissionRequest", &with_matcher),
        ("PreCompact", &pre_compact),
        ("Stop", &without_matcher),
        ("SubagentStop", &without_matcher),
        ("SessionStart", &without_matcher),
        ("SessionEnd", &without_matcher),
    ];

    // Detect both old (.cmd path) and new (powershell.exe ... .ps1) hook entries for cleanup
    let has_our_hook = |entry: &serde_json::Value| -> bool {
        let is_ours = |cmd: &str| -> bool {
            cmd == hook_path_str || cmd.contains("ooclaw-hook")
        };
        entry.get("command").and_then(|c| c.as_str()).map_or(false, |c| is_ours(c))
        || entry.get("hooks").and_then(|hs| hs.as_array()).map_or(false, |hs| {
            hs.iter().any(|inner| inner.get("command").and_then(|c| c.as_str()).map_or(false, |c| is_ours(c)))
        })
    };

    for (event, configs) in hook_configs {
        let event_hooks = hooks.entry(event).or_insert(serde_json::json!([]));
        let arr = event_hooks.as_array_mut().ok_or("not array")?;
        arr.retain(|h| !has_our_hook(h));
        for config in configs {
            arr.push(config.clone());
        }
    }

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    // Keep Codex desktop integration in sync with Claude integration.
    // Frontend still invokes `install_claude_hooks`, so we install both
    // hook systems here to avoid requiring frontend API changes.
    install_codex_hooks().await?;

    Ok(())
}

async fn install_codex_hooks() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let codex_dir = home.join(".Codex");
    let hooks_dir = codex_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    let hook_path = hooks_dir.join("ooclaw-codex-hook.sh");
    #[cfg(windows)]
    let hook_path = hooks_dir.join("ooclaw-codex-hook.ps1");

    #[cfg(unix)]
    {
        let hook_script = r#"#!/bin/bash
# ooclaw Codex hook - forwards events to /tmp/ooclaw-claude.sock
SOCKET_PATH="/tmp/ooclaw-claude.sock"
[ -S "$SOCKET_PATH" ] || { echo '{}'; exit 0; }
export CC_PID=$PPID

# Capture Ghostty terminal ID once per Codex process so stop-time active-tab
# checks and click-to-jump can target the exact tab.
_TID_CACHE="/tmp/ooclaw-tid-$PPID"
if [ -f "$_TID_CACHE" ]; then
    export GHOSTTY_TID=$(cat "$_TID_CACHE" 2>/dev/null)
else
    export GHOSTTY_TID=$(osascript -e 'try
tell application "Ghostty" to return id of first terminal of selected tab of front window as text
end try' 2>/dev/null || echo "")
    [ -n "$GHOSTTY_TID" ] && echo "$GHOSTTY_TID" > "$_TID_CACHE" 2>/dev/null
fi

/usr/bin/python3 -c "
import json, os, socket, sys

raw = sys.stdin.read()
if not raw.strip():
    print('{}')
    sys.exit(0)

try:
    data = json.loads(raw)
except:
    print('{}')
    sys.exit(0)

if not isinstance(data, dict):
    print('{}')
    sys.exit(0)

if not data.get('source'):
    data['source'] = 'codex'

if not data.get('pid'):
    try:
        pid = int(os.environ.get('CC_PID', '0'))
        if pid > 0:
            data['pid'] = pid
    except:
        pass

tid = os.environ.get('GHOSTTY_TID', '')
if tid and not data.get('terminalId'):
    data['terminalId'] = tid

hook_event = data.get('hook_event_name') or data.get('event') or data.get('codex_event_type') or ''
if hook_event and not data.get('hook_event_name'):
    data['hook_event_name'] = hook_event

# Codex may omit cwd in some events. Fall back to process cwd so session
# records still have a stable workspace path.
if not data.get('cwd') and not data.get('workdir'):
    try:
        data['cwd'] = os.getcwd()
    except:
        pass

payload = json.dumps(data)

try:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect('$SOCKET_PATH')
    sock.sendall(payload.encode('utf-8'))

    if hook_event == 'PermissionRequest':
        sock.shutdown(socket.SHUT_WR)
        response = b''
        while True:
            chunk = sock.recv(4096)
            if not chunk:
                break
            response += chunk
        sock.close()
        if response:
            sys.stdout.write(response.decode('utf-8', errors='replace'))
        else:
            sys.stdout.write('{}')
    else:
        sock.shutdown(socket.SHUT_WR)
        sock.close()
        sys.stdout.write('{}')
except:
    sys.stdout.write('{}')
"
"#;
        std::fs::write(&hook_path, hook_script).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    #[cfg(windows)]
    {
        // On Windows, keep the hook simple: forward Codex JSON to the existing
        // oc-claw TCP hook server. `process_claude_event` handles both Codex
        // and Claude field variants.
        let ps1_script = r#"$ErrorActionPreference = 'SilentlyContinue'
[Console]::InputEncoding = [System.Text.Encoding]::UTF8
try {
    $raw = [Console]::In.ReadToEnd()
    if ([string]::IsNullOrWhiteSpace($raw)) {
        [Console]::Out.Write('{}')
        exit 0
    }

    $obj = $null
    try { $obj = $raw | ConvertFrom-Json } catch {}
    if ($obj -ne $null) {
        $ccPid = (Get-Process -Id $PID).Parent.Parent.Id
        if (-not $obj.source) { $obj.source = 'codex' }
        if ($ccPid -and -not $obj.pid) { $obj | Add-Member -NotePropertyName pid -NotePropertyValue $ccPid -Force }
        if (-not $obj.hook_event_name -and $obj.codex_event_type) { $obj.hook_event_name = $obj.codex_event_type }
        if (-not $obj.cwd -and -not $obj.workdir) { $obj.cwd = (Get-Location).Path }
        $raw = $obj | ConvertTo-Json -Compress -Depth 20
    }

    $hookName = ''
    if ($obj -ne $null -and $obj.hook_event_name) { $hookName = [string]$obj.hook_event_name }

    $client = [System.Net.Sockets.TcpClient]::new('127.0.0.1', 19283)
    $stream = $client.GetStream()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($raw)
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush()
    $client.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send)

    if ($hookName -eq 'PermissionRequest') {
        $reader = [System.IO.StreamReader]::new($stream, [System.Text.Encoding]::UTF8)
        $response = $reader.ReadToEnd()
        if ($response) { [Console]::Out.Write($response) } else { [Console]::Out.Write('{}') }
        $reader.Close()
    } else {
        [Console]::Out.Write('{}')
    }
    [Console]::Out.Flush()
    $client.Close()
} catch {
    try { [Console]::Out.Write('{}'); [Console]::Out.Flush() } catch {}
}
"#;
        std::fs::write(&hook_path, ps1_script).map_err(|e| e.to_string())?;
    }

    let hooks_json_path = codex_dir.join("hooks.json");
    let mut config: serde_json::Value = if hooks_json_path.exists() {
        let content = std::fs::read_to_string(&hooks_json_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };
    if config.get("hooks").is_none() {
        config["hooks"] = serde_json::json!({});
    }
    let hooks = config["hooks"].as_object_mut().ok_or("hooks is not an object")?;

    #[cfg(windows)]
    let hook_command = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -File '{}'",
        hook_path.to_string_lossy().replace('\\', "/"),
    );
    #[cfg(not(windows))]
    let hook_command = hook_path.to_string_lossy().to_string();

    let has_our_hook = |entry: &serde_json::Value| -> bool {
        let is_ours = |cmd: &str| -> bool {
            cmd == hook_command || cmd.contains("ooclaw-codex-hook")
        };
        entry.get("command").and_then(|c| c.as_str()).map_or(false, |c| is_ours(c))
            || entry.get("hooks").and_then(|hs| hs.as_array()).map_or(false, |hs| {
                hs.iter().any(|inner| inner.get("command").and_then(|c| c.as_str()).map_or(false, |c| is_ours(c)))
            })
    };

    let hook_def = serde_json::json!({"type": "command", "command": hook_command});
    let event_names = [
        "SessionStart",
        "UserPromptSubmit",
        "PreToolUse",
        "PostToolUse",
        "PermissionRequest",
        "Stop",
        "StopFailure",
        "SubagentStop",
    ];
    for event_name in event_names {
        let arr = hooks.entry(event_name.to_string()).or_insert(serde_json::json!([]));
        let list = arr.as_array_mut().ok_or("hook event is not an array")?;
        list.retain(|entry| !has_our_hook(entry));
        list.push(serde_json::json!({"hooks": [hook_def.clone()]}));
    }

    let json_str = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(&hooks_json_path, json_str).map_err(|e| e.to_string())?;

    Ok(())
}

fn codex_requires_escalation(event: &serde_json::Value) -> bool {
    fn read_bool(v: &serde_json::Value, keys: &[&str]) -> bool {
        keys.iter()
            .filter_map(|k| v.get(k))
            .any(|x| x.as_bool().unwrap_or(false))
    }

    fn read_string<'a>(v: &'a serde_json::Value, keys: &[&str]) -> Option<&'a str> {
        keys.iter().find_map(|k| v.get(k).and_then(|x| x.as_str()))
    }

    fn has_explicit_escalation_markers(v: &serde_json::Value) -> bool {
        let sandbox_mode = read_string(v, &["sandbox_permissions", "sandboxPermissions"])
            .unwrap_or("");
        if sandbox_mode.eq_ignore_ascii_case("require_escalated")
            || sandbox_mode.eq_ignore_ascii_case("escalated")
        {
            return true;
        }
        if read_bool(
            v,
            &[
                "with_escalated_permissions",
                "withEscalatedPermissions",
                "requires_approval",
                "requiresApproval",
                "approval_required",
                "approvalRequired",
            ],
        ) {
            return true;
        }
        let justification = read_string(v, &["justification"]).unwrap_or("").trim();
        !justification.is_empty()
    }

    fn parse_tool_input(event: &serde_json::Value) -> Option<serde_json::Value> {
        let tool_input = event.get("tool_input").or_else(|| event.get("toolInput"))?;
        if tool_input.is_object() {
            return Some(tool_input.clone());
        }
        if let Some(raw) = tool_input.as_str() {
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(raw) {
                return Some(parsed);
            }
        }
        None
    }

    // Preferred path: explicit approval/escalation fields.
    if has_explicit_escalation_markers(event) {
        return true;
    }
    let parsed_tool_input = parse_tool_input(event);
    if let Some(tool_input) = parsed_tool_input.as_ref() {
        if has_explicit_escalation_markers(tool_input) {
            return true;
        }
    }

    // Fallback for Codex payloads that omit explicit flags:
    // PreToolUse(Bash) in default permission mode with an obvious
    // out-of-workspace write command almost always means approval UI.
    let tool_name = read_string(event, &["tool", "tool_name"]).unwrap_or("");
    let permission_mode = read_string(event, &["permission_mode", "permissionMode"]).unwrap_or("");
    let is_codex_like = event.get("turn_id").is_some()
        || event.get("hook_event_name").is_some()
        || read_string(event, &["source"]).unwrap_or("").eq_ignore_ascii_case("codex");
    if !(is_codex_like && tool_name == "Bash" && permission_mode == "default") {
        return false;
    }

    let command = parsed_tool_input
        .as_ref()
        .and_then(|ti| read_string(ti, &["command"]))
        .unwrap_or("");
    if command.is_empty() {
        return false;
    }
    command.contains("$HOME/")
        || command.contains("/Users/")
        || command.contains("Desktop/")
        || command.contains(" cat > ")
        || command.contains(" > ")
        || command.contains("<<'EOF'")
        || command.contains("<<EOF")
}

fn is_codex_internal_utility_event(event: &serde_json::Value) -> bool {
    let permission_mode = event.get("permission_mode")
        .or_else(|| event.get("permissionMode"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if permission_mode != "bypassPermissions" {
        return false;
    }

    let prompt = event.get("prompt")
        .or_else(|| event.get("userPrompt"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if prompt.starts_with("You are a helpful assistant. You will be presented with a user prompt") {
        return true;
    }

    let transcript_is_null = event.get("transcript_path").map(|v| v.is_null()).unwrap_or(false);
    let source = event.get("source").and_then(|v| v.as_str()).unwrap_or("");
    let model = event.get("model").and_then(|v| v.as_str()).unwrap_or("");
    if transcript_is_null && (source == "startup" || model == "gpt-5.4-mini") {
        return true;
    }

    let last_message = event.get("last_assistant_message")
        .or_else(|| event.get("codex_last_assistant_message"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_start();
    if last_message.starts_with("{\"title\":") {
        return true;
    }

    false
}

fn is_codex_internal_utility_session(session: &ClaudeSession) -> bool {
    if session.source != "codex" {
        return false;
    }

    let prompt = session.user_prompt.as_deref().unwrap_or("");
    if prompt.starts_with("You are a helpful assistant. You will be presented with a user prompt") {
        return true;
    }

    let last = session.last_response.as_deref().unwrap_or("").trim_start();
    last.starts_with("{\"title\":")
}

/// Process a Claude hook event (shared logic between Unix socket and TCP server).
/// Returns Some((session_id, hook_event)) if the event needs further handling
/// (e.g. PermissionRequest requires blocking the connection for a response).
fn process_claude_event(
    buf: &str,
    state: &Arc<Mutex<HashMap<String, ClaudeSession>>>,
    app: &tauri::AppHandle,
    source_override: Option<&str>,
) -> Option<(String, String)> {
    log::info!("[claude_event] raw buf len={} content={}", buf.len(), &buf[..buf.len().min(500)]);
    if let Ok(event) = serde_json::from_str::<serde_json::Value>(buf) {
        // Accept both processed field names (sessionId, event, claudeStatus) from the old
        // hook format AND raw CC field names (session_id, hook_event_name, status).
        // On Windows the hook now forwards raw CC JSON directly to avoid truncation issues
        // with large payloads (Stop events contain last_assistant_message with full response text).
        let session_id = event.get("sessionId")
            .or_else(|| event.get("session_id"))
            .or_else(|| event.get("conversation_id"))
            .and_then(|v| v.as_str()).unwrap_or("").to_string();
        if session_id.is_empty() { log::warn!("[claude_event] empty sessionId, ignoring"); return None; }

        let raw_hook_event = event.get("event")
            .or_else(|| event.get("hook_event_name"))
            .or_else(|| event.get("codex_event_type"))
            .and_then(|v| v.as_str()).unwrap_or("").to_string();
        // Normalize Cursor's camelCase event names to CC's PascalCase.
        // Cursor and CC have different hook event sets:
        //   Cursor: beforeSubmitPrompt, stop, beforeShellExecution, afterShellExecution,
        //           beforeMCPExecution, afterMCPExecution, afterFileEdit, beforeReadFile,
        //           afterAgentThought, afterAgentResponse
        //   CC:     UserPromptSubmit, Stop, PreToolUse, PostToolUse, SessionStart, etc.
        let hook_event = match raw_hook_event.as_str() {
            "beforeSubmitPrompt" => "UserPromptSubmit".to_string(),
            "hook-user-prompt-submit" => "UserPromptSubmit".to_string(),
            "sessionStart" => "SessionStart".to_string(),
            "sessionEnd" => "SessionEnd".to_string(),
            "agentStop" => "Stop".to_string(),
            "StopFailure" | "stopFailure" => "Stop".to_string(),
            "preToolUse" => "PreToolUse".to_string(),
            "postToolUse" | "postToolUseFailure" => "PostToolUse".to_string(),
            "subagentStart" => "PreToolUse".to_string(),
            "subagentStop" => "SubagentStop".to_string(),
            "preCompact" => "PreCompact".to_string(),
            // Cursor-specific tool events → map to PreToolUse/PostToolUse
            "beforeShellExecution" | "beforeMCPExecution" | "beforeReadFile" => "PreToolUse".to_string(),
            "afterShellExecution" | "afterMCPExecution" | "afterFileEdit" => "PostToolUse".to_string(),
            "afterAgentThought" | "afterAgentResponse" => "PostToolUse".to_string(),
            "stop" => "Stop".to_string(),
            other => other.to_string(),
        };

        // Codex desktop may emit internal utility sessions (for example title
        // generation). These should not appear in the session list or trigger
        // completion notifications.
        if is_codex_internal_utility_event(&event) {
            if let Ok(mut sessions) = state.lock() {
                sessions.remove(&session_id);
            }
            stop_session_file_watcher(&session_id);
            log::info!(
                "[claude_event] ignore internal codex utility session={} event={}",
                session_id,
                hook_event
            );
            return None;
        }

        let claude_status = event.get("claudeStatus").or_else(|| event.get("status"))
            .and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

        let is_processing = claude_status != "waiting_for_input";

        let user_prompt = event.get("userPrompt").or_else(|| event.get("prompt"))
            .and_then(|v| v.as_str()).unwrap_or("");
        let is_local_slash = if user_prompt.starts_with('/') {
            let cmd = user_prompt.split_whitespace().next().unwrap_or("");
            matches!(cmd, "/clear" | "/compact" | "/help" | "/cost" | "/status" | "/vim" | "/fast" | "/model" | "/login" | "/logout")
        } else { false };

        let pretool_needs_waiting = hook_event == "PreToolUse" && codex_requires_escalation(&event);
        let mut status = match hook_event.as_str() {
            "UserPromptSubmit" => {
                if is_local_slash { "stopped".to_string() } else { "processing".to_string() }
            }
            "PreCompact" => "compacting".to_string(),
            "PreToolUse" => {
                let tool = event.get("tool")
                    .or_else(|| event.get("tool_name"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Different clients may report interactive choice tools with
                // slightly different names. Treat both as waiting states so
                // the selection popup can be shown consistently.
                if tool == "AskUserQuestion" || tool == "AskQuestion" || pretool_needs_waiting {
                    "waiting".to_string()
                } else {
                    "tool_running".to_string()
                }
            }
            "PostToolUse" => "processing".to_string(),
            "Stop" => "stopped".to_string(),
            "SubagentStop" => "processing".to_string(),
            "SessionEnd" => "ended".to_string(),
            "PermissionRequest" => "waiting".to_string(),
            "SessionStart" => {
                if is_processing { "processing".to_string() } else { "stopped".to_string() }
            }
            _ => {
                if !is_processing { "stopped".to_string() } else { claude_status.clone() }
            }
        };

        // Guard: if CC's own status is "waiting_for_input" but our event-derived
        // status says "processing"/"tool_running", something is out of sync.
        // Override to "stopped" — EXCEPT for UserPromptSubmit, where CC's status
        // field may still say "waiting_for_input" because the hook fires before
        // CC's internal state transitions. A new prompt always means processing.
        if !is_processing
            && matches!(status.as_str(), "processing" | "tool_running")
            && hook_event != "UserPromptSubmit"
        {
            log::info!("[claude_event] guard override: {} → stopped (is_processing=false)", status);
            status = "stopped".to_string();
        }
        log::info!("[claude_event] session={} event={} claude_status={} is_processing={} → final_status={}",
            &session_id[..session_id.len().min(8)], hook_event, claude_status, is_processing, status);

        let was_processing;
        let was_compacting;
        let pending_agents;
        let session_source: String;
        let stop_was_interrupted;

        {
            let mut sessions = state.lock().unwrap();
            let prev_status = sessions.get(&session_id).map(|s| s.status.clone()).unwrap_or_default();
            was_processing = matches!(prev_status.as_str(), "processing" | "tool_running" | "compacting");
            was_compacting = prev_status == "compacting";

            if hook_event == "SessionEnd" {
                session_source = sessions.get(&session_id).map(|s| s.source.clone()).unwrap_or_else(|| "cc".to_string());
                sessions.remove(&session_id);
                pending_agents = 0;
                stop_was_interrupted = false;
            } else {
                // Determine source: explicit override from socket server, or from JSON, or default "cc"
                let source = source_override
                    .map(|s| s.to_string())
                    .or_else(|| event.get("source").and_then(|v| v.as_str()).map(|s| s.to_string()))
                    .unwrap_or_else(|| "cc".to_string());
                let session = sessions.entry(session_id.clone()).or_insert_with(|| ClaudeSession {
                    session_id: session_id.clone(),
                    cwd: String::new(),
                    status: "idle".to_string(),
                    tool: None,
                    tool_input: None,
                    user_prompt: None,
                    interactive: true,
                    updated_at: 0,
                    is_processing: false,
                    pid: None,
                    pending_agents: 0,
                    last_response: None,
                    is_active_tab: false,
                    source: source.clone(),
                    permission_suggestions: None,
                    terminal_id: None,
                    host_terminal: None,
                    cursor_port: None,
                    cursor_workspace_root: None,
                    cursor_workspace_name: None,
                    cursor_native_handle: None,
                });
                // Only upgrade source, never downgrade:
                // cc < codex < cursor.
                // Once a session is identified as codex/cursor, later generic
                // CC events (source=cc) for the same sessionId must not
                // overwrite it, otherwise active-tab/staleness logic regresses.
                let source_rank = |s: &str| -> u8 {
                    match s {
                        "cc" => 1,
                        "codex" => 2,
                        "cursor" => 3,
                        _ => 0,
                    }
                };
                if source_rank(&source) >= source_rank(&session.source) {
                    session.source = source.clone();
                }

                // Track pending sub-agents:
                // - PreToolUse with tool=Agent → a sub-agent is being launched
                // - SubagentStop → a sub-agent has completed
                // Sound only plays on Stop when pending_agents == 0 (all agents done).
                let tool_name = event.get("tool").or_else(|| event.get("tool_name"))
                    .and_then(|v| v.as_str()).unwrap_or("");
                if hook_event == "UserPromptSubmit" {
                    // New user prompt = fresh start. Reset counter in case previous
                    // agents were killed or SubagentStop was never delivered.
                    session.pending_agents = 0;
                } else if (hook_event == "PreToolUse" && tool_name == "Agent") || raw_hook_event == "subagentStart" {
                    session.pending_agents += 1;
                    log::info!("[claude_event] session={} Agent launched, pending_agents={}",
                        &session_id[..session_id.len().min(8)], session.pending_agents);
                } else if hook_event == "SubagentStop" {
                    session.pending_agents = session.pending_agents.saturating_sub(1);
                    log::info!("[claude_event] session={} SubagentStop, pending_agents={}",
                        &session_id[..session_id.len().min(8)], session.pending_agents);
                }

                session.status = status.clone();
                session.is_processing = is_processing;
                let incoming_cwd = event.get("cwd")
                    .or_else(|| event.get("workdir"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if !incoming_cwd.is_empty() || session.cwd.is_empty() {
                    session.cwd = incoming_cwd.to_string();
                }
                session.interactive = event.get("interactive").and_then(|v| v.as_bool()).unwrap_or(true);
                session.updated_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

                if session.source == "cursor" && !session.cwd.is_empty() {
                    // Cursor hook payloads do not expose a stable window ID or terminal PID.
                    // Instead we bind the session to the extension port whose workspace roots
                    // best match the session cwd. We do this on first sighting and whenever a
                    // new prompt starts so a re-opened / re-focused window can rebind cleanly.
                    let needs_rebind = hook_event == "UserPromptSubmit"
                        || session.cursor_port.is_none()
                        || session.cursor_workspace_root.as_ref()
                            .map(|root| !cwd_matches_workspace_root(&session.cwd, root))
                            .unwrap_or(false);

                    if needs_rebind {
                        if let Some(binding) = resolve_cursor_window_binding(
                            &session.cwd,
                            session.cursor_port,
                            session.cursor_native_handle.as_deref(),
                        ) {
                            if session.cursor_port != Some(binding.port)
                                || session.cursor_workspace_root.as_deref() != Some(binding.workspace_root.as_str()) {
                                log::info!(
                                    "[cursor_bind] session={} port={} workspace_root={} workspace_name={} native_handle={:?}",
                                    &session_id[..session_id.len().min(8)],
                                    binding.port,
                                    binding.workspace_root,
                                    binding.workspace_name,
                                    binding.native_handle,
                                );
                            }
                            session.cursor_port = Some(binding.port);
                            session.cursor_workspace_root = Some(binding.workspace_root);
                            session.cursor_workspace_name = Some(binding.workspace_name);
                            session.cursor_native_handle = binding.native_handle;
                        } else {
                            log::info!(
                                "[cursor_bind] session={} unresolved cwd={}",
                                &session_id[..session_id.len().min(8)],
                                session.cwd,
                            );
                        }
                    }
                }

                if let Some(t) = event.get("tool").or_else(|| event.get("tool_name")).and_then(|v| v.as_str()) {
                    if !t.is_empty() { session.tool = Some(t.to_string()); }
                }
                if let Some(tool_input_val) = event.get("toolInput").or_else(|| event.get("tool_input")) {
                    let tool_input_text = tool_input_val
                        .as_str()
                        .map(|s| s.to_string())
                        .or_else(|| serde_json::to_string(tool_input_val).ok());
                    if let Some(t) = tool_input_text {
                        if !t.is_empty() {
                            session.tool_input = Some(t);
                        }
                    }
                }
                if let Some(t) = event.get("userPrompt")
                    .or_else(|| event.get("prompt"))
                    .and_then(|v| v.as_str()) {
                    if !t.is_empty() { session.user_prompt = Some(t.to_string()); }
                }
                // Store CC process PID from hook event for stale-session detection
                if let Some(p) = event.get("pid").and_then(|v| v.as_u64()) {
                    let pid_u32 = p as u32;
                    session.pid = Some(pid_u32);
                    #[cfg(target_os = "macos")]
                    if session.host_terminal.is_none() && session.source != "cursor" {
                        session.host_terminal = find_terminal_app_for_pid(pid_u32);
                        log::info!("[claude_event] session={} host_terminal={:?}",
                            &session_id[..session_id.len().min(8)], session.host_terminal);
                        if session.source == "cc"
                            && session
                                .host_terminal
                                .as_deref()
                                .map(is_codex_host_terminal)
                                .unwrap_or(false)
                        {
                            session.source = "codex".to_string();
                        }
                    }
                }

                // Store Ghostty terminal ID from hook event for precise tab jumping.
                // The hook captures this from inside the CC terminal, so it's
                // always the correct tab — even for pre-existing sessions.
                if session.terminal_id.is_none() {
                    if let Some(tid) = event.get("terminalId").and_then(|v| v.as_str()) {
                        if !tid.is_empty() {
                            log::info!("[claude_event] session={} stored terminal_id={}",
                                &session_id[..session_id.len().min(8)], tid);
                            session.terminal_id = Some(tid.to_string());
                        }
                    }
                }

                if hook_event == "Stop" || hook_event == "SubagentStop" {
                    session.tool = None;
                    session.tool_input = None;
                }

                // Store AI's last response for the completion reminder popup.
                // Clear on new prompt so stale responses don't linger.
                //
                // For Cursor: afterAgentResponse fires before stop and carries
                // the actual response text. We stash it here so the Stop handler
                // can use it instead of a placeholder.
                if raw_hook_event == "afterAgentResponse" {
                    if let Some(resp) = event.get("lastResponse").and_then(|v| v.as_str()) {
                        if !resp.is_empty() {
                            session.last_response = Some(resp.to_string());
                        }
                    }
                }

                // Check at Stop time (real-time, not polling) whether the user
                // is already looking at this terminal tab. If so, skip setting
                // last_response so the completion popup never triggers.
                if hook_event == "Stop" {
                    let interrupted = stop_event_was_interrupted(&event, &session.source, &claude_status);
                    // CC: check if the user is looking at this session's Ghostty tab
                    // Cursor: check if Cursor (or oc-claw) is the frontmost app.
                    // If a terminal ID is missing (older hooks / non-Ghostty),
                    // fall back to host-terminal checks where available.
                    let frontmost = get_frontmost_app_name();
                    let is_ghostty_session = matches!(
                        session.host_terminal.as_deref(),
                        Some("Ghostty" | "ghostty")
                    );
                    let is_tab_active = if session.source == "cursor" {
                        is_cursor_frontmost_app(&frontmost)
                    } else if session.source == "codex" {
                        let ghostty_match = is_ghostty_session
                            && session.terminal_id.as_ref()
                                .and_then(|tid| get_active_ghostty_terminal_id().map(|a| a == *tid))
                                .unwrap_or(false);
                        ghostty_match || is_codex_frontmost_app(&frontmost)
                    } else if is_ghostty_session {
                        session.terminal_id.as_ref()
                            .and_then(|tid| get_active_ghostty_terminal_id().map(|a| a == *tid))
                            .unwrap_or(false)
                    } else if let Some(ht) = session.host_terminal.as_deref() {
                        frontmost_matches_host_terminal(&frontmost, ht)
                    } else {
                        false
                    };
                    if is_tab_active || interrupted {
                        session.last_response = None;
                    } else {
                        // Prefer lastResponse from the event itself (CC's Stop has it),
                        // then fall back to any value pre-stored by afterAgentResponse,
                        // then use a placeholder for Cursor/Codex so the popup
                        // still triggers when stop payload omits assistant text.
                        let resp_from_event = event.get("lastResponse")
                            .or_else(|| event.get("last_assistant_message"))
                            .or_else(|| event.get("codex_last_assistant_message"))
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string());
                        if resp_from_event.is_some() {
                            session.last_response = resp_from_event;
                        } else if session.last_response.is_none()
                            && (session.source == "cursor" || session.source == "codex")
                        {
                            session.last_response = Some("✓".to_string());
                        }
                        // else: keep existing last_response from afterAgentResponse
                    }
                    stop_was_interrupted = interrupted;
                } else if hook_event == "UserPromptSubmit" {
                    session.last_response = None;
                    stop_was_interrupted = false;
                } else {
                    stop_was_interrupted = false;
                }

                if hook_event == "PermissionRequest" {
                    session.permission_suggestions = event.get("permission_suggestions")
                        .or_else(|| event.get("permissionSuggestions"))
                        .cloned();
                } else {
                    session.permission_suggestions = None;
                }

                pending_agents = session.pending_agents;
                session_source = session.source.clone();
            }
        }

        let _ = app.emit("claude-session-update", &session_id);

        // Only emit completion sound on explicit Stop or PermissionRequest events.
        // Previously we checked status transitions, but guard overrides on PostToolUse
        // could falsely trigger "stopped" mid-task when CC's status field lags behind.
        // Also suppress sound while sub-agents are still running (pending_agents > 0).
        // Each PreToolUse(Agent) increments the counter, each SubagentStop decrements it.
        // Sound only plays when all sub-agents have completed.
        let is_wait_event = hook_event == "PermissionRequest"
            || (hook_event == "PreToolUse" && status == "waiting");
        let is_completion_stop = hook_event == "Stop" && pending_agents == 0 && !stop_was_interrupted;
        if was_processing && !was_compacting
            && (is_completion_stop || is_wait_event) {
            let is_waiting = is_wait_event;
            let _ = app.emit("claude-task-complete", serde_json::json!({"sessionId": session_id, "waiting": is_waiting, "source": session_source}));
        }

        let cwd_str = event.get("cwd")
            .or_else(|| event.get("workdir"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        log::info!("[claude_event] session={} event={} status={} cwd={}", session_id, hook_event, status, cwd_str);
        if hook_event == "UserPromptSubmit" {
            if let Some(jsonl_path) = resolve_session_jsonl_path(&session_id, Some(&cwd_str)) {
                log::info!(
                    "[claude_event] session file path: {} exists={}",
                    jsonl_path.display(),
                    jsonl_path.exists()
                );
                if jsonl_path.exists() {
                    start_session_file_watcher(
                        session_id.clone(),
                        jsonl_path,
                        state.clone(),
                        app.clone(),
                    );
                }
            }
        } else if hook_event == "Stop" || hook_event == "SubagentStop" || hook_event == "SessionEnd" {
            stop_session_file_watcher(&session_id);
        }

        return Some((session_id, hook_event));
    } else if let Err(e) = serde_json::from_str::<serde_json::Value>(buf) {
        let tail: String = buf.chars().rev().take(300).collect::<String>().chars().rev().collect();
        log::warn!("[claude_event] JSON parse failed: err={}, len={}, tail=...{}", e, buf.len(), tail);
    }
    None
}

// ─── Cursor Integration ───────────────────────────────────────────────

/// Install hooks for Cursor IDE.
/// Creates ~/.cursor/hooks/occlaw-cursor-hook.sh and registers it in
/// ~/.cursor/hooks.json for all Cursor hook events.
#[tauri::command]
async fn install_cursor_hooks() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let cursor_dir = home.join(".cursor");
    let hooks_dir = cursor_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).map_err(|e| e.to_string())?;

    // ── Write hook script (Unix) ──
    #[cfg(unix)]
    {
        let socket_path = "/tmp/occlaw-cursor.sock";
        let hook_script = format!(r##"#!/bin/bash
# occlaw Cursor hook — forwards events to {socket}
SOCKET_PATH="{socket}"
[ -S "$SOCKET_PATH" ] || {{ echo '{{}}'; exit 0; }}
export CC_PID=$PPID

/usr/bin/python3 -c "
import json, os, socket, sys

try:
    input_data = json.load(sys.stdin)
except:
    print('{{}}')
    sys.exit(0)

hook_event = input_data.get('hook_event_name', '')
if not hook_event:
    print('{{}}')
    sys.exit(0)

session_id = input_data.get('session_id', '') or input_data.get('conversation_id', '') or 'default'
cwd = input_data.get('cwd', '')
if not cwd:
    roots = input_data.get('workspace_roots', [])
    if roots:
        cwd = roots[0]

output = {{}}
output['sessionId'] = session_id
output['event'] = hook_event
output['source'] = 'cursor'
if cwd:
    output['cwd'] = cwd

# Map tool info — Cursor events use different field names than CC:
#   beforeShellExecution: command, cwd
#   beforeMCPExecution: tool_name, tool_input
#   afterFileEdit: file_path, edits
#   beforeReadFile: file_path, content
tool_name = input_data.get('tool_name', '')
if hook_event == 'beforeShellExecution' or hook_event == 'afterShellExecution':
    output['tool'] = 'Shell'
    cmd = input_data.get('command', '')
    if cmd:
        output['toolInput'] = json.dumps({{'command': cmd[:500]}})
elif hook_event in ('beforeMCPExecution', 'afterMCPExecution'):
    output['tool'] = tool_name or 'MCP'
    ti = input_data.get('tool_input', {{}})
    if ti:
        output['toolInput'] = json.dumps(ti)[:300]
elif hook_event == 'afterFileEdit':
    output['tool'] = 'Edit'
    fp = input_data.get('file_path', '')
    edits = input_data.get('edits', [])
    slim = {{}}
    if fp:
        slim['file_path'] = fp
    if edits:
        combined = '\\n'.join(e.get('new_string', '')[:1000] for e in edits[:3])
        slim['content'] = combined[:5000]
    output['toolInput'] = json.dumps(slim)
elif hook_event == 'beforeReadFile':
    output['tool'] = 'Read'
    fp = input_data.get('file_path', '')
    if fp:
        output['toolInput'] = json.dumps({{'file_path': fp}})
elif tool_name:
    output['tool'] = tool_name
    ti = input_data.get('tool_input', {{}})
    if ti:
        output['toolInput'] = json.dumps(ti)[:300]

# Stop event: extract status and last response
if hook_event == 'stop':
    status = input_data.get('status', '')
    if status:
        output['claudeStatus'] = status
    transcript_path = input_data.get('transcript_path', '')
    if transcript_path:
        output['transcript_path'] = transcript_path
    msg = input_data.get('last_assistant_message', '')
    if msg:
        output['lastResponse'] = msg[:2000]

# afterAgentResponse: Cursor sends the AI's response text here
# (stop event doesn't include it). Forward it so Rust can store it.
if hook_event == 'afterAgentResponse':
    text = input_data.get('text', '')
    if text:
        output['lastResponse'] = text[:2000]

# UserPromptSubmit: extract prompt text
if hook_event == 'beforeSubmitPrompt':
    prompt = input_data.get('prompt', '')
    if prompt:
        output['userPrompt'] = prompt[:200]

# PID for stale-session detection
cc_pid = os.environ.get('CC_PID', '')
if cc_pid:
    try:
        output['pid'] = int(cc_pid)
    except:
        pass

# Send to socket
try:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect('$SOCKET_PATH')
    sock.sendall(json.dumps(output).encode())
    sock.shutdown(socket.SHUT_WR)
    sock.close()
except:
    pass

# Required stdout for Cursor:
#   beforeSubmitPrompt → gating hook, needs {{'continue': true}}
#   beforeShellExecution, beforeMCPExecution → permission hooks, need {{'permission': 'allow'}}
#   beforeReadFile → permission hook, needs {{'permission': 'allow'}}
#   everything else → {{}}
if hook_event == 'beforeSubmitPrompt':
    print(json.dumps({{'continue': True}}))
elif hook_event in ('beforeShellExecution', 'beforeMCPExecution', 'beforeReadFile'):
    print(json.dumps({{'permission': 'allow'}}))
else:
    print('{{}}')
"
"##, socket = socket_path);

        let hook_path = hooks_dir.join("occlaw-cursor-hook.sh");
        std::fs::write(&hook_path, hook_script).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))
                .map_err(|e| e.to_string())?;
        }
    }

    // ── Write hook script (Windows) ──
    #[cfg(windows)]
    {
        let hook_script = r#"$ErrorActionPreference = 'SilentlyContinue'
[Console]::InputEncoding = [System.Text.Encoding]::UTF8
$raw = [Console]::In.ReadToEnd()
if (-not $raw) { Write-Output '{}'; exit 0 }
$ccPid = (Get-Process -Id $PID).Parent.Parent.Id
if ($ccPid -and $raw.StartsWith('{')) {
    $raw = '{"pid":' + $ccPid + ',"source":"cursor",' + $raw.Substring(1)
} else {
    $raw = '{"source":"cursor",' + $raw.Substring(1)
}
try {
    $client = [System.Net.Sockets.TcpClient]::new('127.0.0.1', 19284)
    $stream = $client.GetStream()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($raw)
    $stream.Write($bytes, 0, $bytes.Length)
    $client.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send)
    $hookName = ($raw | ConvertFrom-Json).hook_event_name
    if ($hookName -eq 'beforeSubmitPrompt') {
        Write-Output '{"continue":true}'
    } elseif ($hookName -eq 'beforeShellExecution' -or $hookName -eq 'beforeMCPExecution' -or $hookName -eq 'beforeReadFile') {
        Write-Output '{"permission":"allow"}'
    } else {
        Write-Output '{}'
    }
    $client.Close()
} catch {
    Write-Output '{}'
}
"#;
        let hook_path = hooks_dir.join("occlaw-cursor-hook.ps1");
        std::fs::write(&hook_path, hook_script).map_err(|e| e.to_string())?;
    }

    // ── Register hooks in ~/.cursor/hooks.json ──
    let hooks_json_path = cursor_dir.join("hooks.json");
    let mut config: serde_json::Value = if hooks_json_path.exists() {
        let content = std::fs::read_to_string(&hooks_json_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    config["version"] = serde_json::json!(1);
    if config.get("hooks").is_none() {
        config["hooks"] = serde_json::json!({});
    }

    #[cfg(unix)]
    let hook_command = hooks_dir.join("occlaw-cursor-hook.sh").to_string_lossy().to_string();
    #[cfg(windows)]
    let hook_command = format!("powershell.exe -NoProfile -ExecutionPolicy Bypass -File '{}'",
        hooks_dir.join("occlaw-cursor-hook.ps1").to_string_lossy());

    // Cursor's actual supported hook events (as of 2026-04):
    // - beforeShellExecution, beforeMCPExecution: permission hooks (need {"permission":"allow"})
    // - afterFileEdit, beforeReadFile: notification hooks
    // - beforeSubmitPrompt: gating hook (needs {"continue":true})
    // - stop: notification hook
    // NOTE: preToolUse/postToolUse/sessionStart/sessionEnd/subagentStart/subagentStop
    // are NOT supported by Cursor — those are Claude Code events.
    let cursor_events = [
        "beforeSubmitPrompt", "stop",
        "beforeShellExecution", "afterShellExecution",
        "beforeMCPExecution", "afterMCPExecution",
        "afterFileEdit", "beforeReadFile",
        "afterAgentThought", "afterAgentResponse",
    ];
    let marker = "occlaw-cursor-hook";

    let hooks = config["hooks"].as_object_mut().ok_or("hooks is not an object")?;

    // Clean up our hook from old event names that Cursor doesn't actually support.
    // Previous versions incorrectly registered CC-only events like preToolUse, sessionStart, etc.
    let stale_events = [
        "sessionStart", "sessionEnd", "preToolUse", "postToolUse",
        "postToolUseFailure", "subagentStart", "subagentStop", "preCompact",
    ];
    for stale in &stale_events {
        if let Some(arr) = hooks.get_mut(*stale).and_then(|v| v.as_array_mut()) {
            arr.retain(|entry| {
                !entry.get("command").and_then(|c| c.as_str())
                    .map(|c| c.contains(marker))
                    .unwrap_or(false)
            });
        }
    }

    for event_name in &cursor_events {
        let arr = hooks.entry(event_name.to_string())
            .or_insert_with(|| serde_json::json!([]))
            .as_array_mut()
            .ok_or("hook event is not an array")?;

        let existing_idx = arr.iter().position(|entry| {
            entry.get("command").and_then(|c| c.as_str())
                .map(|c| c.contains(marker))
                .unwrap_or(false)
        });

        let entry = serde_json::json!({"command": hook_command});
        if let Some(idx) = existing_idx {
            arr[idx] = entry;
        } else {
            arr.push(entry);
        }
    }

    let json_str = serde_json::to_string_pretty(&config).map_err(|e| e.to_string())?;
    std::fs::write(&hooks_json_path, json_str).map_err(|e| e.to_string())?;

    log::info!("[cursor_hooks] installed hooks to {:?}", hooks_json_path);

    // ── Sync oc-claw terminal-focus extension for Cursor ──
    // The extension exposes a tiny localhost API per Cursor window:
    // - GET  /window-meta  → workspace roots + focus state + bound port
    // - POST /focus-window → surface that specific Cursor window
    // We intentionally overwrite the installed files on every startup so
    // extension changes take effect after the user reloads Cursor windows.
    let ext_id = "oc-claw.terminal-focus";
    let ext_dir = home.join(".cursor").join("extensions").join(format!("{}-1.0.0", ext_id));
    log::info!("[cursor_hooks] syncing terminal-focus extension...");

    // Locate extension source with multiple fallbacks:
    // - repo/dev layout
    // - unpacked release binary layout
    // - macOS app bundle Resources
    let ext_source = {
        let mut candidates = Vec::new();

        if let Ok(exe) = std::env::current_exe() {
            let mut dir = exe.parent();
            for _ in 0..10 {
                if let Some(d) = dir {
                    let repo_candidate = d.join("extensions").join("cursor");
                    if repo_candidate.join("extension.js").exists() {
                        candidates.push(repo_candidate);
                        break;
                    }

                    let bundled_candidate = d.join("Resources").join("extensions").join("cursor");
                    if bundled_candidate.join("extension.js").exists() {
                        candidates.push(bundled_candidate);
                        break;
                    }

                    dir = d.parent();
                } else {
                    break;
                }
            }
        }

        if let Ok(manifest_dir) = std::env::var("CARGO_MANIFEST_DIR") {
            let repo_candidate = PathBuf::from(manifest_dir)
                .join("..")
                .join("..")
                .join("extensions")
                .join("cursor");
            if repo_candidate.join("extension.js").exists() {
                candidates.push(repo_candidate);
            }
        }

        candidates.into_iter().next()
    };

    if let Some(src) = ext_source {
        if let Err(e) = std::fs::create_dir_all(&ext_dir) {
            log::warn!("[cursor_hooks] failed to create extension dir: {}", e);
        } else {
            let files = ["package.json", "extension.js", "icon.png", "README.md"];
            let mut ok = true;
            for fname in &files {
                let from = src.join(fname);
                let to = ext_dir.join(fname);
                if let Err(e) = std::fs::copy(&from, &to) {
                    log::warn!("[cursor_hooks] failed to copy {}: {}", fname, e);
                    ok = false;
                }
            }
            if ok {
                // If the user previously uninstalled this extension in Cursor,
                // Cursor records it in ~/.cursor/extensions/.obsolete and keeps
                // hiding it even when files are copied back. Clear that flag.
                let obsolete_path = home.join(".cursor").join("extensions").join(".obsolete");
                let ext_folder_name = format!("{}-1.0.0", ext_id);
                if obsolete_path.exists() {
                    match std::fs::read_to_string(&obsolete_path) {
                        Ok(content) => {
                            if let Ok(mut v) = serde_json::from_str::<serde_json::Value>(&content) {
                                if let Some(obj) = v.as_object_mut() {
                                    if obj.remove(&ext_folder_name).is_some() {
                                        match serde_json::to_string(obj) {
                                            Ok(s) => {
                                                if let Err(e) = std::fs::write(&obsolete_path, s) {
                                                    log::warn!("[cursor_hooks] failed to update .obsolete: {}", e);
                                                } else {
                                                    log::info!("[cursor_hooks] removed obsolete flag for {}", ext_folder_name);
                                                }
                                            }
                                            Err(e) => {
                                                log::warn!("[cursor_hooks] failed to serialize .obsolete: {}", e);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            log::warn!("[cursor_hooks] failed to read .obsolete: {}", e);
                        }
                    }
                }

                // Ensure Cursor extension registry includes this local extension.
                // Some Cursor builds rely on extensions.json for listing/loading.
                let extensions_json_path = home.join(".cursor").join("extensions").join("extensions.json");
                let ext_version = "1.0.0";
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                let registry_entry = serde_json::json!({
                    "identifier": { "id": ext_id },
                    "version": ext_version,
                    "location": {
                        "$mid": 1,
                        "path": ext_dir.to_string_lossy().to_string(),
                        "scheme": "file"
                    },
                    "relativeLocation": format!("{}-{}", ext_id, ext_version),
                    "metadata": {
                        "installedTimestamp": now_ms,
                        "pinned": false,
                        "source": "vsix"
                    }
                });
                let mut updated_registry = false;
                let mut registry_val: serde_json::Value = if extensions_json_path.exists() {
                    match std::fs::read_to_string(&extensions_json_path) {
                        Ok(content) => serde_json::from_str(&content).unwrap_or(serde_json::json!([])),
                        Err(_) => serde_json::json!([]),
                    }
                } else {
                    serde_json::json!([])
                };
                if !registry_val.is_array() {
                    registry_val = serde_json::json!([]);
                }
                if let Some(arr) = registry_val.as_array_mut() {
                    let mut found = false;
                    for item in arr.iter_mut() {
                        let item_id = item.get("identifier")
                            .and_then(|v| v.get("id"))
                            .and_then(|v| v.as_str());
                        if item_id == Some(ext_id) {
                            *item = registry_entry.clone();
                            found = true;
                            break;
                        }
                    }
                    if !found {
                        arr.push(registry_entry);
                    }
                    updated_registry = true;
                }
                if updated_registry {
                    match serde_json::to_string(&registry_val) {
                        Ok(s) => {
                            if let Err(e) = std::fs::write(&extensions_json_path, s) {
                                log::warn!("[cursor_hooks] failed to update extensions.json: {}", e);
                            } else {
                                log::info!("[cursor_hooks] registered extension {} in extensions.json", ext_id);
                            }
                        }
                        Err(e) => {
                            log::warn!("[cursor_hooks] failed to serialize extensions.json: {}", e);
                        }
                    }
                }
                log::info!("[cursor_hooks] terminal-focus extension synced at {:?}", ext_dir);
            }
        }
    } else {
        log::warn!("[cursor_hooks] extension source not found, skipping sync");
    }

    Ok(())
}

/// Start the Cursor IPC server.
/// On macOS/Linux: Unix domain socket at /tmp/occlaw-cursor.sock
/// On Windows: TCP server on localhost:19284
fn start_cursor_socket_server(
    claude_state: Arc<Mutex<HashMap<String, ClaudeSession>>>,
    app: tauri::AppHandle,
) {
    #[cfg(unix)]
    {
        let socket_path = "/tmp/occlaw-cursor.sock";
        let _ = std::fs::remove_file(socket_path);
        let listener = match std::os::unix::net::UnixListener::bind(socket_path) {
            Ok(l) => l,
            Err(e) => { log::warn!("[cursor_socket] bind failed: {}", e); return; }
        };
        log::info!("[cursor_socket] listening on {}", socket_path);

        let state = Arc::clone(&claude_state);
        let app2 = app.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    let state = Arc::clone(&state);
                    let app = app2.clone();
                    std::thread::spawn(move || {
                        use std::io::Read;
                        let mut buf = String::new();
                        let _ = stream.read_to_string(&mut buf);
                        if !buf.is_empty() {
                            // Cursor events never block (no PermissionRequest)
                            process_claude_event(&buf, &state, &app, Some("cursor"));
                        }
                    });
                }
            }
        });
    }

    #[cfg(windows)]
    {
        let listener = match std::net::TcpListener::bind("127.0.0.1:19284") {
            Ok(l) => l,
            Err(e) => { log::warn!("[cursor_socket] TCP bind failed: {}", e); return; }
        };
        log::info!("[cursor_socket] listening on 127.0.0.1:19284");

        let state = Arc::clone(&claude_state);
        let app2 = app.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    let state = Arc::clone(&state);
                    let app = app2.clone();
                    std::thread::spawn(move || {
                        use std::io::Read;
                        let mut buf = String::new();
                        let _ = stream.read_to_string(&mut buf);
                        if !buf.is_empty() {
                            process_claude_event(&buf, &state, &app, Some("cursor"));
                        }
                    });
                }
            }
        });
    }
}

/// Start the Claude IPC server.
/// On macOS/Linux: Unix domain socket at /tmp/ooclaw-claude.sock
/// On Windows: TCP server on localhost:19283
fn start_claude_socket_server(
    claude_state: Arc<Mutex<HashMap<String, ClaudeSession>>>,
    pending_permissions: PendingPermissions,
    app_handle: tauri::AppHandle,
) {
    #[cfg(unix)]
    {
        let state = claude_state;
        let pending = pending_permissions;
        let app = app_handle;
        std::thread::spawn(move || {
            let sock_path = "/tmp/ooclaw-claude.sock";
            let _ = std::fs::remove_file(sock_path);

            let listener = match std::os::unix::net::UnixListener::bind(sock_path) {
                Ok(l) => l,
                Err(e) => { log::error!("Failed to bind claude socket: {}", e); return; }
            };
            log::info!("Claude socket server listening on {}", sock_path);

            for stream in listener.incoming() {
                match stream {
                    Ok(s) => {
                        let state = state.clone();
                        let app = app.clone();
                        let pending = pending.clone();
                        std::thread::spawn(move || {
                            use std::io::{Read, Write};
                            let mut s = s;
                            let mut buf = String::new();
                            let _ = s.read_to_string(&mut buf);
                            if let Some((session_id, hook_event)) = process_claude_event(&buf, &state, &app, None) {
                                if hook_event == "PermissionRequest" {
                                    let (tx, rx) = std::sync::mpsc::channel::<String>();
                                    {
                                        let mut map = pending.lock().unwrap();
                                        map.insert(session_id.clone(), tx);
                                    }
                                    log::info!("[claude_socket] blocking for PermissionRequest session={}", &session_id[..session_id.len().min(8)]);
                                    match rx.recv_timeout(std::time::Duration::from_secs(600)) {
                                        Ok(response_json) => {
                                            log::info!("[claude_socket] sending permission response for session={}", &session_id[..session_id.len().min(8)]);
                                            let _ = s.write_all(response_json.as_bytes());
                                            let _ = s.flush();
                                        }
                                        Err(_) => {
                                            log::warn!("[claude_socket] permission timeout for session={}", &session_id[..session_id.len().min(8)]);
                                        }
                                    }
                                    let mut map = pending.lock().unwrap();
                                    map.remove(&session_id);
                                }
                            }
                        });
                    }
                    Err(e) => { log::error!("Claude socket accept error: {}", e); }
                }
            }
        });
    }

    #[cfg(windows)]
    {
        let state = claude_state;
        let pending = pending_permissions;
        let app = app_handle;
        std::thread::spawn(move || {
            use std::net::TcpListener;
            let listener = match TcpListener::bind("127.0.0.1:19283") {
                Ok(l) => l,
                Err(e) => { log::error!("Failed to bind claude TCP socket: {}", e); return; }
            };
            log::info!("Claude TCP server listening on 127.0.0.1:19283");

            for stream in listener.incoming() {
                match stream {
                    Ok(mut s) => {
                        let state = state.clone();
                        let app = app.clone();
                        let pending = pending.clone();
                        std::thread::spawn(move || {
                            use std::io::{Read, Write};
                            s.set_read_timeout(Some(std::time::Duration::from_secs(5))).ok();
                            let mut buf = Vec::new();
                            let mut chunk = [0u8; 4096];
                            loop {
                                match s.read(&mut chunk) {
                                    Ok(0) => break,
                                    Ok(n) => buf.extend_from_slice(&chunk[..n]),
                                    Err(e) => {
                                        if !buf.is_empty() { break; }
                                        log::warn!("[claude_tcp] read error with empty buf: {}", e);
                                        return;
                                    }
                                }
                            }
                            let text = String::from_utf8_lossy(&buf);
                            if let Some((session_id, hook_event)) = process_claude_event(&text, &state, &app, None) {
                                if hook_event == "PermissionRequest" {
                                    let (tx, rx) = std::sync::mpsc::channel::<String>();
                                    {
                                        let mut map = pending.lock().unwrap();
                                        map.insert(session_id.clone(), tx);
                                    }
                                    s.set_read_timeout(None).ok();
                                    match rx.recv_timeout(std::time::Duration::from_secs(600)) {
                                        Ok(response_json) => {
                                            let _ = s.write_all(response_json.as_bytes());
                                            let _ = s.flush();
                                        }
                                        Err(_) => {
                                            log::warn!("[claude_tcp] permission timeout for session={}", &session_id[..session_id.len().min(8)]);
                                        }
                                    }
                                    let mut map = pending.lock().unwrap();
                                    map.remove(&session_id);
                                }
                            }
                        });
                    }
                    Err(e) => { log::error!("Claude TCP accept error: {}", e); }
                }
            }
        });
    }
}

#[cfg(target_os = "macos")]
fn install_wry_webview_ime_fix() {
    use std::ffi::CString;
    use std::sync::Once;

    use objc2::ffi;
    use objc2::runtime::{AnyClass, AnyObject, AnyProtocol, Imp, Sel};
    use objc2::{msg_send, sel};

    static INSTALL_ONCE: Once = Once::new();

    unsafe extern "C-unwind" fn window_level(this: &AnyObject, _cmd: Sel) -> isize {
        let window: *mut AnyObject = unsafe { msg_send![this, window] };
        if window.is_null() {
            0
        } else {
            unsafe { msg_send![&*window, level] }
        }
    }

    fn patch_class(class_name: &'static std::ffi::CStr, text_input_protocol: Option<&'static AnyProtocol>) {
        let Some(cls) = AnyClass::get(class_name) else {
            log::warn!("[ime] class not found: {}", class_name.to_string_lossy());
            return;
        };

        let cls_ptr = cls as *const AnyClass as *mut AnyClass;
        let type_encoding = CString::new("q@:").unwrap();
        unsafe {
            if let Some(protocol) = text_input_protocol {
                let _ = ffi::class_addProtocol(cls_ptr, protocol);
            }
            let _ = ffi::class_addMethod(
                cls_ptr,
                sel!(windowLevel),
                std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyObject, Sel) -> isize, Imp>(window_level),
                type_encoding.as_ptr(),
            );
        }
    }

    INSTALL_ONCE.call_once(|| {
        let text_input_protocol = AnyProtocol::get(c"NSTextInputClient");
        patch_class(c"WryWebView", text_input_protocol);
        patch_class(c"WKWebView", text_input_protocol);
    });
}

fn asset_mime_for_path(path: &str) -> &'static str {
    let lower = path.to_ascii_lowercase();
    if lower.ends_with(".gif") {
        "image/gif"
    } else if lower.ends_with(".png") {
        "image/png"
    } else if lower.ends_with(".jpg") || lower.ends_with(".jpeg") {
        "image/jpeg"
    } else if lower.ends_with(".webp") {
        "image/webp"
    } else if lower.ends_with(".webm") {
        "video/webm"
    } else if lower.ends_with(".mp4") {
        "video/mp4"
    } else if lower.ends_with(".mov") {
        "video/quicktime"
    } else {
        "application/octet-stream"
    }
}

fn build_asset_response(
    req: &tauri::http::Request<Vec<u8>>,
    path: &str,
    file_path: &std::path::Path,
    add_cors: bool,
    log_label: &str,
) -> tauri::http::Response<Vec<u8>> {
    match std::fs::read(file_path) {
        Ok(data) => {
            let mime = asset_mime_for_path(path);
            let total_len = data.len();
            let mut status = 200;
            let mut body = data;
            let mut content_range: Option<String> = None;

            // Serve byte ranges for media files so WKWebView/Safari can stream
            // video containers like HEVC .mov/.mp4 reliably.
            if total_len > 0 {
                if let Some(range_header) = req.headers().get("Range").or_else(|| req.headers().get("range")) {
                    if let Ok(range) = range_header.to_str() {
                        if let Some(spec) = range.strip_prefix("bytes=") {
                            let mut parts = spec.splitn(2, '-');
                            let start_part = parts.next().unwrap_or("");
                            let end_part = parts.next().unwrap_or("");
                            let parsed = if start_part.is_empty() {
                                end_part.parse::<usize>().ok().map(|suffix_len| {
                                    let suffix_len = suffix_len.min(total_len);
                                    let start = total_len.saturating_sub(suffix_len);
                                    (start, total_len.saturating_sub(1))
                                })
                            } else if let Ok(start) = start_part.parse::<usize>() {
                                let end = if end_part.is_empty() {
                                    total_len.saturating_sub(1)
                                } else {
                                    end_part
                                        .parse::<usize>()
                                        .unwrap_or(total_len.saturating_sub(1))
                                        .min(total_len.saturating_sub(1))
                                };
                                if start < total_len && start <= end {
                                    Some((start, end))
                                } else {
                                    None
                                }
                            } else {
                                None
                            };

                            if let Some((start, end)) = parsed {
                                body = body[start..=end].to_vec();
                                status = 206;
                                content_range = Some(format!("bytes {}-{}/{}", start, end, total_len));
                            }
                        }
                    }
                }
            }

            let mut resp = tauri::http::Response::builder()
                .status(status)
                .header("Content-Type", mime)
                .header("Content-Length", body.len().to_string())
                .header("Accept-Ranges", "bytes");
            if let Some(content_range) = content_range {
                resp = resp.header("Content-Range", content_range);
            }
            if add_cors {
                resp = resp.header("Access-Control-Allow-Origin", "*");
            }
            resp.body(body).unwrap()
        }
        Err(e) => {
            log::warn!("[{}] 404: {} err={}", log_label, file_path.display(), e);
            tauri::http::Response::builder()
                .status(404)
                .body(Vec::new())
                .unwrap()
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .register_uri_scheme_protocol("localasset", |ctx, req| {
            let raw_path = req.uri().path();
            let path = percent_decode_str(raw_path).decode_utf8_lossy();
            let resource_dir = ctx.app_handle().path().resource_dir().unwrap_or_default();
            let file_path = resource_dir.join("assets").join("builtin").join(path.trim_start_matches('/'));
            log::info!("[localasset] request={} resolved={}", raw_path, file_path.display());
            build_asset_response(&req, path.as_ref(), &file_path, cfg!(target_os = "windows"), "localasset")
        })
        .register_uri_scheme_protocol("customasset", |ctx, req| {
            let raw_path = req.uri().path();
            // Percent-decode path for Chinese character names etc.
            let path = percent_decode_str(raw_path).decode_utf8_lossy();
            let data_dir = ctx.app_handle().path().app_data_dir().unwrap_or_default();
            let file_path = data_dir.join("characters").join(path.trim_start_matches('/'));
            build_asset_response(&req, path.as_ref(), &file_path, cfg!(target_os = "windows"), "customasset")
        })
        .setup(|app| {
            // Fix PATH so openclaw (Node.js script) and node are both reachable
            fix_path();
            #[cfg(target_os = "macos")]
            install_wry_webview_ime_fix();

            // Install Claude + Codex hooks on every startup (idempotent)
            if let Err(e) = tauri::async_runtime::block_on(install_claude_hooks()) {
                log::warn!("Failed to install Claude hooks on startup: {}", e);
            }
            // Install Cursor hooks + terminal-focus extension on startup (idempotent)
            if let Err(e) = tauri::async_runtime::block_on(install_cursor_hooks()) {
                log::warn!("Failed to install Cursor hooks on startup: {}", e);
            }

            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .build(),
            )?;

            // Accessibility permission is no longer requested automatically on
            // startup. Cursor window raising is handled by the extension running
            // inside the Cursor process itself, which doesn't need AX permission.
            // The check_ax_permission / request_ax_permission commands remain
            // available for the frontend to invoke if needed.

            // Hide from Dock, show only in menu bar (macOS only)
            #[cfg(target_os = "macos")]
            {
                use objc2::runtime::{AnyClass, AnyObject};
                use objc2::msg_send;
                unsafe {
                    let ns_app_cls = AnyClass::get(c"NSApplication").unwrap();
                    let ns_app: *mut AnyObject = msg_send![ns_app_cls, sharedApplication];
                    // NSApplicationActivationPolicyAccessory = 1
                    let _: () = msg_send![ns_app, setActivationPolicy: 1i64];
                }
            }

            // Position mini window initially
            #[cfg(target_os = "macos")]
            if let Some(win) = app.get_webview_window("mini") {
                let win_clone = win.clone();
                let _ = app.handle().run_on_main_thread(move || {
                    use objc2::runtime::{AnyClass, AnyObject};
                    use objc2::msg_send;
                    use objc2_foundation::{NSRect, NSPoint, NSSize};

                    if let Ok(ns_win) = win_clone.ns_window() {
                        let obj = unsafe { &*(ns_win as *mut AnyObject) };

                        unsafe {
                            let _: () = msg_send![obj, setLevel: 27isize];
                            let behavior: usize = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 6);
                            let _: () = msg_send![obj, setCollectionBehavior: behavior];
                        }

                        let screen_info: Option<(f64, f64, f64, f64, f64)> = unsafe {
                            let cls = match AnyClass::get(c"NSScreen") {
                                Some(c) => c,
                                None => return,
                            };
                            let screens: *mut AnyObject = msg_send![cls, screens];
                            if screens.is_null() { return; }
                            let count: usize = msg_send![&*screens, count];
                            if count == 0 { return; }
                            let screen: *mut AnyObject = msg_send![&*screens, objectAtIndex: 0usize];
                            if screen.is_null() { return; }
                            let sf: NSRect = msg_send![&*screen, frame];
                            let notch_off = get_notch_offset(screen);
                            Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height, notch_off))
                        };

                        if let Some((sx, sy, sw, sh, notch_off)) = screen_info {
                            let (win_w, win_h) = collapsed_mascot_window_size(1.0);
                            let x = sx + sw / 2.0 + notch_off;
                            let y = sy + sh - win_h;
                            let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                            unsafe {
                                let _: () = msg_send![obj, setFrame: frame, display: true];
                                let _: () = msg_send![obj, orderFrontRegardless];
                            }
                        }
                    }
                });
            }

            // Windows: position mini window at top-center of primary monitor
            #[cfg(target_os = "windows")]
            if let Some(win) = app.get_webview_window("mini") {
                let _ = win.set_always_on_top(true);
                let _ = win.set_skip_taskbar(true);
                if let Ok(Some(monitor)) = win.primary_monitor() {
                    let screen = monitor.size();
                    let scale = monitor.scale_factor();
                    let sw = screen.width as f64 / scale;
                    let x = sw / 2.0 + 40.0;
                    let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
                }
                let _ = win.show();
            }

            // Windows: move window off-screen when a fullscreen app is on the SAME
            // monitor as the mini window.  We avoid hide()/show() because show()
            // triggers a focus event which causes the panel to expand.
            #[cfg(target_os = "windows")]
            {
                let app_handle = app.handle().clone();
                std::thread::spawn(move || {
                    use windows::Win32::Graphics::Gdi::{HMONITOR, MonitorFromPoint, MONITOR_DEFAULTTONEAREST};
                    use windows::Win32::Foundation::POINT;

                    let mut was_hidden = false;
                    let mut saved_pos: Option<tauri::LogicalPosition<f64>> = None;
                    let mut hidden_monitor: Option<HMONITOR> = None;
                    // Debounce counter: require several consecutive non-fullscreen
                    // polls before restoring, so brief foreground changes (mouse
                    // movement, overlay popups) during video playback don't cause
                    // the pet to flicker.
                    let mut non_fs_streak: u32 = 0;
                    const RESTORE_THRESHOLD: u32 = 4; // 4 × 500ms = 2s
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(500));
                        let fs_monitor = fullscreen_foreground_monitor();

                        if let Some(win) = app_handle.get_webview_window("mini") {
                            let tracked_monitor = if was_hidden {
                                hidden_monitor
                            } else if let Ok(pos) = win.outer_position() {
                                Some(unsafe {
                                    MonitorFromPoint(
                                        POINT { x: pos.x, y: pos.y },
                                        MONITOR_DEFAULTTONEAREST,
                                    )
                                })
                            } else {
                                None
                            };
                            let same_monitor = matches!(
                                (fs_monitor, tracked_monitor),
                                (Some(fs_mon), Some(mini_mon)) if mini_mon == fs_mon
                            );

                            if same_monitor {
                                non_fs_streak = 0;
                                if !was_hidden {
                                    log::info!("[fullscreen] detected fullscreen app on same monitor, moving mini off-screen");
                                    FULLSCREEN_HIDING.store(true, std::sync::atomic::Ordering::SeqCst);
                                    if let Ok(pos) = win.outer_position() {
                                        hidden_monitor = Some(unsafe {
                                            MonitorFromPoint(
                                                POINT { x: pos.x, y: pos.y },
                                                MONITOR_DEFAULTTONEAREST,
                                            )
                                        });
                                    }
                                    if let Ok(Some(pos)) = win.outer_position().map(|p| {
                                        win.current_monitor().ok().flatten().map(|m| {
                                            let s = m.scale_factor();
                                            tauri::LogicalPosition::new(p.x as f64 / s, p.y as f64 / s)
                                        })
                                    }) {
                                        saved_pos = Some(pos);
                                    }
                                    let _ = win.set_always_on_top(false);
                                    let _ = win.set_position(tauri::LogicalPosition::new(-9999.0_f64, -9999.0_f64));
                                    was_hidden = true;
                                }
                            } else if was_hidden {
                                non_fs_streak += 1;
                                if non_fs_streak >= RESTORE_THRESHOLD {
                                    log::info!("[fullscreen] fullscreen exited or on different monitor, restoring mini position");
                                    FULLSCREEN_HIDING.store(false, std::sync::atomic::Ordering::SeqCst);
                                    if let Some(pos) = saved_pos.take() {
                                        let _ = win.set_position(pos);
                                    }
                                    let _ = win.set_always_on_top(true);
                                    was_hidden = false;
                                    hidden_monitor = None;
                                    non_fs_streak = 0;
                                }
                            }
                        }
                    }
                });
            }

            // Start Claude Code socket server
            {
                let claude_state = app.state::<ClaudeState>();
                let sessions_arc = Arc::clone(&claude_state.sessions);
                let pending_arc = Arc::clone(&claude_state.pending_permissions);
                start_claude_socket_server(sessions_arc, pending_arc, app.handle().clone());
            }

            // Start Cursor socket server (shares ClaudeState for unified session tracking)
            {
                let claude_state = app.state::<ClaudeState>();
                let sessions_arc = Arc::clone(&claude_state.sessions);
                start_cursor_socket_server(sessions_arc, app.handle().clone());
            }

            // System tray — use saved language, fallback to system language
            let initial_lang = {
                let store_path = app.path().app_data_dir().ok().map(|p| p.join("settings.json"));
                let mut lang = None;
                if let Some(ref sp) = store_path {
                    if let Ok(data) = std::fs::read_to_string(sp) {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&data) {
                            lang = val.get("oc-claw-lang").and_then(|v| v.as_str()).map(|s| s.to_string());
                        }
                    }
                }
                lang.unwrap_or_else(|| {
                    let sys = std::env::var("LANG").unwrap_or_default().to_lowercase();
                    if sys.starts_with("zh") { "zh".into() }
                    else if sys.starts_with("ja") { "ja".into() }
                    else if sys.starts_with("ko") { "ko".into() }
                    else if sys.starts_with("es") { "es".into() }
                    else if sys.starts_with("fr") { "fr".into() }
                    else { "en".into() }
                })
            };
            let (show_label, hide_label, quit_label) = tray_labels(&initial_lang);
            let show = MenuItem::with_id(app, "show", show_label, true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", hide_label, true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", quit_label, true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &hide, &quit])?;

            // Use dedicated tray icon (logo-mini: white cat silhouette on transparent bg)
            // instead of the app icon, so it renders correctly in macOS menu bar / Windows tray
            let tray_icon_bytes = include_bytes!("../icons/tray-icon.png");
            let tray_icon = tauri::image::Image::from_bytes(tray_icon_bytes)
                .expect("failed to load tray icon");
            TrayIconBuilder::with_id("main")
                .icon(tray_icon)
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(win) = app.get_webview_window("mini") {
                            #[cfg(target_os = "windows")]
                            {
                                FULLSCREEN_HIDING.store(false, std::sync::atomic::Ordering::SeqCst);
                                if let Ok(Some(monitor)) = win.primary_monitor() {
                                    let scale = monitor.scale_factor();
                                    let sw = monitor.size().width as f64 / scale;
                                    let ui = win_ui_scale(&monitor);
                                    let x = sw / 2.0 + (80.0 * ui).round();
                                    let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
                                }
                                let _ = win.set_always_on_top(true);
                            }
                            let _ = win.show();
                            let _ = win.set_focus();
                        }
                    }
                    "hide" => {
                        if let Some(win) = app.get_webview_window("mini") {
                            let _ = win.hide();
                        }
                    }
                    "quit" => {
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![get_status, send_chat, open_detail_panel, save_character_gif, delete_character_assets, delete_character_gif, get_agents, get_health, get_agent_metrics, interrupt_agent, scan_characters, get_agent_extra_info, open_mini, close_mini, set_mini_expanded, set_mini_size, set_efficiency_hover_tracking, resize_mini_height, move_mini_by, get_mini_origin, set_mini_origin, set_ime_mode, get_agent_sessions, get_session_preview, get_session_messages, get_active_sessions, proxy_post, play_sound, get_claude_sessions, get_claude_conversation, install_claude_hooks, install_cursor_hooks, remove_claude_session, resolve_claude_permission, get_claude_stats, open_url, activate_app, focus_cursor_terminal, check_ax_permission, request_ax_permission, jump_to_claude_terminal, check_for_update, run_update, close_ssh, read_local_file, list_backgrounds, save_background, get_background_data, exit_app, get_ssh_key_info, reset_ssh, get_ui_scale, update_tray_language, set_pet_mode_window, set_pet_context_menu])
        .manage(ActiveAgentPid { pid: Mutex::new(None) })
        .manage(ClaudeState { sessions: Arc::new(Mutex::new(HashMap::new())), pending_permissions: Arc::new(Mutex::new(HashMap::new())) })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
