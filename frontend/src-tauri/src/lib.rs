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
/// Whether a pomodoro timer is currently active. When true the poll thread
/// keeps the entire window interactive so the bottom-anchored Pomodoro
/// stop button receives clicks (it sits in the centered hitbox's bottom
/// inset region and would otherwise pass through to whatever is behind).
static PET_POMODORO_ACTIVE: AtomicBool = AtomicBool::new(false);

/// Coalesces drag-apply tasks so we never queue more than one
/// setFrameOrigin: call on the main thread at a time. The poll thread
/// records the anchor (cursor-to-origin offset at drag start) once; each
/// scheduled main-thread task simply reads the live cursor position and
/// snaps the window origin to (cursor - anchor). This is the same pattern
/// macOS uses for native window dragging and avoids the lag introduced by
/// accumulating deltas across pre-empted frames.
static DRAG_TASK_PENDING: AtomicBool = AtomicBool::new(false);
static DRAG_ANCHOR: std::sync::OnceLock<Mutex<Option<(f64, f64)>>> = std::sync::OnceLock::new();
fn drag_anchor() -> &'static Mutex<Option<(f64, f64)>> {
    DRAG_ANCHOR.get_or_init(|| Mutex::new(None))
}

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
            // Skip codex sprite-pet directories. Those carry `pet.json` +
            // `spritesheet.{webp,png}` and are consumed by the frontend's
            // mini-mode pet loader directly, not by this anime-character
            // scan. Without this guard the 9 builtin codex pets would
            // pollute the IP/character lists with empty entries.
            if entry.path().join("pet.json").is_file() { continue; }
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
            char_obj.insert("name".into(), serde_json::Value::String(name.clone()));
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

            // Read audio.json if it exists: maps action names to audio file URLs
            let audio_json_path = entry.path().join("audio.json");
            if audio_json_path.exists() {
                if let Ok(data) = std::fs::read_to_string(&audio_json_path) {
                    if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&data) {
                        let mut audio_map: serde_json::Map<String, serde_json::Value> = serde_json::Map::new();
                        for (action, file_val) in &map {
                            if let Some(filename) = file_val.as_str() {
                                let audio_path = entry.path().join("audio").join(filename);
                                if audio_path.exists() {
                                    audio_map.insert(
                                        action.clone(),
                                        serde_json::Value::String(format!("{}/{}/audio/{}", url_prefix, name, filename)),
                                    );
                                }
                            }
                        }
                        if !audio_map.is_empty() {
                            char_obj.insert("audioMap".into(), serde_json::Value::Object(audio_map));
                        }
                    }
                }
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
        log::debug!("[scan_characters] scanning builtin: {} (exists={})", builtin_dir.display(), builtin_dir.exists());
        scan_dir(&builtin_dir, builtin_prefix, true, &ip_map, &mut results);
        log::debug!("[scan_characters] found {} characters after builtin scan", results.len());
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
        log::debug!("[scan_characters] scanning custom: {} (exists={})", custom_dir.display(), custom_dir.exists());
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
    log::debug!("[mini-pos] open_mini called");
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
                        // Pull the window down by MASCOT_TOP_INSET so it
                        // does not sit under the menu bar / notch on launch.
                        let y = sy + sh - win_h - MASCOT_TOP_INSET;
                        log::debug!(
                            "[mini-pos] open_mini(existing,mac) target frame x={:.1} y={:.1} w={:.1} h={:.1} inset={:.1} screen=({:.1},{:.1},{:.1},{:.1}) notch_off={:.1}",
                            x, y, win_w, win_h, MASCOT_TOP_INSET, sx, sy, sw, sh, notch_off
                        );
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
                log::debug!(
                    "[mini-pos] open_mini(existing,win) target pos x={:.1} y={:.1} w={:.1} h={:.1} ui={:.2} notch_off={:.1}",
                    x, 0.0, win_w, win_h, ui, notch_off
                );
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
                    // Pull the window down by MASCOT_TOP_INSET so the sprite
                    // is fully visible below the menu bar / notch on launch.
                    let y = sy + sh - win_h - MASCOT_TOP_INSET;
                    log::debug!(
                        "[mini-pos] open_mini(new,mac) target frame x={:.1} y={:.1} w={:.1} h={:.1} inset={:.1} screen=({:.1},{:.1},{:.1},{:.1}) notch_off={:.1}",
                        x, y, win_w, win_h, MASCOT_TOP_INSET, sx, sy, sw, sh, notch_off
                    );
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
            log::debug!(
                "[mini-pos] open_mini(new,win) target pos x={:.1} y={:.1} w={:.1} h={:.1} ui={:.2} notch_off={:.1}",
                x, 0.0, win_w, win_h, ui, notch_off
            );
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

// Bumped from 60x45 so the codex sprite-pet (rendered at ~86x93 CSS px due
// to the MINI_SPRITE_DISPLAY_MULTIPLIER=2 used in Mini.tsx) fits entirely
// inside the native window. Without the extra room the sprite gets clipped
// at the bottom/right edges of the OS-level mascot window.
const COLLAPSED_MASCOT_BASE_W: f64 = 96.0;
const COLLAPSED_MASCOT_BASE_H: f64 = 96.0;
// Vertical inset applied to the default mascot position so the sprite is
// always rendered below the macOS menu bar / notch (or the equivalent top
// chrome on Windows). Covers both notched (~38pt) and non-notched (~24pt)
// menu bars with extra breathing room.
const MASCOT_TOP_INSET: f64 = 120.0;
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

/// Return the monitor rect (x, y, w, h) in logical pixels for the monitor
/// the mini window currently lives on.  Used by the front-end to detect
/// screen edges correctly on multi-monitor setups.
#[tauri::command]
async fn get_mini_monitor_rect(app: tauri::AppHandle) -> Result<(f64, f64, f64, f64), String> {
    let win = app.get_webview_window("mini").ok_or("mini not found")?;
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let win_clone = win.clone();
        app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::NSRect;
            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
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
                let _ = tx.send((
                    screen_frame.origin.x,
                    screen_frame.origin.y,
                    screen_frame.size.width,
                    screen_frame.size.height,
                ));
            }
        }).map_err(|e| e.to_string())?;
        if let Ok(rect) = rx.recv_timeout(std::time::Duration::from_secs(1)) {
            return Ok(rect);
        }
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(Some(monitor)) = win.current_monitor() {
            let pos = monitor.position();
            let size = monitor.size();
            let scale = win.scale_factor().unwrap_or(1.0);
            return Ok((
                pos.x as f64 / scale,
                pos.y as f64 / scale,
                size.width as f64 / scale,
                size.height as f64 / scale,
            ));
        }
    }
    Err("failed to get monitor rect".into())
}

/// Set the mini window's origin in logical coordinates.
/// macOS: bottom-left origin. Windows: top-left origin.
///
/// `confine` (default true) clamps the target into the current monitor's
/// rect. Set `Some(false)` for live drag flows so the user can pull the
/// mascot across to a neighbouring monitor — the per-monitor clamp here
/// is what previously made cross-monitor drag impossible.
#[tauri::command]
async fn set_mini_origin(
    app: tauri::AppHandle,
    x: f64,
    y: f64,
    confine: Option<bool>,
) -> Result<(), String> {
    let confine = confine.unwrap_or(true);
    log::debug!(
        "[mini-pos] set_mini_origin request x={:.1} y={:.1} confine={}",
        x, y, confine
    );
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
                let (clamped_x, clamped_y) = if confine {
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
                    // Keep collapsed mascot windows below top chrome. This also
                    // prevents stale persisted positions from parking the window
                    // under the notch/menu bar after startup.
                    let max_y = (screen_frame.origin.y + screen_frame.size.height - frame.size.height - MASCOT_TOP_INSET).max(min_y);
                    let cx = x.max(min_x).min(max_x);
                    let cy = y.max(min_y).min(max_y);
                    log::debug!(
                        "[mini-pos] set_mini_origin(mac) clamped x={:.1}->{:.1} y={:.1}->{:.1} bounds x[{:.1},{:.1}] y[{:.1},{:.1}]",
                        x, cx, y, cy, min_x, max_x, min_y, max_y
                    );
                    (cx, cy)
                } else {
                    log::debug!(
                        "[mini-pos] set_mini_origin(mac) unconfined x={:.1} y={:.1}",
                        x, y
                    );
                    (x, y)
                };
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
        if !confine {
            log::debug!(
                "[mini-pos] set_mini_origin(win) unconfined x={:.1} y={:.1}",
                x, y
            );
            let _ = win.set_position(tauri::LogicalPosition::new(x, y));
            return Ok(());
        }
        if let Ok(Some(monitor)) = win.current_monitor() {
            let scale = monitor.scale_factor();
            let mp = monitor.position();
            let mx = mp.x as f64 / scale;
            let my = mp.y as f64 / scale;
            let sw = monitor.size().width as f64 / scale;
            let sh = monitor.size().height as f64 / scale;
            let ui = win_ui_scale(&monitor);
            let (ww, wh) = win
                .outer_size()
                .map(|s| (s.width as f64 / scale, s.height as f64 / scale))
                .unwrap_or((0.0, 0.0));
            let min_x = mx;
            let max_x = (mx + sw - ww).max(min_x);
            let min_y = my + (MASCOT_TOP_INSET * ui).round();
            let max_y = (my + sh - wh).max(min_y);
            let clamped_x = x.max(min_x).min(max_x);
            let clamped_y = y.max(min_y).min(max_y);
            log::debug!(
                "[mini-pos] set_mini_origin(win) clamped x={:.1}->{:.1} y={:.1}->{:.1} bounds x[{:.1},{:.1}] y[{:.1},{:.1}]",
                x, clamped_x, y, clamped_y, min_x, max_x, min_y, max_y
            );
            let _ = win.set_position(tauri::LogicalPosition::new(clamped_x, clamped_y));
        } else {
            log::debug!(
                "[mini-pos] set_mini_origin(win,fallback) apply x={:.1} y={:.1} (with inset)",
                x, y + MASCOT_TOP_INSET
            );
            let _ = win.set_position(tauri::LogicalPosition::new(x, y + MASCOT_TOP_INSET));
        }
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
    log::debug!(
        "[mini-pos] set_mini_expanded request expanded={} pos={} efficiency={:?} keep_position={:?} large_mascot={:?} mascot_scale={:.2} large_scale={:.2}",
        expanded, pos, efficiency, keep_position, large_mascot, mascot_scale, large_mascot_scale
    );

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
                        // Expanded panel hugs the top of the screen (its window
                        // level is high enough to draw over the menu bar). The
                        // MASCOT_TOP_INSET only applies to the collapsed mascot
                        // so it stays clear of the notch.
                        let y = sy + sh - win_h;
                        log::debug!(
                            "[mini-pos] set_mini_expanded(mac,expanded) frame x={:.1} y={:.1} w={:.1} h={:.1}",
                            x, y, win_w, win_h
                        );
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
                        let (mut x, mut y) = if keep_position.unwrap_or(false) {
                            let cur: NSRect = unsafe { msg_send![obj, frame] };
                            (cur.origin.x, cur.origin.y + cur.size.height - win_h)
                        } else if large_mascot.unwrap_or(false) {
                            let margin_x = 10.0;
                            let margin_y = 300.0;
                            (sx + sw - win_w - margin_x, sy + margin_y)
                        } else {
                            (
                                collapsed_x(sx, sw, win_w, &pos, notch_off),
                                sy + sh - win_h - MASCOT_TOP_INSET,
                            )
                        };
                        if !large_mascot.unwrap_or(false) {
                            let max_y = sy + sh - win_h - MASCOT_TOP_INSET;
                            if y > max_y { y = max_y; }
                        }
                        log::debug!(
                            "[mini-pos] set_mini_expanded(mac,collapsed) frame x={:.1} y={:.1} w={:.1} h={:.1} keep_position={}",
                            x, y, win_w, win_h, keep_position.unwrap_or(false)
                        );
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
                // Expanded panel hugs the top of the monitor (no inset) so it
                // does not get pushed below the IDE chrome.
                let y = my;
                log::debug!(
                    "[mini-pos] set_mini_expanded(win,expanded) frame x={:.1} y={:.1} w={:.1} h={:.1} ui={:.2}",
                    x, y, win_w, win_h, ui
                );
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_position(tauri::LogicalPosition::new(x, y));
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
                        let y = my + (MASCOT_TOP_INSET * ui).round();
                        log::debug!(
                            "[mini-pos] set_mini_expanded(win,collapsed) frame x={:.1} y={:.1} w={:.1} h={:.1} keep_position={}",
                            x, y, win_w, win_h, keep_position.unwrap_or(false)
                        );
                        let _ = win.set_position(tauri::LogicalPosition::new(x, y));
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
    let mut was_over_mascot = false;
    let mut last_enter_emit = Instant::now();
    // Drag state machine, driven entirely by NSEvent.pressedMouseButtons +
    // NSEvent.mouseLocation. The webview cannot observe mouseDown on a
    // non-key floating window, so the JS-side drag would otherwise need a
    // priming click. We mirror codex's approach: poll cursor + button,
    // translate the mini NSWindow ourselves, and emit walk-dir events to
    // the frontend so the codex sprite shows run-left/run-right.
    let mut drag_active = false;
    let mut last_cursor: (f64, f64) = (0.0, 0.0);
    let mut last_walk_dir: i32 = 0;
    let mut was_pressed = false;
    // Used only for run-left/right detection — measured between successive
    // poll iterations. Window translation itself is anchor-based and lives
    // in request_drag_apply (which reads the live cursor on main thread).

    while EFFICIENCY_HOVER_ACTIVE.load(Ordering::SeqCst) {
        let info = NOTCH_SCREEN_INFO.lock().ok().and_then(|g| *g);
        let sleep_ms = if let Some((sx, sy, sw, sh, notch_off)) = info {
            let cursor = macos_cursor_position();
            let buttons = macos_pressed_mouse_buttons();
            let left_pressed = (buttons & 1) != 0;
            let is_expanded = EFFICIENCY_EXPANDED.load(Ordering::SeqCst);
            let frame = MINI_WINDOW_FRAME.lock().ok().and_then(|g| *g);

            let inside = if is_expanded {
                if let Some((fx, fy, fw, fh)) = frame {
                    cursor.0 >= fx && cursor.0 <= fx + fw
                        && cursor.1 >= fy && cursor.1 <= fy + fh
                } else {
                    false
                }
            } else {
                let rw = (notch_off * 2.0 + 10.0).max(80.0);
                let rh = frame
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

            // ── Mascot body hit-test ──
            // Use a tighter rect than the full 96x96 window: the codex
            // 192x208 cell paints the character roughly in its centre with
            // transparent margins (and the status badge lives in the
            // bottom-right corner). Hover/drag should only fire on the
            // visible body, so we inset to ~35% wide x 65% tall around the
            // upper-centre where the head/torso sit.
            let over_mascot = if is_expanded {
                false
            } else if let Some((fx, fy, fw, fh)) = frame {
                let l = fx + fw * 0.32;
                let r = fx + fw * 0.68;
                let b = fy + fh * 0.25; // NSEvent y axis grows upward
                let t = fy + fh * 0.90;
                cursor.0 >= l && cursor.0 <= r && cursor.1 >= b && cursor.1 <= t
            } else {
                false
            };

            // ── Drag state machine ──
            // Only engage in collapsed (mascot) state, never in expanded
            // panel mode (clicks inside the panel must keep their normal
            // webview behavior).
            if !is_expanded {
                if drag_active {
                    if left_pressed {
                        // Always request a fresh window-snap; the main-thread
                        // task reads cursor position itself, so even if many
                        // requests collapse into one, the window still ends
                        // up under the live cursor.
                        request_drag_apply(&app);
                        let dx = cursor.0 - last_cursor.0;
                        last_cursor = cursor;
                        let walk_dir = if dx > 0.5 { 1 } else if dx < -0.5 { -1 } else { last_walk_dir };
                        if walk_dir != last_walk_dir {
                            let _ = app.emit("mini-mascot-walk", walk_dir);
                            last_walk_dir = walk_dir;
                        }
                    } else {
                        // Drag finished. Clear anchor + walk dir and notify
                        // the frontend so it can persist the new origin.
                        drag_active = false;
                        if let Ok(mut a) = drag_anchor().lock() {
                            *a = None;
                        }
                        if last_walk_dir != 0 {
                            let _ = app.emit("mini-mascot-walk", 0i32);
                            last_walk_dir = 0;
                        }
                        let _ = app.emit("mini-mascot-drag-end", ());
                    }
                } else if over_mascot && left_pressed && !was_pressed {
                    drag_active = true;
                    last_cursor = cursor;
                    // Capture the cursor-to-origin offset at drag start so
                    // the main-thread task can place the window absolutely
                    // each frame instead of summing deltas.
                    if let Some((fx, fy, _, _)) = frame {
                        if let Ok(mut a) = drag_anchor().lock() {
                            *a = Some((cursor.0 - fx, cursor.1 - fy));
                        }
                    }
                    // Cancel any active hover so the sprite immediately
                    // switches from `jumping` to its base/run state when
                    // the drag begins.
                    if was_over_mascot {
                        let _ = app.emit("mini-mascot-hover", false);
                        was_over_mascot = false;
                    }
                }
            } else if drag_active {
                drag_active = false;
                if let Ok(mut a) = drag_anchor().lock() {
                    *a = None;
                }
                if last_walk_dir != 0 {
                    let _ = app.emit("mini-mascot-walk", 0i32);
                    last_walk_dir = 0;
                }
            }
            was_pressed = left_pressed;

            // Hover signal is suppressed while dragging so the sprite
            // shows run-left/run-right instead of jumping.
            let hover_signal = over_mascot && !drag_active;
            if hover_signal != was_over_mascot {
                let _ = app.emit("mini-mascot-hover", hover_signal);
                was_over_mascot = hover_signal;
            }

            // Adaptive polling: fastest while dragging (60fps) so the
            // window keeps up with the cursor; slower when just hovering;
            // very slow when far from the mascot to save battery.
            if drag_active {
                16
            } else if is_expanded || inside || over_mascot {
                30
            } else {
                let screen_top = sy + sh;
                let dist_from_top = screen_top - cursor.1;
                let near_mascot = frame
                    .map(|(fx, fy, fw, fh)| {
                        cursor.0 >= fx - 80.0
                            && cursor.0 <= fx + fw + 80.0
                            && cursor.1 >= fy - 80.0
                            && cursor.1 <= fy + fh + 80.0
                    })
                    .unwrap_or(false);
                if near_mascot || dist_from_top < 200.0 {
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

/// Schedule a main-thread task that snaps the mini window origin to
/// `(cursor_now - DRAG_ANCHOR)` — i.e. wherever the cursor currently is,
/// minus the offset captured at drag-start. Calls coalesce: while a task
/// is in flight, repeated invocations are no-ops; the running task always
/// reads the freshest cursor position. This keeps drag tracking tight
/// even when the poll thread runs much faster than the main thread can
/// repaint, and avoids the cumulative lag of relative-delta translation.
#[cfg(target_os = "macos")]
fn request_drag_apply(app: &tauri::AppHandle) {
    if DRAG_TASK_PENDING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app_clone = app.clone();
    let _ = app.run_on_main_thread(move || {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;
        use objc2_foundation::NSPoint;

        DRAG_TASK_PENDING.store(false, Ordering::SeqCst);
        let anchor = drag_anchor().lock().ok().and_then(|g| *g);
        let Some((ax, ay)) = anchor else { return };

        let cursor = macos_cursor_position();
        let new_origin = NSPoint::new(cursor.0 - ax, cursor.1 - ay);

        if let Some(win) = app_clone.get_webview_window("mini") {
            if let Ok(ns_win) = win.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
                // setFrameOrigin: only moves the window — it does not
                // redraw the contents — so it is far cheaper than
                // setFrame:display:animate:NO and keeps up with fast
                // cursor motion.
                unsafe {
                    let _: () = msg_send![obj, setFrameOrigin: new_origin];
                }
                if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                    if let Some((_, _, w, h)) = *f {
                        *f = Some((new_origin.x, new_origin.y, w, h));
                    }
                }
            }
        }
    });
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

/// Returns the bitmask of currently pressed mouse buttons via
/// `[NSEvent pressedMouseButtons]`. Bit 0 = left button. This works
/// regardless of whether the receiving window is the key window, which is
/// what we need to detect drags on the floating mini mascot.
#[cfg(target_os = "macos")]
fn macos_pressed_mouse_buttons() -> usize {
    unsafe {
        use objc2::msg_send;
        if let Some(cls) = objc2::runtime::AnyClass::get(c"NSEvent") {
            let mask: usize = msg_send![cls, pressedMouseButtons];
            mask
        } else {
            0
        }
    }
}

#[cfg(not(target_os = "macos"))]
fn macos_cursor_position() -> (f64, f64) {
    (0.0, 0.0)
}

// Non-macOS stubs: the efficiency hover / notch drag tracker is a macOS-only
// feature (driven by NSEvent), but the polling loop itself is not gated, so
// we provide no-op implementations on other platforms to keep the build
// happy. The poll loop never engages drag here because `NOTCH_SCREEN_INFO`
// stays unset on Windows/Linux.
#[cfg(not(target_os = "macos"))]
fn macos_pressed_mouse_buttons() -> usize {
    0
}

#[cfg(not(target_os = "macos"))]
fn request_drag_apply(_app: &tauri::AppHandle) {}

/// Resize the expanded mini window height while keeping it top-aligned.
/// macOS: bottom-left origin, so adjust y to keep the same top anchor.
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
                // Top-aligned to the screen, matching the expanded panel's
                // initial placement in `set_mini_expanded`. No MASCOT_TOP_INSET
                // here — that inset only applies to the collapsed mascot.
                let new_y = sf.origin.y + sf.size.height - capped_h;
                let new_frame = NSRect::new(
                    NSPoint::new(cur.origin.x, new_y),
                    NSSize::new(cur.size.width, capped_h),
                );
                log::debug!(
                    "[mini-pos] resize_mini_height(mac) frame x={:.1} y={:.1} w={:.1} h={:.1}",
                    cur.origin.x, new_y, cur.size.width, capped_h
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

/// Detect what media the user is consuming.
///
/// Returns: "music", "video", or "none".
///
/// Priority:
/// 1) System-level now playing playback state (MediaRemote)
/// 2) Frontmost app fallback (video/music bundle IDs)
/// 3) Explicit player-state scripts for background music fallback
#[tauri::command]
async fn get_system_idle_time(app: tauri::AppHandle) -> Result<f64, String> {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel::<f64>();
        app.run_on_main_thread(move || {
            #[link(name = "CoreGraphics", kind = "framework")]
            extern "C" {
                fn CGEventSourceSecondsSinceLastEventType(
                    state_id: i32,
                    event_type: u32,
                ) -> f64;
            }
            let idle = unsafe { CGEventSourceSecondsSinceLastEventType(0, 0xFFFFFFFF) };
            let _ = tx.send(idle);
        }).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(0.0)
    }
}

/// Seconds since the last keyboard event (key down).
/// Used to suppress auto-expand popups when the user is actively typing.
#[tauri::command]
async fn get_keyboard_idle_secs(app: tauri::AppHandle) -> Result<f64, String> {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel::<f64>();
        app.run_on_main_thread(move || {
            #[link(name = "CoreGraphics", kind = "framework")]
            extern "C" {
                fn CGEventSourceSecondsSinceLastEventType(
                    state_id: i32,
                    event_type: u32,
                ) -> f64;
            }
            // kCGEventKeyDown = 10
            let idle = unsafe { CGEventSourceSecondsSinceLastEventType(0, 10) };
            let _ = tx.send(idle);
        }).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(999.0)
    }
}

#[tauri::command]
async fn get_now_playing(app: tauri::AppHandle) -> Result<String, String> {
    #[cfg(target_os = "macos")]
    {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        app.run_on_main_thread(move || {
            let bid = get_frontmost_bundle_id().to_lowercase();
            let cli_status = nowplaying_cli_status();

            let result = if let Some((playing, ref source)) = cli_status {
                if !playing || source.contains("openclaw") || source.contains("ooclaw") || source.contains("com.apple.webkit") {
                    // Not playing, or our own pet SFX hijacked the Now Playing session.
                    // WebView audio (HTML5 Audio / <video>) reports as "com.apple.WebKit.GPU",
                    // not the host app's bundle ID, so we must also filter that.
                    // Fall back to AppleScript to check if a real music app is still playing,
                    // because nowplaying-cli only reports one source at a time.
                    if is_any_music_app_playing() { "music" } else { "none" }
                } else if is_music_app(source) {
                    "music"
                } else if is_video_app(source) || is_browser(source) {
                    "video"
                } else {
                    "music"
                }
            } else {
                // nowplaying-cli not available, fall back to AppleScript
                if is_any_music_app_playing() {
                    "music"
                } else {
                    "none"
                }
            };
            log::info!(
                "[now_playing] frontmost_bid={} cli_status={:?} result={}",
                bid, cli_status, result
            );
            let _ = tx.send(result.into());
        }).map_err(|e| e.to_string())?;
        rx.recv().map_err(|e| e.to_string())
    }
    #[cfg(target_os = "windows")]
    {
        let result = tokio::task::spawn_blocking(|| -> Result<String, String> {
            use windows::Media::Control::{
                GlobalSystemMediaTransportControlsSessionManager,
                GlobalSystemMediaTransportControlsSessionPlaybackStatus,
            };

            let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()
                .map_err(|e| format!("GSMTC RequestAsync failed: {}", e))?
                .get()
                .map_err(|e| format!("GSMTC get manager failed: {}", e))?;

            let sessions = match manager.GetSessions() {
                Ok(s) => s,
                Err(_) => return Ok("none".into()),
            };

            let count = sessions.Size().unwrap_or(0);
            let mut best: Option<&str> = None;
            for i in 0..count {
                let session: windows::Media::Control::GlobalSystemMediaTransportControlsSession =
                    match sessions.GetAt(i) {
                        Ok(s) => s,
                        Err(_) => continue,
                    };

                let source = session.SourceAppUserModelId()
                    .map(|s| s.to_string_lossy().to_lowercase())
                    .unwrap_or_default();

                let info = match session.GetPlaybackInfo() {
                    Ok(i) => i,
                    Err(_) => continue,
                };
                let status = match info.PlaybackStatus() {
                    Ok(s) => s,
                    Err(_) => continue,
                };

                log::info!(
                    "[now_playing/gsmtc] source={} status={:?}",
                    source, status.0
                );

                if status != GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing {
                    continue;
                }

                let kind = if is_video_app_win(&source) || is_browser_win(&source) {
                    "video"
                } else if is_music_app_win(&source) {
                    "music"
                } else {
                    "music"
                };

                if kind == "video" {
                    best = Some("video");
                    break;
                }
                if best.is_none() {
                    best = Some(kind);
                }
            }
            Ok(best.unwrap_or("none").into())
        })
        .await
        .map_err(|e| format!("spawn_blocking join error: {}", e))?;
        result
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Ok("none".into())
    }
}

/// Get the bundle identifier of the frontmost application.
#[cfg(target_os = "macos")]
fn get_frontmost_bundle_id() -> String {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};
    unsafe {
        let cls = match AnyClass::get(c"NSWorkspace") {
            Some(c) => c,
            None => return String::new(),
        };
        let ws: *mut AnyObject = msg_send![cls, sharedWorkspace];
        if ws.is_null() { return String::new(); }
        let front_app: *mut AnyObject = msg_send![&*ws, frontmostApplication];
        if front_app.is_null() { return String::new(); }
        let bid_ns: *mut AnyObject = msg_send![&*front_app, bundleIdentifier];
        if bid_ns.is_null() { return String::new(); }
        let utf8: *const u8 = msg_send![&*bid_ns, UTF8String];
        if utf8.is_null() { return String::new(); }
        let len: usize = msg_send![&*bid_ns, length];
        String::from_utf8_lossy(std::slice::from_raw_parts(utf8, len)).into_owned()
    }
}

#[cfg(target_os = "macos")]
const MUSIC_APP_BIDS: &[&str] = &[
    "com.apple.music", "com.spotify.client", "com.netease.163music",
    "com.tencent.qqmusic", "com.kugou", "com.kuwo",
    "com.xiami.client", "com.apple.itunes",
    "com.soda.music", "com.bytedance.soda.music",
];

#[cfg(target_os = "macos")]
fn is_music_app(bid: &str) -> bool {
    MUSIC_APP_BIDS.iter().any(|m| bid.contains(m))
}

#[cfg(target_os = "macos")]
fn is_music_app_running() -> bool {
    let script = r#"
        set musicBids to {"com.apple.music", "com.spotify.client", "com.netease.163music", "com.tencent.qqmusic", "com.kugou", "com.kuwo", "com.xiami.client", "com.apple.itunes", "com.soda.music", "com.bytedance.soda.music"}
        repeat with bid in musicBids
            try
                if application id (bid as text) is running then return "1"
            end try
        end repeat
        return "0"
    "#;
    match std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
    {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim() == "1",
        Err(_) => false,
    }
}

#[cfg(target_os = "macos")]
fn _get_system_now_playing_is_playing_unused() -> Option<bool> {
    use block2::RcBlock;
    use std::ffi::c_void;
    use std::sync::{Mutex, OnceLock};
    use std::sync::mpsc::channel;
    use std::time::{SystemTime, UNIX_EPOCH};
    use std::time::Duration;

    type DispatchQueue = *mut std::ffi::c_void;
    type PlaybackState = u32;

    const MEDIA_REMOTE_PLAYING: PlaybackState = 1;
    const MEDIA_REMOTE_AMBIGUOUS: PlaybackState = 2;
    const K_CFNUMBER_DOUBLE_TYPE: i32 = 13;
    const K_CFSTRING_ENCODING_UTF8: u32 = 0x0800_0100;

    type MrGetIsPlayingFn = unsafe extern "C" fn(DispatchQueue, &block2::Block<dyn Fn(i8)>);
    type MrGetPlaybackStateFn =
        unsafe extern "C" fn(DispatchQueue, &block2::Block<dyn Fn(PlaybackState)>);
    type MrGetNowPlayingInfoFn =
        unsafe extern "C" fn(DispatchQueue, &block2::Block<dyn Fn(*const c_void)>);
    type DispatchGetGlobalQueueFn = unsafe extern "C" fn(isize, usize) -> DispatchQueue;

    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        fn CFDictionaryGetValue(dict: *const c_void, key: *const c_void) -> *const c_void;
        fn CFNumberGetValue(number: *const c_void, the_type: i32, value: *mut c_void) -> u8;
        fn CFStringCreateWithCString(
            alloc: *const c_void,
            c_str: *const u8,
            encoding: u32,
        ) -> *const c_void;
    }

    static MR_GET_IS_PLAYING_FN: OnceLock<MrGetIsPlayingFn> = OnceLock::new();
    static MR_GET_STATE_FN: OnceLock<MrGetPlaybackStateFn> = OnceLock::new();
    static MR_GET_INFO_FN: OnceLock<MrGetNowPlayingInfoFn> = OnceLock::new();
    static MR_PLAYBACK_RATE_KEY_ADDR: OnceLock<usize> = OnceLock::new();
    static MR_ELAPSED_TIME_KEY_ADDR: OnceLock<usize> = OnceLock::new();
    static DISPATCH_GET_GLOBAL_QUEUE_FN: OnceLock<DispatchGetGlobalQueueFn> = OnceLock::new();
    static LAST_ELAPSED_SAMPLE: OnceLock<Mutex<Option<(f64, f64)>>> = OnceLock::new();

    unsafe {
        let mr_handle = libc::dlopen(
            c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote"
                .as_ptr()
                .cast(),
            libc::RTLD_NOW,
        );
        if mr_handle.is_null() {
            log::info!("[now_playing/media_remote] dlopen MediaRemote failed");
            return None;
        }

        let get_is_playing = if let Some(f) = MR_GET_IS_PLAYING_FN.get() {
            Some(*f)
        } else {
            let mr_is_playing_sym = libc::dlsym(
                mr_handle,
                c"MRMediaRemoteGetNowPlayingApplicationIsPlaying".as_ptr().cast(),
            );
            if mr_is_playing_sym.is_null() {
                None
            } else {
                let f: MrGetIsPlayingFn =
                    std::mem::transmute::<*mut c_void, MrGetIsPlayingFn>(mr_is_playing_sym);
                let _ = MR_GET_IS_PLAYING_FN.set(f);
                Some(f)
            }
        };

        let get_playback_state = if let Some(f) = MR_GET_STATE_FN.get() {
            Some(*f)
        } else {
            let mr_handle = libc::dlopen(
                c"/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote"
                    .as_ptr()
                    .cast(),
                libc::RTLD_NOW,
            );
            if mr_handle.is_null() {
                None
            } else {
                let mr_sym = libc::dlsym(
                    mr_handle,
                    c"MRMediaRemoteGetNowPlayingApplicationPlaybackState"
                        .as_ptr()
                        .cast(),
                );
                if mr_sym.is_null() {
                    None
                } else {
                    let f: MrGetPlaybackStateFn = std::mem::transmute::<*mut c_void, MrGetPlaybackStateFn>(mr_sym);
                    let _ = MR_GET_STATE_FN.set(f);
                    Some(f)
                }
            }
        };

        let get_now_playing_info = if let Some(f) = MR_GET_INFO_FN.get() {
            Some(*f)
        } else {
            let mr_info_sym = libc::dlsym(
                mr_handle,
                c"MRMediaRemoteGetNowPlayingInfo".as_ptr().cast(),
            );
            if mr_info_sym.is_null() {
                None
            } else {
                let f: MrGetNowPlayingInfoFn =
                    std::mem::transmute::<*mut c_void, MrGetNowPlayingInfoFn>(mr_info_sym);
                let _ = MR_GET_INFO_FN.set(f);
                Some(f)
            }
        };

        let playback_rate_key = if let Some(addr) = MR_PLAYBACK_RATE_KEY_ADDR.get() {
            Some(*addr as *const c_void)
        } else {
            let key_sym = libc::dlsym(
                mr_handle,
                c"kMRMediaRemoteNowPlayingInfoPlaybackRate".as_ptr().cast(),
            );
            let key = if key_sym.is_null() {
                let fallback = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"kMRMediaRemoteNowPlayingInfoPlaybackRate".as_ptr().cast(),
                    K_CFSTRING_ENCODING_UTF8,
                );
                if fallback.is_null() {
                    std::ptr::null()
                } else {
                    fallback
                }
            } else {
                // Exported as CFStringRef* global; dereference once to get key object.
                *(key_sym as *const *const c_void)
            };
            if key.is_null() {
                None
            } else {
                let _ = MR_PLAYBACK_RATE_KEY_ADDR.set(key as usize);
                Some(key)
            }
        };

        let elapsed_time_key = if let Some(addr) = MR_ELAPSED_TIME_KEY_ADDR.get() {
            Some(*addr as *const c_void)
        } else {
            let key_sym = libc::dlsym(
                mr_handle,
                c"kMRMediaRemoteNowPlayingInfoElapsedTime".as_ptr().cast(),
            );
            let key = if key_sym.is_null() {
                let fallback = CFStringCreateWithCString(
                    std::ptr::null(),
                    c"kMRMediaRemoteNowPlayingInfoElapsedTime".as_ptr().cast(),
                    K_CFSTRING_ENCODING_UTF8,
                );
                if fallback.is_null() {
                    std::ptr::null()
                } else {
                    fallback
                }
            } else {
                *(key_sym as *const *const c_void)
            };
            if key.is_null() {
                None
            } else {
                let _ = MR_ELAPSED_TIME_KEY_ADDR.set(key as usize);
                Some(key)
            }
        };

        let get_global_queue = if let Some(f) = DISPATCH_GET_GLOBAL_QUEUE_FN.get() {
            *f
        } else {
            let dispatch_handle =
                libc::dlopen(c"/usr/lib/system/libdispatch.dylib".as_ptr().cast(), libc::RTLD_NOW);
            if dispatch_handle.is_null() {
                log::info!("[now_playing/media_remote] dlopen libdispatch failed");
                return None;
            }
            let dispatch_sym =
                libc::dlsym(dispatch_handle, c"dispatch_get_global_queue".as_ptr().cast());
            if dispatch_sym.is_null() {
                log::info!("[now_playing/media_remote] dlsym dispatch_get_global_queue failed");
                return None;
            }
            let f: DispatchGetGlobalQueueFn =
                std::mem::transmute::<*mut c_void, DispatchGetGlobalQueueFn>(dispatch_sym);
            let _ = DISPATCH_GET_GLOBAL_QUEUE_FN.set(f);
            f
        };

        let queue = get_global_queue(0, 0);

        // Best signal: now playing info playbackRate (0 paused, 1 playing).
        if let Some(get_now_playing_info_fn) = get_now_playing_info
        {
            let (tx, rx) = channel::<(Option<f64>, Option<f64>)>();
            let callback = RcBlock::new(move |info: *const c_void| {
                if info.is_null() {
                    let _ = tx.send((None, None));
                    return;
                }
                let read_number = |key: Option<*const c_void>| -> Option<f64> {
                    let k = key?;
                    let value = CFDictionaryGetValue(info, k);
                    if value.is_null() {
                        return None;
                    }
                    let mut n: f64 = 0.0;
                    let ok = CFNumberGetValue(
                        value,
                        K_CFNUMBER_DOUBLE_TYPE,
                        &mut n as *mut f64 as *mut c_void,
                    );
                    if ok != 0 { Some(n) } else { None }
                };
                let rate = read_number(playback_rate_key);
                let elapsed = read_number(elapsed_time_key);
                let _ = tx.send((rate, elapsed));
            });
            get_now_playing_info_fn(queue, &callback);
            match rx.recv_timeout(Duration::from_millis(220)) {
                Ok((Some(rate), _)) => {
                    let is_playing = rate > 0.01;
                    log::info!(
                        "[now_playing/media_remote] playback_rate={} source=now_playing_info is_playing={}",
                        rate, is_playing
                    );
                    return Some(is_playing);
                }
                Ok((None, Some(elapsed))) => {
                    let now_sec = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs_f64())
                        .unwrap_or(0.0);
                    let cache = LAST_ELAPSED_SAMPLE.get_or_init(|| Mutex::new(None));
                    let mut guard = cache.lock().unwrap();
                    let inferred = if let Some((prev_elapsed, prev_ts)) = *guard {
                        let dt = (now_sec - prev_ts).max(0.001);
                        let de = elapsed - prev_elapsed;
                        // Progress increasing at a meaningful pace => playing.
                        // Paused typically keeps elapsed almost unchanged.
                        Some(de > dt * 0.15)
                    } else {
                        None
                    };
                    *guard = Some((elapsed, now_sec));
                    log::info!(
                        "[now_playing/media_remote] elapsed_time={} source=elapsed_fallback inferred={:?}",
                        elapsed, inferred
                    );
                    if let Some(v) = inferred {
                        return Some(v);
                    }
                }
                Ok((None, None)) => {
                    log::info!(
                        "[now_playing/media_remote] playback_rate/elapsed missing source=now_playing_info fallback=is_playing/state"
                    );
                }
                Err(_) => {
                    log::info!(
                        "[now_playing/media_remote] now_playing_info timeout fallback=is_playing/state"
                    );
                }
            }
        }

        let mut is_playing_api_result: Option<bool> = None;
        if let Some(get_is_playing_fn) = get_is_playing {
            let (tx, rx) = channel::<i8>();
            let callback = RcBlock::new(move |is_playing: i8| {
                let _ = tx.send(is_playing);
            });
            get_is_playing_fn(queue, &callback);
            match rx.recv_timeout(Duration::from_millis(220)) {
                Ok(is_playing_raw) => {
                    let is_playing = is_playing_raw != 0;
                    log::info!("[now_playing/media_remote] is_playing_api={} source=is_playing", is_playing);
                    is_playing_api_result = Some(is_playing);
                }
                Err(_) => {
                    log::info!("[now_playing/media_remote] is_playing_api timeout, fallback=playback_state");
                }
            }
        }

        if let Some(get_playback_state_fn) = get_playback_state {
            let (tx, rx) = channel::<PlaybackState>();
            let callback = RcBlock::new(move |state: PlaybackState| {
                let _ = tx.send(state);
            });
            get_playback_state_fn(queue, &callback);
            let playback_state_result = match rx.recv_timeout(Duration::from_millis(220)) {
                Ok(state) => {
                    log::info!(
                        "[now_playing/media_remote] playback_state={} source=state_fallback",
                        state
                    );
                    Some(state)
                }
                Err(_) => {
                    log::info!("[now_playing/media_remote] playback_state timeout");
                    None
                }
            };
            let audio_active = is_audio_output_active();
            return match (is_playing_api_result, playback_state_result) {
                // Prefer explicit API when it reliably reports playing.
                (Some(true), _) => Some(true),
                // Some integrations always return false from is_playing API.
                // In that case, accept ambiguous state=2 only when audio output is active.
                (Some(false), Some(state)) if state == MEDIA_REMOTE_AMBIGUOUS => {
                    let inferred = false;
                    log::info!(
                        "[now_playing/media_remote] reconcile is_playing=false state=2 audio_active={} inferred={}",
                        audio_active, inferred
                    );
                    Some(inferred)
                }
                (Some(false), Some(state)) => {
                    let inferred = state == MEDIA_REMOTE_PLAYING;
                    log::info!(
                        "[now_playing/media_remote] reconcile is_playing=false state={} inferred={}",
                        state, inferred
                    );
                    Some(inferred)
                }
                // If explicit API timed out/unavailable, use state + audio tie-breaker.
                (None, Some(state)) if state == MEDIA_REMOTE_AMBIGUOUS => {
                    let inferred = audio_active;
                    log::info!(
                        "[now_playing/media_remote] reconcile no_is_playing state=2 audio_active={} inferred={}",
                        audio_active, inferred
                    );
                    Some(inferred)
                }
                (None, Some(state)) => Some(state == MEDIA_REMOTE_PLAYING),
                (Some(v), None) => Some(v),
                (None, None) => None,
            };
        }

        if is_playing_api_result.is_some() {
            return is_playing_api_result;
        }
        log::info!("[now_playing/media_remote] no usable media_remote symbol");
        None
    }
}

/// Check if the default audio output device has any audio running.
/// Used only as a tie-breaker for ambiguous MediaRemote states.
#[cfg(target_os = "macos")]
fn is_audio_output_active() -> bool {
    #[allow(non_upper_case_globals)]
    const kAudioHardwarePropertyDefaultOutputDevice: u32 = u32::from_be_bytes(*b"dOut");
    #[allow(non_upper_case_globals)]
    const kAudioDevicePropertyDeviceIsRunningSomewhere: u32 = u32::from_be_bytes(*b"gone");
    #[allow(non_upper_case_globals)]
    const kAudioObjectPropertyScopeGlobal: u32 = u32::from_be_bytes(*b"glob");
    #[allow(non_upper_case_globals)]
    const kAudioObjectPropertyElementMain: u32 = 0;
    #[allow(non_upper_case_globals)]
    const kAudioObjectSystemObject: u32 = 1;

    #[repr(C)]
    struct AudioObjectPropertyAddress {
        selector: u32,
        scope: u32,
        element: u32,
    }

    #[link(name = "CoreAudio", kind = "framework")]
    unsafe extern "C" {
        fn AudioObjectGetPropertyData(
            id: u32,
            addr: *const AudioObjectPropertyAddress,
            qualifier_size: u32,
            qualifier: *const std::ffi::c_void,
            data_size: *mut u32,
            data: *mut std::ffi::c_void,
        ) -> i32;
    }

    unsafe {
        let addr = AudioObjectPropertyAddress {
            selector: kAudioHardwarePropertyDefaultOutputDevice,
            scope: kAudioObjectPropertyScopeGlobal,
            element: kAudioObjectPropertyElementMain,
        };
        let mut device: u32 = 0;
        let mut size = std::mem::size_of::<u32>() as u32;
        let err = AudioObjectGetPropertyData(
            kAudioObjectSystemObject,
            &addr,
            0,
            std::ptr::null(),
            &mut size,
            &mut device as *mut u32 as *mut std::ffi::c_void,
        );
        if err != 0 || device == 0 {
            return false;
        }

        let addr2 = AudioObjectPropertyAddress {
            selector: kAudioDevicePropertyDeviceIsRunningSomewhere,
            scope: kAudioObjectPropertyScopeGlobal,
            element: kAudioObjectPropertyElementMain,
        };
        let mut running: u32 = 0;
        size = std::mem::size_of::<u32>() as u32;
        let err2 = AudioObjectGetPropertyData(
            device,
            &addr2,
            0,
            std::ptr::null(),
            &mut size,
            &mut running as *mut u32 as *mut std::ffi::c_void,
        );
        err2 == 0 && running != 0
    }
}

/// Use `nowplaying-cli` to check playback rate and source app.
/// Returns (is_playing, source_bundle_id) or None if tool unavailable.
#[cfg(target_os = "macos")]
fn nowplaying_cli_status() -> Option<(bool, String)> {
    static CLI_PATH: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    let path = CLI_PATH.get_or_init(|| {
        for p in &["/opt/homebrew/bin/nowplaying-cli", "/usr/local/bin/nowplaying-cli"] {
            if std::path::Path::new(p).exists() {
                return Some(p.to_string());
            }
        }
        None
    });
    let cli = path.as_deref()?;
    let output = std::process::Command::new(cli)
        .args(["get", "playbackRate", "clientBundleIdentifier"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut lines = text.lines();
    let rate: f64 = lines.next()?.trim().parse().ok()?;
    let source_bid = lines.next().unwrap_or("").trim().to_lowercase();
    Some((rate > 0.01, source_bid))
}

#[cfg(target_os = "macos")]
fn is_any_music_app_playing() -> bool {
    let script = r#"
        set isPlaying to false

        -- Check apps that support "player state" AppleScript
        if application "Music" is running then
            tell application "Music"
                try
                    if player state is playing then set isPlaying to true
                end try
            end tell
        end if

        if (not isPlaying) and application "Spotify" is running then
            tell application "Spotify"
                try
                    if player state is playing then set isPlaying to true
                end try
            end tell
        end if

        -- For apps without AppleScript player-state (NeteaseMusic, QQ Music, etc.),
        -- check the system menu bar: the first item in the "控制" menu
        -- toggles between "播放"/"暂停" or "Play"/"Pause".
        if not isPlaying then
            tell application "System Events"
                set menuChecks to {{"com.netease.163music", "控制"}, {"com.tencent.qqmusic", "控制"}, {"com.soda.music", "控制"}, {"com.bytedance.soda.music", "控制"}}
                repeat with entry in menuChecks
                    if isPlaying then exit repeat
                    set bid to item 1 of entry
                    set menuName to item 2 of entry
                    try
                        set procs to every process whose bundle identifier is bid
                        if (count of procs) > 0 then
                            set p to item 1 of procs
                            set firstItem to name of menu item 1 of menu 1 of menu bar item menuName of menu bar 1 of p
                            if firstItem is "暂停" or firstItem is "Pause" then
                                set isPlaying to true
                            end if
                        end if
                    end try
                end repeat
            end tell
        end if

        if isPlaying then
            return "1"
        else
            return "0"
        end if
    "#;

    match std::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
    {
        Ok(output) => {
            let result = String::from_utf8_lossy(&output.stdout).trim() == "1";
            log::info!("[now_playing/script] is_any_music_app_playing={}", result);
            result
        }
        Err(_) => false,
    }
}

#[cfg(target_os = "macos")]
fn is_video_app(bid: &str) -> bool {
    const VIDEO_APPS: &[&str] = &[
        "com.colliderli.iina", "org.videolan.vlc", "com.apple.quicktimeplayer",
        "tv.plex.plexmediaplayer", "io.mpv", "com.apple.tv",
        "com.bilibili.bili", "com.disneyplus", "com.netflix",
    ];
    VIDEO_APPS.iter().any(|v| bid.contains(v))
}

#[cfg(target_os = "macos")]
fn is_browser(bid: &str) -> bool {
    const BROWSERS: &[&str] = &[
        "com.google.chrome", "org.mozilla.firefox", "com.apple.safari",
        "com.microsoft.edgemac", "com.brave.browser", "com.vivaldi.vivaldi",
        "company.thebrowser.browser", "com.operasoftware.opera",
    ];
    BROWSERS.iter().any(|b| bid.contains(b))
}

#[cfg(target_os = "windows")]
fn is_music_app_win(id: &str) -> bool {
    const MUSIC_APPS: &[&str] = &[
        "spotify", "zune", "zunemusic",
        "cloudmusic", "163music", "netease", "\u{7f51}\u{6613}\u{4e91}",
        "qqmusic", "qq\u{97f3}\u{4e50}",
        "kugou", "\u{9177}\u{72d7}", "kuwo", "\u{9177}\u{6211}",
        "foobar2000", "aimp", "musicbee",
        "itunes", "applemusic", "cider",
        "\u{6c7d}\u{6c34}\u{97f3}\u{4e50}", "soda",
    ];
    MUSIC_APPS.iter().any(|m| id.contains(m))
}

#[cfg(target_os = "windows")]
fn is_video_app_win(id: &str) -> bool {
    const VIDEO_APPS: &[&str] = &[
        "potplayer", "vlc", "mpv",
        "plex", "mpc-hc", "mpc-be",
        "kmplayer", "iina", "films",
        "bilibili", "\u{54d4}\u{54e9}\u{54d4}\u{54e9}",
        "disney", "netflix", "hbo",
        "douyin", "\u{6296}\u{97f3}", "tiktok",
        "iqiyi", "\u{7231}\u{5947}\u{827a}",
        "youku", "\u{4f18}\u{9177}",
        "mgtv", "\u{8292}\u{679c}",
        "dandanplay",
    ];
    VIDEO_APPS.iter().any(|v| id.contains(v))
}

#[cfg(target_os = "windows")]
fn is_browser_win(id: &str) -> bool {
    const BROWSERS: &[&str] = &[
        "chrome", "firefox", "msedge",
        "brave", "vivaldi", "opera",
        "arc",
    ];
    BROWSERS.iter().any(|b| id.contains(b))
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
        #[cfg(target_os = "windows")]
        {
            if let Ok(Some(monitor)) = win.current_monitor() {
                let scale = monitor.scale_factor();
                if let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) {
                    let current_x = pos.x as f64 / scale;
                    let current_y = pos.y as f64 / scale;
                    let current_w = size.width as f64 / scale;
                    let current_h = size.height as f64 / scale;
                    let sw = monitor.size().width as f64 / scale;
                    let sh = monitor.size().height as f64 / scale;
                    let left_pad = 180.0;
                    let top_pad = 100.0;
                    let win_w = (current_w + left_pad).min(sw);
                    let win_h = (current_h + top_pad).min(sh);
                    // Keep bottom-right corner fixed so mascot stays anchored.
                    let x = (current_x + current_w - win_w).max(0.0).min(sw - win_w);
                    let y = current_y.max(0.0).min(sh - win_h);
                    let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                    let _ = win.set_position(tauri::LogicalPosition::new(x, y));
                }
            }
            if !FULLSCREEN_HIDING.load(std::sync::atomic::Ordering::SeqCst) {
                let _ = win.set_always_on_top(true);
                let _ = win.show();
            }
        }

        // Start the click-through poll thread.
        PET_PASSTHROUGH_ACTIVE.store(true, Ordering::SeqCst);
        #[cfg(target_os = "macos")]
        if !PET_PASSTHROUGH_THREAD_ALIVE.load(Ordering::SeqCst) {
            let app2 = app.clone();
            std::thread::spawn(move || pet_passthrough_poll(app2, mascot_scale, large_mascot_scale));
        }
        #[cfg(target_os = "windows")]
        if !PET_PASSTHROUGH_THREAD_ALIVE.load(Ordering::SeqCst) {
            let app2 = app.clone();
            std::thread::spawn(move || pet_passthrough_poll_windows(app2, mascot_scale, large_mascot_scale));
        }
    } else {
        // Stop the poll thread.
        PET_PASSTHROUGH_ACTIVE.store(false, Ordering::SeqCst);
        PET_CONTEXT_MENU_OPEN.store(false, Ordering::SeqCst);
        PET_POMODORO_ACTIVE.store(false, Ordering::SeqCst);

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
        #[cfg(target_os = "windows")]
        {
            if let Ok(Some(monitor)) = win.current_monitor() {
                let scale = monitor.scale_factor();
                if let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) {
                    let current_x = pos.x as f64 / scale;
                    let current_y = pos.y as f64 / scale;
                    let current_w = size.width as f64 / scale;
                    let (win_w, win_h) = large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale);
                    // Collapse towards bottom-right corner.
                    let x = current_x + current_w - win_w;
                    let y = current_y;
                    let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                    let _ = win.set_position(tauri::LogicalPosition::new(x, y));
                }
            }
        }
    }
    Ok(())
}

/// Tell the pet-mode pass-through poll whether a pomodoro timer is active.
/// When true, the entire mascot window stays interactive so the bottom-
/// anchored Pomodoro stop button receives clicks instead of having them
/// pass through (it sits in the centered hitbox's bottom inset region).
#[tauri::command]
async fn set_pet_pomodoro_active(active: bool) -> Result<(), String> {
    PET_POMODORO_ACTIVE.store(active, Ordering::SeqCst);
    Ok(())
}

/// Tell the pet-mode pass-through poll whether the context menu is open.
/// When `side` is `"right"` the window is widened rightward by 180 px
/// (left edge stays put).  The frontend sets the mascot CSS to
/// `right: 180` so it does not move on screen — it stays at exactly
/// the same pixel position.  Menu buttons render in the new 180 px area
/// via `overflow: visible` + `left: mascotSize + 14`.
#[tauri::command]
async fn set_pet_context_menu(app: tauri::AppHandle, open: bool, side: Option<String>) -> Result<(), String> {
    PET_CONTEXT_MENU_OPEN.store(open, Ordering::SeqCst);

    #[cfg(target_os = "macos")]
    {
        let right_pad = 180.0_f64;
        if open && side.as_deref() == Some("right") {
            if let Some(win) = app.get_webview_window("mini") {
                let win_clone = win.clone();
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                let _ = app.run_on_main_thread(move || {
                    use objc2::runtime::AnyObject;
                    use objc2::msg_send;
                    use objc2_foundation::{NSRect, NSPoint, NSSize};
                    if let Ok(ns_win) = win_clone.ns_window() {
                        let obj = unsafe { &*(ns_win as *mut AnyObject) };
                        let current: NSRect = unsafe { msg_send![obj, frame] };
                        if let Ok(mut saved) = PET_MENU_RESTORE_FRAME.lock() {
                            *saved = Some((
                                current.origin.x,
                                current.origin.y,
                                current.size.width,
                                current.size.height,
                            ));
                        }
                        // Widen rightward — left edge stays fixed, mascot
                        // keeps its screen position via CSS right: 180.
                        let new_w = current.size.width + right_pad;
                        let frame = NSRect::new(
                            NSPoint::new(current.origin.x, current.origin.y),
                            NSSize::new(new_w, current.size.height),
                        );
                        unsafe {
                            let _: () = msg_send![obj, setAlphaValue: 0.0f64];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                        if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                            *f = Some((current.origin.x, current.origin.y, new_w, current.size.height));
                        }
                        pet_context_schedule_restore_alpha(ns_win as *mut std::ffi::c_void);
                    }
                    let _ = tx.send(());
                });
                let _ = rx.recv();
            }
        } else if !open {
            if let Some(win) = app.get_webview_window("mini") {
                let win_clone = win.clone();
                let (tx, rx) = std::sync::mpsc::channel::<()>();
                let _ = app.run_on_main_thread(move || {
                    use objc2::runtime::AnyObject;
                    use objc2::msg_send;
                    use objc2_foundation::{NSRect, NSPoint, NSSize};
                    if let Ok(ns_win) = win_clone.ns_window() {
                        let obj = unsafe { &*(ns_win as *mut AnyObject) };
                        if let Ok(mut saved) = PET_MENU_RESTORE_FRAME.lock() {
                            if let Some((_x, _y, w, h)) = *saved {
                                let current: NSRect = unsafe { msg_send![obj, frame] };
                                let frame = NSRect::new(
                                    // Keep current position (user may have dragged while menu open),
                                    // only restore size.
                                    NSPoint::new(current.origin.x, current.origin.y),
                                    NSSize::new(w, h),
                                );
                                unsafe {
                                    let _: () = msg_send![obj, setAlphaValue: 0.0f64];
                                    let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                                }
                                if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                                    *f = Some((current.origin.x, current.origin.y, w, h));
                                }
                                *saved = None;
                                pet_context_schedule_restore_alpha(ns_win as *mut std::ffi::c_void);
                            }
                        }
                    }
                    let _ = tx.send(());
                });
                let _ = rx.recv();
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        let right_pad = 180.0_f64;
        if open && side.as_deref() == Some("right") {
            if let Some(win) = app.get_webview_window("mini") {
                if let Ok(Some(monitor)) = win.current_monitor() {
                    let scale = monitor.scale_factor();
                    if let (Ok(pos), Ok(size)) = (win.outer_position(), win.outer_size()) {
                        let current_x = pos.x as f64 / scale;
                        let current_y = pos.y as f64 / scale;
                        let current_w = size.width as f64 / scale;
                        let current_h = size.height as f64 / scale;
                        if let Ok(mut saved) = PET_MENU_RESTORE_FRAME.lock() {
                            if saved.is_none() {
                                *saved = Some((current_x, current_y, current_w, current_h));
                            }
                        }
                        // Widen rightward — left edge stays fixed, mascot keeps
                        // screen position via CSS right: 180.
                        let new_w = current_w + right_pad;
                        let _ = win.set_size(tauri::LogicalSize::new(new_w, current_h));
                        if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                            *f = Some((current_x, current_y, new_w, current_h));
                        }
                    }
                }
            }
        } else if !open {
            if let Some(win) = app.get_webview_window("mini") {
                if let Ok(mut saved) = PET_MENU_RESTORE_FRAME.lock() {
                    if let Some((_x, _y, w, h)) = *saved {
                        let (current_x, current_y) = match (win.outer_position(), win.current_monitor()) {
                            (Ok(pos), Ok(Some(monitor))) => {
                                let scale = monitor.scale_factor();
                                (pos.x as f64 / scale, pos.y as f64 / scale)
                            }
                            _ => (0.0, 0.0),
                        };
                        let _ = win.set_size(tauri::LogicalSize::new(w, h));
                        if let Ok(mut f) = MINI_WINDOW_FRAME.lock() {
                            *f = Some((current_x, current_y, w, h));
                        }
                        *saved = None;
                    }
                }
            }
        }
    }

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
    let (mascot_w, mascot_h) = large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale);
    // Keep these ratios aligned with frontend `Mini.tsx` so hover cursor and
    // native pass-through behavior remain consistent around mascot edges.
    let hit_w = mascot_w * (2.4 / 3.0);
    let hit_h = mascot_h * (2.8 / 3.0);
    let inset_x = (mascot_w - hit_w) / 2.0;
    let inset_y = (mascot_h - hit_h) / 2.0;
    let edge_threshold = 30.0;
    // Get screen bounds once at startup so we can detect edge proximity.
    let screen_bounds: Option<(f64, f64, f64, f64)> = {
        let (tx, rx) = std::sync::mpsc::channel();
        let app_c = app.clone();
        let _ = app_c.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::NSRect;
            let result: Option<(f64, f64, f64, f64)> = unsafe {
                AnyClass::get(c"NSScreen").and_then(|cls| {
                    let ms: *mut AnyObject = msg_send![cls, mainScreen];
                    if ms.is_null() { None } else {
                        let sf: NSRect = msg_send![&*ms, frame];
                        Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height))
                    }
                })
            };
            let _ = tx.send(result);
        });
        rx.recv().ok().flatten()
    };

    while PET_PASSTHROUGH_ACTIVE.load(Ordering::SeqCst) {
        let menu_open = PET_CONTEXT_MENU_OPEN.load(Ordering::SeqCst);
        let pomodoro_active = PET_POMODORO_ACTIVE.load(Ordering::SeqCst);
        let frame = MINI_WINDOW_FRAME.lock().ok().and_then(|g| *g);

        let should_be_interactive = if menu_open || pomodoro_active {
            true
        } else if let Some((fx, fy, fw, _fh)) = frame {
            let cursor = macos_cursor_position();
            let mascot_left = fx + fw - mascot_w;
            let mascot_right = mascot_left + mascot_w;
            let mascot_bottom = fy;
            // Drop hitbox insets when the mascot extends near/past a screen
            // edge so the visible portion stays fully clickable.
            let near_edge = if let Some((sx, _sy, sw, _sh)) = screen_bounds {
                mascot_left < sx + edge_threshold || mascot_right > sx + sw - edge_threshold
            } else {
                mascot_left < edge_threshold
            };
            // Near screen edge, keep hitbox reasonably generous but never full-rect.
            // Full-rect near-edge hitboxes make peek feel "too clickable" and steal
            // hover/clicks away from nearby desktop content.
            let ix = if near_edge { inset_x * 0.5 } else { inset_x };
            let iy = inset_y;
            let hit_left = mascot_left + ix;
            let hit_right = mascot_right - ix;
            let hit_bottom = mascot_bottom + iy;
            let hit_top = mascot_bottom + mascot_h - iy;
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

/// Windows equivalent of `pet_passthrough_poll`. Polls the global cursor
/// position (via Win32 `GetCursorPos`) every 20 ms and toggles the mini
/// webview's `set_ignore_cursor_events` so clicks outside the mascot
/// hit-box pass through to whatever is behind, while clicks on the mascot
/// itself reach the webview. When the pet context menu is open the entire
/// window is interactive so menu buttons receive clicks.
#[cfg(target_os = "windows")]
fn pet_passthrough_poll_windows(app: tauri::AppHandle, mascot_scale: f64, large_mascot_scale: f64) {
    use std::time::Duration;
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;

    PET_PASSTHROUGH_THREAD_ALIVE.store(true, Ordering::SeqCst);
    // mascot dimensions in logical pixels (matches CSS px on Windows WebView2).
    let (mascot_w_logical, mascot_h_logical) = large_collapsed_mascot_window_size(mascot_scale, large_mascot_scale);
    let hit_w = mascot_w_logical * (1.8 / 3.0);
    let hit_h = mascot_h_logical * (2.5 / 3.0);
    let inset_x_logical = (mascot_w_logical - hit_w) / 2.0;
    let inset_y_logical = (mascot_h_logical - hit_h) / 2.0;
    let edge_threshold_logical = 30.0_f64;

    let mut last_state: Option<bool> = None;

    while PET_PASSTHROUGH_ACTIVE.load(Ordering::SeqCst) {
        let menu_open = PET_CONTEXT_MENU_OPEN.load(Ordering::SeqCst);
        let pomodoro_active = PET_POMODORO_ACTIVE.load(Ordering::SeqCst);

        let should_be_interactive = if menu_open || pomodoro_active {
            true
        } else {
            // Read cursor position and window geometry in physical pixels.
            let cursor = unsafe {
                let mut pt = POINT::default();
                if GetCursorPos(&mut pt).is_ok() {
                    Some((pt.x as f64, pt.y as f64))
                } else {
                    None
                }
            };
            let win = app.get_webview_window("mini");
            match (win, cursor) {
                (Some(win), Some((cx, cy))) => {
                    let pos = win.outer_position().ok();
                    let size = win.outer_size().ok();
                    let scale = win.scale_factor().unwrap_or(1.0);
                    let monitor = win.current_monitor().ok().flatten();
                    if let (Some(pos), Some(size)) = (pos, size) {
                        let fx = pos.x as f64;
                        let fy = pos.y as f64;
                        let fw = size.width as f64;
                        let fh = size.height as f64;

                        // Mascot is anchored at `left: petBaseWinW - mascotW` and `bottom: 0`,
                        // i.e. the right-bottom corner of the no-menu window. When the menu
                        // is closed, fw == petBaseWinW so the mascot's right edge in screen
                        // physical px is fx + fw and its bottom is fy + fh.
                        let mascot_w = mascot_w_logical * scale;
                        let mascot_h = mascot_h_logical * scale;
                        let inset_x = inset_x_logical * scale;
                        let inset_y = inset_y_logical * scale;
                        let edge_threshold = edge_threshold_logical * scale;

                        let mascot_right = fx + fw;
                        let mascot_left = mascot_right - mascot_w;
                        let mascot_bottom = fy + fh;
                        let mascot_top = mascot_bottom - mascot_h;

                        let near_edge = if let Some(monitor) = monitor {
                            let mp = monitor.position();
                            let ms = monitor.size();
                            let monitor_left = mp.x as f64;
                            let monitor_right = monitor_left + ms.width as f64;
                            mascot_left < monitor_left + edge_threshold
                                || mascot_right > monitor_right - edge_threshold
                        } else {
                            false
                        };

                        // Keep edge hitbox slightly relaxed on X only; do not use
                        // full-rect hitboxes, which feel too large during peek.
                        let ix = if near_edge { inset_x * 0.5 } else { inset_x };
                        let iy = inset_y;
                        let hit_left = mascot_left + ix;
                        let hit_right = mascot_right - ix;
                        let hit_top = mascot_top + iy;
                        let hit_bottom = mascot_bottom - iy;

                        cx >= hit_left && cx <= hit_right
                            && cy >= hit_top && cy <= hit_bottom
                    } else {
                        false
                    }
                }
                _ => false,
            }
        };

        if last_state != Some(should_be_interactive) {
            if let Some(win) = app.get_webview_window("mini") {
                let _ = win.set_ignore_cursor_events(!should_be_interactive);
            }
            last_state = Some(should_be_interactive);
        }

        std::thread::sleep(Duration::from_millis(20));
    }

    // Re-enable click events on exit so the window stays usable when leaving pet mode.
    if let Some(win) = app.get_webview_window("mini") {
        let _ = win.set_ignore_cursor_events(false);
    }
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
                            (
                                collapsed_x(sx, sw, win_w, &pos, notch_off),
                                sy + sh - win_h - MASCOT_TOP_INSET,
                            )
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
                    let _ = win.set_position(tauri::LogicalPosition::new(
                        x,
                        my + (MASCOT_TOP_INSET * ui).round(),
                    ));
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
    /// Source of this session: "cc" (Claude Code), "codex" (Codex), "cursor" (Cursor IDE), or "gemini" (Gemini CLI).
    pub source: String,
    /// Ghostty terminal `id` captured when the session is first seen.
    /// Used by `jump_to_claude_terminal` to select the exact tab instead
    /// of relying on CWD/title matching which is ambiguous.
    #[serde(skip)]
    pub terminal_id: Option<String>,
    /// Host terminal app name (e.g. "Ghostty", "Cursor", "iTerm2", "Claude Desktop").
    /// Captured once at session creation via process chain walk or from the
    /// hook script's `host` tag. Exposed to the frontend so it can distinguish
    /// CC CLI sessions from CC Desktop sessions and gate them independently.
    #[serde(rename = "hostTerminal", skip_serializing_if = "Option::is_none")]
    pub host_terminal: Option<String>,
    /// Hermes platform identifier (e.g. "feishu", "slack", "discord").
    /// Used to decide which application to activate on session click.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
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
    /// Session IDs dismissed by the user (trash icon). Prevents DB-loaded
    /// Hermes sessions from reappearing after removal.
    dismissed: Arc<Mutex<std::collections::HashSet<String>>>,
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

/// Build an empty 14-day `ClaudeStats` skeleton with zeroed daily entries.
/// Used for sources where token usage is intentionally not surfaced — at the
/// moment that means `cursor`, since Cursor doesn't expose reliable per-turn
/// token counts to oc-claw (it stopped writing them to its local SQLite in
/// April 2026, and the hook payload would only cover sessions completed after
/// the user enables tracking, which is misleading).
fn empty_claude_stats() -> ClaudeStats {
    let now = chrono::Local::now();
    let mut daily_stats: Vec<ClaudeDailyStats> = Vec::with_capacity(14);
    for i in (0..14).rev() {
        let day = (now - chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
        daily_stats.push(ClaudeDailyStats {
            date: day,
            input_tokens: 0, output_tokens: 0,
            cache_read_tokens: 0, cache_write_tokens: 0,
            messages: 0, sessions: 0,
        });
    }
    ClaudeStats {
        total_input_tokens: 0, total_output_tokens: 0,
        total_cache_read_tokens: 0, total_cache_write_tokens: 0,
        total_messages: 0, total_sessions: 0,
        daily_stats,
        model: "unsupported".to_string(),
    }
}

fn collect_hermes_stats() -> Result<ClaudeStats, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let db_path = home.join(".hermes").join("state.db");
    if !db_path.exists() {
        return Ok(empty_claude_stats());
    }

    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|e| format!("open hermes state.db: {}", e))?;

    let now = chrono::Utc::now();
    let cutoff = now - chrono::Duration::days(14);
    let cutoff_ts = cutoff.timestamp() as f64;

    let mut daily_map: std::collections::BTreeMap<String, ClaudeDailyStats> = std::collections::BTreeMap::new();
    for i in (0..14).rev() {
        let day = (chrono::Local::now() - chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
        daily_map.insert(day.clone(), ClaudeDailyStats {
            date: day, input_tokens: 0, output_tokens: 0,
            cache_read_tokens: 0, cache_write_tokens: 0, messages: 0, sessions: 0,
        });
    }

    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_cache_write = 0u64;
    let mut total_messages = 0u64;
    let mut total_sessions = 0u64;
    let mut model = String::new();

    let mut stmt = conn.prepare(
        "SELECT started_at, input_tokens, output_tokens, cache_read_tokens, \
         cache_write_tokens, message_count, model \
         FROM sessions WHERE started_at > ?1 ORDER BY started_at DESC"
    ).map_err(|e| format!("prepare: {}", e))?;

    let rows = stmt.query_map(rusqlite::params![cutoff_ts], |row| {
        Ok((
            row.get::<_, f64>(0)?,
            row.get::<_, i64>(1).unwrap_or(0),
            row.get::<_, i64>(2).unwrap_or(0),
            row.get::<_, i64>(3).unwrap_or(0),
            row.get::<_, i64>(4).unwrap_or(0),
            row.get::<_, i64>(5).unwrap_or(0),
            row.get::<_, String>(6).unwrap_or_default(),
        ))
    }).map_err(|e| format!("query: {}", e))?;

    for row in rows {
        let (ts, inp, out, cr, cw, msgs, mdl) = row.map_err(|e| format!("row: {}", e))?;
        let day = chrono::DateTime::from_timestamp(ts as i64, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        if let Some(entry) = daily_map.get_mut(&day) {
            entry.input_tokens += inp as u64;
            entry.output_tokens += out as u64;
            entry.cache_read_tokens += cr as u64;
            entry.cache_write_tokens += cw as u64;
            entry.messages += msgs as u64;
            entry.sessions += 1;
        }

        total_input += inp as u64;
        total_output += out as u64;
        total_cache_read += cr as u64;
        total_cache_write += cw as u64;
        total_messages += msgs as u64;
        total_sessions += 1;
        if model.is_empty() && !mdl.is_empty() {
            model = mdl;
        }
    }

    let daily_stats: Vec<ClaudeDailyStats> = daily_map.into_values().collect();

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
    // Claude Code Desktop's macOS bundle short name is "Claude" while the
    // host_terminal we tag is "Claude Desktop" (matches the Windows label).
    if host_terminal == "Claude Desktop" && frontmost.eq_ignore_ascii_case("Claude") {
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

            let is_desktop_hosted = session.host_terminal.as_deref() == Some("Claude Desktop");
            if session.source == "cursor" || session.source == "codex" || is_desktop_hosted {
                // Cursor/Codex/CC Desktop: timeout-based staleness (120s without any event update).
                // Hook PIDs are not stable enough for PID-alive checks in these environments.
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
    // Supplement with recent Hermes sessions from state.db so historical
    // sessions appear even if oc-claw was started after the Hermes session.
    // Also backfill user_prompt for live Hermes sessions that lack it.
    // Skip sessions the user has explicitly dismissed.
    let dismissed = state.dismissed.lock().map(|d| d.clone()).unwrap_or_default();
    let live_hermes_ids: std::collections::HashSet<String> = list.iter()
        .filter(|s| s.source == "hermes")
        .map(|s| s.session_id.clone())
        .collect();
    if let Ok(db_sessions) = load_recent_hermes_sessions_from_db() {
        for dbs in db_sessions {
            if dismissed.contains(&dbs.session_id) {
                continue;
            }
            if live_hermes_ids.contains(&dbs.session_id) {
                // Backfill user_prompt for live sessions missing it
                if let Some(ref prompt) = dbs.user_prompt {
                    if let Some(live) = list.iter_mut().find(|s| s.source == "hermes" && s.session_id == dbs.session_id) {
                        if live.user_prompt.is_none() || live.user_prompt.as_deref() == Some("") {
                            live.user_prompt = Some(prompt.clone());
                        }
                    }
                }
            } else {
                list.push(dbs);
            }
        }
    }

    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(list)
}

/// Load recent Hermes sessions (last 2h) from ~/.hermes/state.db.
fn load_recent_hermes_sessions_from_db() -> Result<Vec<ClaudeSession>, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let db_path = home.join(".hermes").join("state.db");
    if !db_path.exists() {
        return Ok(vec![]);
    }

    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|e| format!("open hermes state.db: {}", e))?;

    let cutoff = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_secs() as f64 - 7200.0;

    let mut stmt = conn.prepare(
        "SELECT id, source, model, started_at, ended_at, message_count, input_tokens, output_tokens \
         FROM sessions WHERE started_at > ?1 ORDER BY started_at DESC LIMIT 20"
    ).map_err(|e| format!("prepare: {}", e))?;

    let rows = stmt.query_map(rusqlite::params![cutoff], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, String>(2).unwrap_or_default(),
            row.get::<_, f64>(3).unwrap_or(0.0),
            row.get::<_, Option<f64>>(4).unwrap_or(None),
            row.get::<_, i64>(5).unwrap_or(0),
            row.get::<_, i64>(6).unwrap_or(0),
            row.get::<_, i64>(7).unwrap_or(0),
        ))
    }).map_err(|e| format!("query: {}", e))?;

    let mut sessions = Vec::new();
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

    // Prepare statements for fetching last user/assistant message per session
    let mut user_stmt = conn.prepare(
        "SELECT substr(content, 1, 200) FROM messages \
         WHERE session_id = ?1 AND role = 'user' ORDER BY timestamp DESC LIMIT 1"
    ).map_err(|e| format!("prepare user msg: {}", e))?;

    for row in rows {
        let (id, source, _model, started_at, ended_at, _msg_count, _input_tok, _output_tok) =
            row.map_err(|e| format!("row: {}", e))?;

        let updated_at_ms = if let Some(end) = ended_at {
            if end > 0.0 { (end * 1000.0) as u64 } else { (started_at * 1000.0) as u64 }
        } else {
            (started_at * 1000.0) as u64
        };

        let is_active = ended_at.is_none() || ended_at == Some(0.0);
        let status = if is_active && (now_ms - updated_at_ms) < 300_000 {
            "processing".to_string()
        } else {
            "stopped".to_string()
        };

        let platform = source.clone();

        // Only show Gateway sessions (feishu, slack, etc.), skip CLI sessions
        if platform == "cli" || platform == "terminal" || platform.is_empty() {
            continue;
        }

        let user_prompt: Option<String> = user_stmt
            .query_row(rusqlite::params![&id], |r| r.get(0)).ok();
        // last_response intentionally set to None for DB-loaded sessions:
        // it is only used for real-time completion notifications, and
        // historical sessions should never trigger the auto-expand popup.

        sessions.push(ClaudeSession {
            session_id: id,
            status,
            source: "hermes".to_string(),
            cwd: "~/.hermes".to_string(),
            tool: None,
            tool_input: None,
            user_prompt,
            interactive: false,
            updated_at: updated_at_ms,
            is_processing: is_active && (now_ms - updated_at_ms) < 300_000,
            pid: None,
            pending_agents: 0,
            permission_suggestions: None,
            last_response: None,
            is_active_tab: false,
            terminal_id: None,
            host_terminal: None,
            platform: Some(platform),
            cursor_port: None,
            cursor_workspace_root: None,
            cursor_workspace_name: None,
            cursor_native_handle: None,
        });
    }

    Ok(sessions)
}

#[tauri::command]
async fn remove_claude_session(session_id: String, state: tauri::State<'_, ClaudeState>) -> Result<(), String> {
    let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    sessions.remove(&session_id);
    drop(sessions);
    if let Ok(mut dismissed) = state.dismissed.lock() {
        dismissed.insert(session_id);
    }
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
    let (tool_name, source) = {
        let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
        let s = sessions.get(&session_id);
        (
            s.and_then(|s| s.tool.clone()),
            s.map(|s| s.source.clone()).unwrap_or_else(|| "cc".to_string()),
        )
    };

    // Codex permissions are intentionally approved in Codex's native UI.
    // The oc-claw popup only serves as a reminder + jump action and must not
    // make the final allow/deny decision for Codex sessions.
    if source == "codex" {
        log::info!(
            "[resolve_permission] ignore local decision='{}' for codex session={}",
            decision,
            &session_id[..session_id.len().min(8)],
        );
        return Ok(());
    }

    let response_json = match decision.as_str() {
            "deny" => serde_json::json!({
                "continue": true,
                "suppressOutput": true,
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "deny" }
                }
            })
            .to_string(),
            "allow_once" => serde_json::json!({
                "continue": true,
                "suppressOutput": true,
                "hookSpecificOutput": {
                    "hookEventName": "PermissionRequest",
                    "decision": { "behavior": "allow" }
                }
            })
            .to_string(),
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
                })
                .to_string()
            }
            "auto_approve" => serde_json::json!({
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
            })
            .to_string(),
            _ => return Err(format!("Unknown decision: {}", decision)),
    };

    let tx = {
        let mut map = state.pending_permissions.lock().map_err(|e| e.to_string())?;
        map.remove(&session_id)
    };

    if let Some(tx) = tx {
        if cfg!(debug_assertions) {
            log::info!(
                "[resolve_permission] sending decision='{}' tool={:?} session={} response_json={}",
                decision,
                tool_name,
                &session_id[..session_id.len().min(8)],
                response_json,
            );
        } else {
            log::info!(
                "[resolve_permission] sent '{}' tool={:?} for session={}",
                decision,
                tool_name,
                &session_id[..session_id.len().min(8)],
            );
        }
        tx.send(response_json).map_err(|_| "Failed to send permission response".to_string())?;
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

    // Cursor doesn't expose reliable token usage:
    //   - Its transcript JSONL has no usage fields at all.
    //   - Its `bubbleId:*` SQLite entries used to carry tokenCount but Cursor
    //     stopped writing it to the local DB around 2026-04 (recent bubbles
    //     are all 0/0). The hook stop event only sees usage for sessions that
    //     run after the user enables tracking, so it would underreport.
    // Returning empty stats lets the frontend render a "not supported" hint
    // instead of mixing in Claude Code's totals as if they were Cursor's.
    if source == "cursor" {
        return Ok(empty_claude_stats());
    }

    if source == "gemini" {
        return Ok(empty_claude_stats());
    }

    if source == "hermes" {
        return collect_hermes_stats();
    }

    let jsonl_files = match source.as_str() {
        "codex" => collect_codex_session_jsonl_files(),
        "cc" | "claude" => collect_claude_project_jsonl_files(),
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
    {
        // `cmd /C start ""` opens the URL in the default browser, but cmd
        // itself is a console app so without CREATE_NO_WINDOW the user
        // sees a black console flash next to the freshly opened browser
        // tab. Hide it.
        let mut cmd = std::process::Command::new("cmd");
        cmd.args(["/C", "start", "", &url]);
        hide_window_cmd(&mut cmd);
        cmd.spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    { std::process::Command::new("xdg-open").arg(&url).spawn().map_err(|e| e.to_string())?; }
    Ok(())
}

#[derive(Debug, Serialize)]
pub struct CodexPetMeta {
    pub id: String,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub description: String,
    #[serde(rename = "spritesheetUrl")]
    pub spritesheet_url: String,
}

/// Path to the user's codex CLI pets directory (`~/.codex/pets`). Mirrors
/// the layout used by the codex CLI hatch-pet skill so users can drop the
/// same pet folders here and have them show up in the picker.
fn codex_pets_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".codex").join("pets"))
}

/// List custom codex pets the user has dropped into `~/.codex/pets`. Each
/// pet folder must contain a `pet.json` metadata file plus a spritesheet
/// (.webp/.png/.jpg). Missing pieces are skipped silently.
#[tauri::command]
async fn list_custom_codex_pets() -> Result<Vec<CodexPetMeta>, String> {
    let Some(root) = codex_pets_dir() else {
        return Ok(Vec::new());
    };
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    let entries = match std::fs::read_dir(&root) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let pet_json = path.join("pet.json");
        if !pet_json.is_file() {
            continue;
        }
        let raw = match std::fs::read_to_string(&pet_json) {
            Ok(s) => s,
            Err(_) => continue,
        };
        let meta: serde_json::Value = match serde_json::from_str(&raw) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = meta
            .get("id")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| {
                path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default()
            });
        let display_name = meta
            .get("displayName")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| id.clone());
        let description = meta
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_default();
        let sheet_path = meta
            .get("spritesheetPath")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(|| "spritesheet.webp".into());
        let abs = path.join(&sheet_path);
        if !abs.is_file() {
            continue;
        }
        let url = codex_asset_url(&abs);
        out.push(CodexPetMeta {
            id,
            display_name,
            description,
            spritesheet_url: url,
        });
    }
    out.sort_by(|a, b| a.display_name.to_lowercase().cmp(&b.display_name.to_lowercase()));
    Ok(out)
}

/// Forward a frontend diagnostic line to the dev terminal so debugging
/// modal/blur/exit paths doesn't require opening webview DevTools.
#[tauri::command]
async fn debug_log(scope: String, msg: String) -> Result<(), String> {
    // Diagnostic-only: keep frontend state/poll/mascot trace lines in debug
    // builds where they help pinpoint UI bugs, drop them in release builds
    // so the log file stays focused on real events.
    if cfg!(debug_assertions) {
        log::info!("[fe:{}] {}", scope, msg);
    }
    Ok(())
}

/// Spawn a demo-mode mini mascot window. Each window runs the bundled
/// frontend with `?demo=1&pet=<id>` query params, which routes to a
/// minimal mascot-only React tree. Used by the dev-mode "演示模式" toggle
/// to drop multiple animated mascots on screen for demo recordings.
#[tauri::command]
async fn spawn_demo_mascot(app: tauri::AppHandle, pet_id: String) -> Result<String, String> {
    use std::sync::atomic::AtomicU64;
    static DEMO_COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = DEMO_COUNTER.fetch_add(1, Ordering::SeqCst);
    let label = format!("demo-mascot-{}", n);

    let url = format!("index.html#/mini?demo=1&pet={}", pet_id);
    let win = tauri::WebviewWindowBuilder::new(
        &app,
        label.clone(),
        tauri::WebviewUrl::App(url.into()),
    )
    .title("oc-claw demo mascot")
    .inner_size(96.0, 96.0)
    .min_inner_size(96.0, 96.0)
    .resizable(false)
    .decorations(false)
    .transparent(true)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .build()
    .map_err(|e| e.to_string())?;

    // Position the demo window in a known-good area near the top-right
    // of the screen, stepping each subsequent spawn by one collapsed
    // mascot width so they line up next to each other. Avoiding the
    // main mini window's frame keeps us correct even when the user is
    // currently in settings (where the main window is 600px wide and
    // would otherwise push the demos off-screen).
    const DEMO_STEP_W: f64 = 96.0;
    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        let _ = app.run_on_main_thread(move || {
            use objc2::msg_send;
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2_foundation::{NSPoint, NSRect, NSSize};
            if let Ok(demo_ns) = win_clone.ns_window() {
                let demo_obj = unsafe { &*(demo_ns as *mut AnyObject) };

                // Pull the active screen frame from NSScreen so we can
                // anchor relative to the visible area rather than guessing.
                let screen_frame: Option<NSRect> = unsafe {
                    AnyClass::get(c"NSScreen").and_then(|cls| {
                        let screens: *mut AnyObject = msg_send![cls, screens];
                        if screens.is_null() {
                            return None;
                        }
                        let count: usize = msg_send![&*screens, count];
                        if count == 0 {
                            return None;
                        }
                        let screen: *mut AnyObject = msg_send![&*screens, objectAtIndex: 0usize];
                        if screen.is_null() {
                            return None;
                        }
                        let frame: NSRect = msg_send![&*screen, frame];
                        Some(frame)
                    })
                };
                let Some(sf) = screen_frame else { return };

                // Right-aligned baseline anchor: ~120pt below the menu
                // bar on the right edge, then step left by one mascot
                // width per spawn.
                let baseline_x = sf.origin.x + sf.size.width - DEMO_STEP_W * 2.0;
                let baseline_y = sf.origin.y + sf.size.height - DEMO_STEP_W - MASCOT_TOP_INSET;
                let x = baseline_x - (n as f64) * DEMO_STEP_W;
                let new_origin = NSPoint::new(x.max(sf.origin.x), baseline_y);
                let new_frame = NSRect::new(new_origin, NSSize::new(DEMO_STEP_W, DEMO_STEP_W));

                unsafe {
                    let _: () = msg_send![demo_obj, setLevel: 27isize];
                    let _: () = msg_send![demo_obj, setFrame: new_frame, display: true, animate: false];
                    let behavior: usize = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 6);
                    let _: () = msg_send![demo_obj, setCollectionBehavior: behavior];
                    let _: () = msg_send![demo_obj, setAcceptsMouseMovedEvents: true];
                }
            }
        });
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(Some(monitor)) = win.current_monitor() {
            let scale = monitor.scale_factor();
            let mp = monitor.position();
            let mx = mp.x as f64 / scale;
            let my = mp.y as f64 / scale;
            let sw = monitor.size().width as f64 / scale;
            let baseline_x = mx + sw - DEMO_STEP_W * 2.0;
            let baseline_y = my + MASCOT_TOP_INSET;
            let x = (baseline_x - (n as f64) * DEMO_STEP_W).max(mx);
            let _ = win.set_position(tauri::LogicalPosition::new(x, baseline_y));
        }
        let _ = win.set_always_on_top(true);
    }
    let _ = win.show();
    Ok(label)
}

/// Close a single spawned demo mascot window by label.
#[tauri::command]
async fn close_demo_mascot(app: tauri::AppHandle, label: String) -> Result<bool, String> {
    if !label.starts_with("demo-mascot-") {
        return Err("invalid demo mascot label".into());
    }
    if let Some(win) = app.get_webview_window(&label) {
        let _ = win.close();
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Close every spawned demo mascot window, leaving only the main mini.
#[tauri::command]
async fn close_demo_mascots(app: tauri::AppHandle) -> Result<u32, String> {
    let mut closed = 0u32;
    let labels: Vec<String> = app
        .webview_windows()
        .keys()
        .filter(|l| l.starts_with("demo-mascot-"))
        .cloned()
        .collect();
    for label in labels {
        if let Some(win) = app.get_webview_window(&label) {
            let _ = win.close();
            closed += 1;
        }
    }
    Ok(closed)
}

/// Open the platform's native folder picker so the user can choose a
/// codex pet directory to import. Returns the absolute path or `null` if
/// the user cancelled. Implemented with `osascript` on macOS and
/// PowerShell's `FolderBrowserDialog` on Windows so we don't need to add
/// `tauri-plugin-dialog` just for this one flow.
// macOS occasionally demotes our floating mini window back to the normal
// NSWindow level after a foreign helper (osascript, NSOpenPanel-driven
// pickers, etc.) takes focus. Re-apply level 27 (status) and reassert
// always-on-top so the mascot/settings panel stays on top of everything.
//
// All AppKit work is dispatched to the main thread — calling NSWindow
// methods from the Tauri command (runtime) thread trips AppKit's
// main-thread assertions and aborts the app with SIGTERM.
fn reassert_mini_floating(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(win) = app.get_webview_window("mini") else {
        return;
    };
    let win_clone = win.clone();
    let _ = app.run_on_main_thread(move || {
        #[cfg(target_os = "macos")]
        {
            use objc2::runtime::AnyObject;
            use objc2::msg_send;
            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
                unsafe {
                    let _: () = msg_send![obj, setLevel: 27isize];
                    let behavior: usize = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 6);
                    let _: () = msg_send![obj, setCollectionBehavior: behavior];
                }
            }
        }
        let _ = win_clone.set_always_on_top(true);
    });
}

#[tauri::command]
async fn reassert_floating(app: tauri::AppHandle) -> Result<(), String> {
    reassert_mini_floating(&app);
    Ok(())
}

#[tauri::command]
async fn pick_codex_pet_folder(app: tauri::AppHandle) -> Result<Option<String>, String> {
    use tauri::Manager;
    use tauri_plugin_dialog::DialogExt;

    // Use the official tauri-plugin-dialog so we get a real native folder
    // picker on every platform: NSOpenPanel on macOS, IFileOpenDialog
    // (modern explorer-style) on Windows, GTK on Linux.
    //
    // Bind the dialog to the mini window as its parent. Without an explicit
    // owner the dialog renders as a peer top-level window that sits below
    // our always-on-top mini frame on Windows (the user sees the picker
    // visually behind the settings panel on the second open). With a parent
    // window, the OS layers the dialog above its owner.
    let (tx, rx) = tokio::sync::oneshot::channel();
    let mut builder = app.dialog().file().set_title("选择 codex 宠物文件夹");
    if let Some(win) = app.get_webview_window("mini") {
        builder = builder.set_parent(&win);
    }
    builder.pick_folder(move |path| {
        let _ = tx.send(path);
    });
    let picked = rx.await.map_err(|e| e.to_string())?;
    let result = picked.and_then(|p| p.into_path().ok())
        .map(|p| p.to_string_lossy().into_owned());

    // The dialog briefly steals focus and the OS can demote our floating
    // mini window back to the normal level. Re-apply always-on-top so the
    // settings panel doesn't visually sink under other apps.
    reassert_mini_floating(&app);
    Ok(result)
}

/// Open `~/.codex/pets` in the platform's file manager. Creates the
/// directory if it doesn't exist yet so the picker's "Open Folder" link
/// always lands somewhere usable.
#[tauri::command]
async fn open_codex_pets_dir() -> Result<String, String> {
    let Some(dir) = codex_pets_dir() else {
        return Err("home directory not found".into());
    };
    if !dir.exists() {
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    }
    let path = dir.to_string_lossy().to_string();
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(&path).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer").arg(&path).spawn().map_err(|e| e.to_string())?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(&path).spawn().map_err(|e| e.to_string())?;
    }
    Ok(path)
}

/// Import a dropped pet folder into `~/.codex/pets`. The source must be a
/// directory containing at minimum a `pet.json` and a spritesheet image.
/// Existing folders with the same id are overwritten so re-dropping a
/// pet upgrades it in place.
#[tauri::command]
async fn import_codex_pet(src_path: String) -> Result<CodexPetMeta, String> {
    let src = PathBuf::from(&src_path);
    if !src.is_dir() {
        return Err(format!("not a directory: {}", src_path));
    }
    let pet_json = src.join("pet.json");
    if !pet_json.is_file() {
        return Err("missing pet.json in dropped folder".into());
    }
    let raw = std::fs::read_to_string(&pet_json).map_err(|e| e.to_string())?;
    let meta: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    let id = meta
        .get("id")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| {
            src.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "pet".into())
        });
    let Some(root) = codex_pets_dir() else {
        return Err("home directory not found".into());
    };
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let dst = root.join(&id);
    if dst.exists() {
        let _ = std::fs::remove_dir_all(&dst);
    }
    copy_dir_recursive(&src, &dst)?;
    let display_name = meta
        .get("displayName")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| id.clone());
    let description = meta
        .get("description")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_default();
    let sheet_path = meta
        .get("spritesheetPath")
        .and_then(|v| v.as_str())
        .map(String::from)
        .unwrap_or_else(|| "spritesheet.webp".into());
    let url = codex_asset_url(&dst.join(&sheet_path));
    Ok(CodexPetMeta {
        id,
        display_name,
        description,
        spritesheet_url: url,
    })
}

/// Build a Tauri custom-protocol URL the webview can fetch. The path is
/// resolved relative to `~/.codex/pets/` by the `codexpet://` scheme
/// registered on the Tauri builder, so files outside the bundled
/// `public/` folder still load.
fn codex_asset_url(abs: &std::path::Path) -> String {
    use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
    let Some(root) = codex_pets_dir() else {
        return String::new();
    };
    let rel = match abs.strip_prefix(&root) {
        Ok(p) => p.to_path_buf(),
        Err(_) => return String::new(),
    };
    let parts: Vec<String> = rel
        .components()
        .map(|c| utf8_percent_encode(&c.as_os_str().to_string_lossy(), NON_ALPHANUMERIC).to_string())
        .collect();
    // Windows WebView2 cannot fetch `<scheme>://...` URLs registered via
    // `register_uri_scheme_protocol`; Tauri exposes them as
    // `http://<scheme>.localhost/...` instead. Match the same convention as
    // localasset/customasset so codex pet sprites actually load.
    let prefix = if cfg!(target_os = "windows") {
        "http://codexpet.localhost"
    } else {
        "codexpet://localhost"
    };
    format!("{}/{}", prefix, parts.join("/"))
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            std::fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
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

    // Windows paths are case-insensitive (NTFS preserves but does not honour
    // case). Cursor's hook may report `c:\foo` while the extension reports
    // `C:\foo`, so do a case-insensitive comparison there. Drive letters and
    // most workspace dir names are ASCII; for the rare non-ASCII path we still
    // win on the ASCII drive-letter portion which is the usual culprit.
    #[cfg(target_os = "windows")]
    {
        let cwd_l = cwd.to_lowercase();
        let root_l = workspace_root.to_lowercase();
        if cwd_l == root_l {
            return true;
        }
        return cwd_l
            .strip_prefix(&root_l)
            .map(|rest| rest.starts_with('/') || rest.starts_with('\\'))
            .unwrap_or(false);
    }

    #[cfg(not(target_os = "windows"))]
    {
        if cwd == workspace_root {
            return true;
        }
        cwd.strip_prefix(workspace_root)
            .map(|rest| rest.starts_with('/') || rest.starts_with('\\'))
            .unwrap_or(false)
    }
}

fn cursor_workspace_name_from_path(path_str: &str) -> String {
    std::path::Path::new(path_str)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_string()
}

/// Recover Chinese characters mangled by Cursor's hook stdin pipeline on
/// CJK Windows. The pipeline writes UTF-8 bytes that have already been
/// passed through a GBK round-trip somewhere upstream, so what arrives at
/// our hook is the UTF-8 of the GBK-mis-interpreted version of the original
/// text (e.g. user types "你好" but we see "浣犲ソ").
///
/// To reverse it: encode the received UTF-8 text as GBK (which gives back the
/// original UTF-8 byte sequence), then decode those bytes as UTF-8.
///
/// Returns `Some(recovered)` only when both directions succeed cleanly. ASCII
/// passes through unchanged in both directions; correctly-encoded CJK falls
/// through unchanged because GBK→UTF-8 would produce invalid UTF-8 (the
/// detection self-gates).
#[cfg(target_os = "windows")]
fn try_recover_cursor_mojibake(buf: &str) -> Option<String> {
    use encoding_rs::{GBK, UTF_8};

    if buf.is_ascii() {
        return None;
    }
    let (gbk_bytes, _, encode_errors) = GBK.encode(buf);
    if encode_errors {
        return None;
    }
    let (decoded, _, decode_errors) = UTF_8.decode(&gbk_bytes);
    if decode_errors {
        return None;
    }
    Some(decoded.into_owned())
}

/// Normalize a path string reported by Cursor's hook payload.
///
/// Cursor's `workspace_roots` field uses URI-style forward-slash paths even on
/// Windows, e.g. `/g:/Desktop/code`. To match against the extension's
/// `vscode.workspace.workspaceFolders[].uri.fsPath` (which returns native
/// `g:\Desktop\code`) we strip a leading slash that precedes a drive letter
/// and convert separators to the platform's preferred form on Windows. On
/// Unix we leave the path untouched.
fn normalize_cursor_path(raw: &str) -> String {
    if raw.is_empty() {
        return String::new();
    }

    #[cfg(target_os = "windows")]
    {
        // /x:/foo  → x:/foo
        let bytes = raw.as_bytes();
        let stripped = if bytes.len() >= 4
            && bytes[0] == b'/'
            && (bytes[1] as char).is_ascii_alphabetic()
            && bytes[2] == b':'
            && (bytes[3] == b'/' || bytes[3] == b'\\')
        {
            &raw[1..]
        } else {
            raw
        };
        return stripped.replace('/', "\\");
    }

    #[cfg(not(target_os = "windows"))]
    {
        raw.to_string()
    }
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

/// Walk parent process chain on Windows to find CC Desktop (claude.exe in WindowsApps).
/// Returns the PID of the CC Desktop main process if the given PID is a descendant of it.
#[cfg(target_os = "windows")]
fn find_claude_desktop_parent_pid(pid: u32) -> Option<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};

    // Take a snapshot of all processes to walk the parent chain
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()? };
    let mut entries: Vec<(u32, u32)> = Vec::new(); // (pid, parent_pid)
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            while Process32NextW(snapshot, &mut entry).is_ok() {
                entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            }
        }
        let _ = CloseHandle(snapshot);
    }

    // Walk up the parent chain from the given PID (max 10 hops).
    // Only match claude.exe in WindowsApps (the GUI Electron app with a window).
    // claude-3p\claude-code\...\claude.exe is the headless CLI — skip it.
    let mut current = pid;
    for _ in 0..10 {
        let parent = entries.iter().find(|(p, _)| *p == current).map(|(_, pp)| *pp)?;
        if parent == 0 || parent == current {
            return None;
        }
        if let Some(exe_path) = get_process_exe_path(parent) {
            let exe_lower = exe_path.to_lowercase();
            if (exe_lower.ends_with("\\claude.exe") || exe_lower.ends_with("/claude.exe"))
                && exe_lower.contains("windowsapps")
            {
                return Some(parent);
            }
        }
        current = parent;
    }
    None
}

/// Get the full exe path for a process on Windows.
#[cfg(target_os = "windows")]
fn get_process_exe_path(pid: u32) -> Option<String> {
    use windows::Win32::System::Threading::{OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION};
    use windows::Win32::Foundation::{CloseHandle, MAX_PATH};

    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
        let mut buf = [0u16; MAX_PATH as usize];
        let mut size: u32 = buf.len() as u32;
        let result = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buf.as_mut_ptr()),
            &mut size,
        );
        let _ = CloseHandle(handle);
        if result.is_ok() {
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        } else {
            None
        }
    }
}

/// Activate a window by its owning process PID on Windows.
/// Finds visible top-level windows owned by the given PID and brings the best one to front.
#[cfg(target_os = "windows")]
fn activate_window_by_pid(target_pid: u32) {
    use windows::Win32::Foundation::{BOOL, HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindowAsync, SW_RESTORE,
    };

    struct EnumCtx {
        target_pid: u32,
        best_hwnd: Option<isize>,
        best_title_len: i32,
    }

    extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let ctx = &mut *(lparam.0 as *mut EnumCtx);
            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid != ctx.target_pid {
                return BOOL(1);
            }
            let title_len = GetWindowTextLengthW(hwnd);
            if title_len > ctx.best_title_len {
                ctx.best_title_len = title_len;
                ctx.best_hwnd = Some(hwnd.0 as isize);
            }
            BOOL(1)
        }
    }

    let mut ctx = EnumCtx {
        target_pid,
        best_hwnd: None,
        best_title_len: -1,
    };
    unsafe {
        let _ = EnumWindows(Some(enum_proc), LPARAM(&mut ctx as *mut EnumCtx as isize));
    }
    if let Some(hwnd_val) = ctx.best_hwnd {
        unsafe {
            let hwnd = HWND(hwnd_val as *mut std::ffi::c_void);
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

/// Find any running CC Desktop main process (WindowsApps\...\Claude.exe).
/// Used as fallback when hook didn't capture a PID.
#[cfg(target_os = "windows")]
fn find_running_claude_desktop_pid() -> Option<u32> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::Foundation::CloseHandle;

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()? };
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut result = None;
    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let exe_name = String::from_utf16_lossy(
                    &entry.szExeFile[..entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(entry.szExeFile.len())]
                ).to_lowercase();
                if exe_name == "claude.exe" {
                    // Verify it's the WindowsApps GUI process, not the CLI
                    if let Some(path) = get_process_exe_path(entry.th32ProcessID) {
                        if path.to_lowercase().contains("windowsapps") {
                            result = Some(entry.th32ProcessID);
                            break;
                        }
                    }
                }
                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(snapshot);
    }
    result
}

/// Detect the host application for a CC process on Windows.
/// Returns a name like "Claude Desktop", "Windows Terminal", etc.
#[cfg(target_os = "windows")]
fn find_host_app_for_pid_win(pid: u32) -> Option<String> {
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS,
    };
    use windows::Win32::Foundation::CloseHandle;

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0).ok()? };
    let mut entries: Vec<(u32, u32)> = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    unsafe {
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            while Process32NextW(snapshot, &mut entry).is_ok() {
                entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            }
        }
        let _ = CloseHandle(snapshot);
    }

    let mut current = pid;
    for _ in 0..10 {
        let parent = entries.iter().find(|(p, _)| *p == current).map(|(_, pp)| *pp)?;
        if parent == 0 || parent == current {
            return None;
        }
        if let Some(exe_path) = get_process_exe_path(parent) {
            let exe_lower = exe_path.to_lowercase();
            if (exe_lower.ends_with("\\claude.exe") || exe_lower.ends_with("/claude.exe"))
                && exe_lower.contains("windowsapps")
            {
                return Some("Claude Desktop".to_string());
            }
            if exe_lower.ends_with("\\windowsterminal.exe") {
                return Some("Windows Terminal".to_string());
            }
            if exe_lower.ends_with("\\ghostty.exe") {
                return Some("Ghostty".to_string());
            }
        }
        current = parent;
    }
    None
}

/// Bring a Cursor window to front on Windows by enumerating top-level windows,
/// finding ones owned by `Cursor.exe`, and selecting the one whose title
/// contains the workspace name. Cursor window titles look like
/// `<file> - <workspace> - Cursor`. If no title matches we fall back to any
/// Cursor window so that at least the app is raised.
///
/// `SetForegroundWindow` is restricted by Windows: it only fully succeeds when
/// the caller is itself the foreground process. oc-claw normally is foreground
/// at click time (the user just clicked the mini panel), so this works in
/// practice. We additionally restore the window if it is minimized.
#[cfg(target_os = "windows")]
fn activate_cursor_workspace_window(workspace_name: &str) {
    use windows::Win32::Foundation::{BOOL, CloseHandle, HWND, LPARAM, MAX_PATH};
    use windows::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_FORMAT,
        PROCESS_QUERY_LIMITED_INFORMATION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
        IsIconic, IsWindowVisible, SetForegroundWindow, ShowWindowAsync, SW_RESTORE,
    };

    #[derive(Clone)]
    struct Hit {
        hwnd: isize,
        score: u32,
    }

    // EnumProcCtx is what the EnumWindows callback receives via LPARAM.
    // We pass a stable pointer to it from the outer call.
    struct EnumProcCtx<'a> {
        workspace_lower: &'a str,
        hits: Vec<Hit>,
    }

    extern "system" fn enum_proc(hwnd: HWND, lparam: LPARAM) -> BOOL {
        unsafe {
            let ctx = &mut *(lparam.0 as *mut EnumProcCtx);

            if !IsWindowVisible(hwnd).as_bool() {
                return BOOL(1);
            }
            let title_len = GetWindowTextLengthW(hwnd);
            if title_len <= 0 {
                return BOOL(1);
            }

            let mut pid: u32 = 0;
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
            if pid == 0 {
                return BOOL(1);
            }

            let handle = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                Ok(h) => h,
                Err(_) => return BOOL(1),
            };
            let mut buf = [0u16; MAX_PATH as usize];
            let mut size: u32 = buf.len() as u32;
            let exe_name = if QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                windows::core::PWSTR(buf.as_mut_ptr()),
                &mut size,
            )
            .is_ok()
            {
                String::from_utf16_lossy(&buf[..size as usize])
            } else {
                String::new()
            };
            let _ = CloseHandle(handle);

            let exe_lower = exe_name.to_lowercase();
            if !exe_lower.ends_with("\\cursor.exe") && !exe_lower.ends_with("/cursor.exe") {
                return BOOL(1);
            }

            let mut title_buf = vec![0u16; (title_len as usize) + 1];
            let n = GetWindowTextW(hwnd, &mut title_buf) as usize;
            let title_lower = String::from_utf16_lossy(&title_buf[..n]).to_lowercase();

            // Workspace-name substring match scores higher than a generic
            // Cursor window. Empty workspace falls back to any Cursor window.
            let score: u32 = if ctx.workspace_lower.is_empty() {
                1
            } else if title_lower.contains(ctx.workspace_lower) {
                100
            } else {
                1
            };

            ctx.hits.push(Hit {
                hwnd: hwnd.0 as isize,
                score,
            });
            BOOL(1)
        }
    }

    let workspace_lower = workspace_name.to_lowercase();
    let mut ctx = EnumProcCtx {
        workspace_lower: workspace_lower.as_str(),
        hits: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(
            Some(enum_proc),
            LPARAM(&mut ctx as *mut EnumProcCtx as isize),
        );
    }

    ctx.hits.sort_by(|a, b| b.score.cmp(&a.score));

    if let Some(best) = ctx.hits.first() {
        unsafe {
            let hwnd = HWND(best.hwnd as *mut std::ffi::c_void);
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindowAsync(hwnd, SW_RESTORE);
            }
            let _ = SetForegroundWindow(hwnd);
        }
        log::info!(
            "[cursor_focus_win] raised hwnd={} score={} (workspace='{}')",
            best.hwnd, best.score, workspace_name,
        );
    } else {
        log::info!(
            "[cursor_focus_win] no Cursor.exe window found for workspace='{}'",
            workspace_name,
        );
    }
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

    #[cfg(any(target_os = "macos", target_os = "windows"))]
    activate_cursor_workspace_window(&workspace_name);

    if let Some(port) = port {
        let focused = post_cursor_window_action(port, "/focus-window", "{}");
        log::info!("[focus_cursor] POST /focus-window to port {} → {}", port, focused);
        if focused {
            return Ok(format!("Focused Cursor window on port {}", port));
        }
        return Ok(format!("Activated Cursor window but /focus-window failed on port {}", port));
    }

    #[cfg(any(target_os = "macos", target_os = "windows"))]
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
    let host_terminal = session.host_terminal.clone();
    let platform = session.platform.clone();
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

        // CC Desktop sessions must short-circuit before the Ghostty fast path.
        // The unix hook script unconditionally captures Ghostty's current
        // front-tab id whenever Ghostty is running (it predates CC Desktop
        // support and assumed CC always lives inside a terminal). For sessions
        // launched by Claude.app, that tid points to an unrelated Ghostty tab,
        // and without this shortcut the fast path would activate Ghostty and
        // return early instead of surfacing Claude.app. Match Codex's pattern:
        // try the GUI app first, fall back to bundle-id AppleScript so a
        // relocated/renamed bundle still gets focused.
        if host_terminal.as_deref() == Some("Claude Desktop") {
            let opened = std::process::Command::new("open")
                .args(["-a", "Claude"])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            if !opened {
                let _ = std::process::Command::new("osascript")
                    .args(["-e", r#"tell application id "com.anthropic.claudefordesktop" to activate"#])
                    .output();
            }
            return Ok("Jumped to Claude Desktop".to_string());
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
        } else if source == "gemini" {
            for app_name in ["Ghostty", "Terminal", "iTerm2", "iTerm", "Warp"] {
                if try_activate_app(app_name) {
                    return Ok(format!("Activated {}", app_name));
                }
            }
            return Err("No PID tracked for this Gemini session".to_string());
        } else if source == "hermes" {
            // Hermes supports multiple platforms. Use the platform field to
            // decide which application to activate, like OpenClaw does for
            // Feishu/Lark/Discord/Slack channels.
            let plat = platform.as_deref().unwrap_or("unknown").to_lowercase();
            let app_name = if plat.contains("feishu") || plat.contains("lark") {
                Some("Lark")
            } else if plat.contains("telegram") {
                Some("Telegram")
            } else if plat.contains("discord") {
                Some("Discord")
            } else if plat.contains("slack") {
                Some("Slack")
            } else if plat.contains("wechat") || plat.contains("weixin") {
                Some("WeChat")
            } else if plat.contains("whatsapp") {
                Some("WhatsApp")
            } else if plat.contains("mattermost") {
                Some("Mattermost")
            } else {
                None
            };
            if let Some(name) = app_name {
                if try_activate_app(name) {
                    return Ok(format!("Activated {}", name));
                }
            }
            // Fallback: try common terminal apps
            for term in ["Ghostty", "Terminal", "iTerm2", "iTerm", "Warp"] {
                if try_activate_app(term) {
                    return Ok(format!("Activated {}", term));
                }
            }
            return Err(format!("No PID tracked for this Hermes session (platform: {})", plat));
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
            Some("Claude Desktop") => {
                // The macOS app bundle is `/Applications/Claude.app` and its
                // AppleScript-resolvable name is `Claude` (CFBundleName), not
                // `Claude Desktop`. Activating via `open -a Claude` works for
                // both running and not-yet-launched cases; fall back to a
                // bundle-id AppleScript so a renamed/relocated bundle still
                // gets focused.
                let opened = std::process::Command::new("open")
                    .args(["-a", "Claude"])
                    .output()
                    .map(|o| o.status.success())
                    .unwrap_or(false);
                if !opened {
                    let _ = std::process::Command::new("osascript")
                        .args(["-e", r#"tell application id "com.anthropic.claudefordesktop" to activate"#])
                        .output();
                }
                Ok("Jumped to Claude Desktop".to_string())
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

    #[cfg(target_os = "windows")]
    {
        let is_desktop = host_terminal.as_deref() == Some("Claude Desktop");
        if is_desktop {
            if let Some(cc_pid) = pid {
                if let Some(desktop_pid) = find_claude_desktop_parent_pid(cc_pid) {
                    activate_window_by_pid(desktop_pid);
                    return Ok("Activated Claude Desktop window".to_string());
                }
            }
            if let Some(desktop_pid) = find_running_claude_desktop_pid() {
                activate_window_by_pid(desktop_pid);
                return Ok("Activated Claude Desktop window".to_string());
            }
        }
        Ok("No action".to_string())
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if !cwd.is_empty() {
            let _ = std::process::Command::new("xdg-open").arg(&cwd).spawn();
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

        // Claude Code Desktop on macOS launches the CLI from
        // `~/Library/Application Support/Claude-3p/.../claude` and its
        // ancestors live under `/Applications/Claude.app/`. Neither path
        // matches a known terminal name, so detect the desktop app via path
        // markers and tag the host accordingly. Keep this label in sync with
        // the Windows side (`find_host_app_for_pid_win`) so the frontend can
        // gate sessions through the same `enable_claude_desktop` toggle and
        // the active-tab check (`frontmost_matches_host_terminal`) can
        // suppress the completion popup when Claude.app is frontmost.
        if comm.contains("Claude-3p") || comm.contains("/Applications/Claude.app/") {
            return Some("Claude Desktop".to_string());
        }

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

    // Pass through any `ui` block as-is. This lets us push UI-level
    // config (e.g. the codex-pets.net URL inside the Create section)
    // without shipping a new app build, while keeping the namespace
    // separated from the platform-specific update metadata above.
    let ui = json.get("ui").cloned().unwrap_or(serde_json::Value::Null);

    Ok(serde_json::json!({
        "current": current,
        "latest": latest,
        "hasUpdate": has_update,
        "url": url,
        "notes": notes,
        "ui": ui,
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
        None => {
            // Fallback: try Hermes state.db
            return load_hermes_conversation(&session_id);
        }
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

/// Return a summary of recent Hermes sessions for the stats detail view.
#[tauri::command]
async fn get_hermes_sessions_summary() -> Result<Vec<serde_json::Value>, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let db_path = home.join(".hermes").join("state.db");
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|e| format!("open state.db: {}", e))?;

    let mut stmt = conn.prepare(
        "SELECT id, source, started_at, message_count, input_tokens, output_tokens \
         FROM sessions ORDER BY started_at DESC LIMIT 30"
    ).map_err(|e| format!("prepare: {}", e))?;

    let mut user_stmt = conn.prepare(
        "SELECT substr(content, 1, 200) FROM messages \
         WHERE session_id = ?1 AND role = 'user' ORDER BY timestamp DESC LIMIT 1"
    ).map_err(|e| format!("prepare user: {}", e))?;

    let mut asst_stmt = conn.prepare(
        "SELECT substr(content, 1, 200) FROM messages \
         WHERE session_id = ?1 AND role = 'assistant' ORDER BY timestamp DESC LIMIT 1"
    ).map_err(|e| format!("prepare asst: {}", e))?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1).unwrap_or_default(),
            row.get::<_, f64>(2).unwrap_or(0.0),
            row.get::<_, i64>(3).unwrap_or(0),
            row.get::<_, i64>(4).unwrap_or(0),
            row.get::<_, i64>(5).unwrap_or(0),
        ))
    }).map_err(|e| format!("query: {}", e))?;

    let mut results = Vec::new();
    for row in rows {
        let (id, platform, started_at, msg_count, input_tok, output_tok) =
            row.map_err(|e| format!("row: {}", e))?;
        let last_user: String = user_stmt
            .query_row(rusqlite::params![&id], |r| r.get(0)).unwrap_or_default();
        let last_asst: String = asst_stmt
            .query_row(rusqlite::params![&id], |r| r.get(0)).unwrap_or_default();
        results.push(serde_json::json!({
            "sessionId": id,
            "platform": platform,
            "startedAt": started_at,
            "messageCount": msg_count,
            "inputTokens": input_tok,
            "outputTokens": output_tok,
            "lastUserMsg": last_user,
            "lastAssistantMsg": last_asst,
        }));
    }
    Ok(results)
}

/// Return recent activity entries across all Hermes sessions for the detail view.
/// Each entry has: type (user/assistant/tool), summary, timestamp.
/// Results are ordered newest-first so the frontend can display them directly.
#[tauri::command]
async fn get_hermes_recent_activity(session_id: String) -> Result<Vec<serde_json::Value>, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let db_path = home.join(".hermes").join("state.db");
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|e| format!("open state.db: {}", e))?;

    let (sql, params): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = if session_id.is_empty() {
        (
            "SELECT role, substr(content, 1, 200), tool_name, tool_calls, tool_call_id, timestamp \
             FROM messages WHERE role IN ('user', 'assistant', 'tool') \
             ORDER BY timestamp DESC LIMIT 60".to_string(),
            vec![],
        )
    } else {
        (
            "SELECT role, substr(content, 1, 200), tool_name, tool_calls, tool_call_id, timestamp \
             FROM messages WHERE role IN ('user', 'assistant', 'tool') AND session_id = ?1 \
             ORDER BY timestamp DESC LIMIT 60".to_string(),
            vec![Box::new(session_id.clone())],
        )
    };

    let mut stmt = conn.prepare(&sql).map_err(|e| format!("prepare: {}", e))?;

    let param_refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt.query_map(param_refs.as_slice(), |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, Option<String>>(4)?,
            row.get::<_, f64>(5)?,
        ))
    }).map_err(|e| format!("query: {}", e))?;

    let truncate = |s: &str, max_chars: usize| -> String {
        if s.chars().count() <= max_chars {
            s.to_string()
        } else {
            let end: String = s.chars().take(max_chars).collect();
            format!("{}…", end)
        }
    };

    // First pass: collect assistant tool_calls to build a call_id -> fn_name map
    struct RawRow {
        role: String,
        content: Option<String>,
        tool_name: Option<String>,
        tool_calls: Option<String>,
        tool_call_id: Option<String>,
        ts: f64,
    }
    let mut raw_rows = Vec::new();
    let mut call_id_to_name: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for row in rows {
        let (role, content, tool_name, tool_calls, tool_call_id, ts) = row.map_err(|e| format!("row: {}", e))?;
        if role == "assistant" {
            if let Some(ref tc_str) = tool_calls {
                if let Ok(tc_arr) = serde_json::from_str::<Vec<serde_json::Value>>(tc_str) {
                    for tc in &tc_arr {
                        let cid = tc.get("id").or_else(|| tc.get("call_id"))
                            .and_then(|v| v.as_str()).unwrap_or("").to_string();
                        let fname = tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown").to_string();
                        if !cid.is_empty() {
                            call_id_to_name.insert(cid, fname);
                        }
                    }
                }
            }
        }
        raw_rows.push(RawRow { role, content, tool_name, tool_calls, tool_call_id, ts });
    }

    let mut results = Vec::new();
    for r in &raw_rows {
        if r.role == "tool" {
            // Resolve tool name: from tool_name field, or from call_id -> fn_name map
            let name = r.tool_name.as_deref().unwrap_or("");
            let resolved_name = if name.is_empty() {
                r.tool_call_id.as_deref()
                    .and_then(|cid| call_id_to_name.get(cid))
                    .map(|s| s.as_str())
                    .unwrap_or("tool")
            } else {
                name
            };
            results.push(serde_json::json!({
                "type": "tool",
                "summary": format!("⚡ {}", resolved_name),
                "toolName": resolved_name,
                "timestamp": r.ts,
            }));
            continue;
        }

        if r.role == "assistant" {
            // Show tool call invocations
            if let Some(ref tc_str) = r.tool_calls {
                if let Ok(tc_arr) = serde_json::from_str::<Vec<serde_json::Value>>(tc_str) {
                    for tc in &tc_arr {
                        let fn_name = tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        results.push(serde_json::json!({
                            "type": "tool",
                            "summary": format!("⚡ {}", fn_name),
                            "toolName": fn_name,
                            "timestamp": r.ts,
                        }));
                    }
                }
            }
            // Text content
            if let Some(ref text) = r.content {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    results.push(serde_json::json!({
                        "type": "assistant",
                        "summary": truncate(trimmed, 60),
                        "timestamp": r.ts,
                    }));
                }
            }
            continue;
        }

        if r.role == "user" || r.role == "human" {
            let text = r.content.as_deref().unwrap_or("");
            let trimmed = text.trim();
            if trimmed.is_empty() { continue; }
            results.push(serde_json::json!({
                "type": "user",
                "summary": truncate(trimmed, 60),
                "timestamp": r.ts,
            }));
        }
    }
    Ok(results)
}

/// Return recent activity from a remote Hermes instance via SSH.
#[tauri::command]
async fn get_hermes_remote_recent_activity(ssh_host: String, ssh_user: String, session_id: String) -> Result<Vec<serde_json::Value>, String> {
    let sid_escaped = session_id.replace('\'', "");
    let py_script = format!(r#"
import json, os, sqlite3, sys
db = os.path.expanduser('~/.hermes/state.db')
if not os.path.exists(db):
    print('[]')
    exit(0)
conn = sqlite3.connect(db)
sid = '{sid}'
if sid:
    cur = conn.execute(
        'SELECT role, substr(content,1,200), tool_name, tool_calls, tool_call_id, timestamp '
        'FROM messages WHERE role IN ("user","assistant","tool") AND session_id = ? '
        'ORDER BY timestamp DESC LIMIT 30', (sid,))
else:
    cur = conn.execute(
        'SELECT role, substr(content,1,200), tool_name, tool_calls, tool_call_id, timestamp '
        'FROM messages WHERE role IN ("user","assistant","tool") '
        'ORDER BY timestamp DESC LIMIT 30')
rows = []
cid_map = {{}}
for r in cur.fetchall():
    role, content, tool_name, tool_calls, tool_call_id, ts = r
    rows.append((role, content, tool_name, tool_calls, tool_call_id, ts))
    if role == 'assistant' and tool_calls:
        try:
            for tc in json.loads(tool_calls):
                cid = tc.get('id') or tc.get('call_id','')
                fname = (tc.get('function') or {{}}).get('name','unknown')
                if cid: cid_map[cid] = fname
        except: pass
conn.close()
results = []
def trunc(s, n=60):
    return s[:n]+'...' if len(s)>n else s
for role, content, tool_name, tool_calls, tool_call_id, ts in rows:
    if role == 'tool':
        name = tool_name or ''
        if not name and tool_call_id:
            name = cid_map.get(tool_call_id, 'tool')
        if not name: name = 'tool'
        results.append({{'type':'tool','summary':'\u26a1 '+name,'toolName':name,'timestamp':ts}})
    elif role == 'assistant':
        if tool_calls:
            try:
                for tc in json.loads(tool_calls):
                    fname = (tc.get('function') or {{}}).get('name','unknown')
                    results.append({{'type':'tool','summary':'\u26a1 '+fname,'toolName':fname,'timestamp':ts}})
            except: pass
        if content and content.strip():
            results.append({{'type':'assistant','summary':trunc(content.strip()),'timestamp':ts}})
    elif role in ('user','human'):
        if content and content.strip():
            results.append({{'type':'user','summary':trunc(content.strip()),'timestamp':ts}})
print(json.dumps(results[:15]))
"#, sid = sid_escaped);
    let cmd = format!("python3 -c {}", shell_escape_single(&py_script));
    let output = ssh_exec(&ssh_host, &ssh_user, &cmd).await?;
    let trimmed = output.trim();
    let items: Vec<serde_json::Value> = serde_json::from_str(trimmed)
        .map_err(|e| format!("parse hermes remote activity: {}", e))?;
    Ok(items)
}

/// Load conversation messages for a Hermes session from ~/.hermes/state.db.
fn load_hermes_conversation(session_id: &str) -> Result<Vec<ChatMessage>, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let db_path = home.join(".hermes").join("state.db");
    if !db_path.exists() {
        return Ok(vec![]);
    }

    let conn = rusqlite::Connection::open_with_flags(
        &db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    ).map_err(|e| format!("open hermes state.db: {}", e))?;

    let mut stmt = conn.prepare(
        "SELECT role, content, tool_name, tool_calls, timestamp \
         FROM messages WHERE session_id = ?1 ORDER BY timestamp ASC LIMIT 500"
    ).map_err(|e| format!("prepare: {}", e))?;

    let rows = stmt.query_map(rusqlite::params![session_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?,
            row.get::<_, Option<String>>(2)?,
            row.get::<_, Option<String>>(3)?,
            row.get::<_, f64>(4)?,
        ))
    }).map_err(|e| format!("query: {}", e))?;

    let mut messages = Vec::new();
    for row in rows {
        let (role, content, tool_name, tool_calls, ts) = row.map_err(|e| format!("row: {}", e))?;

        // Format timestamp as ISO string
        let ts_secs = ts as i64;
        let ts_nanos = ((ts - ts_secs as f64) * 1_000_000_000.0) as u32;
        let timestamp = chrono::DateTime::from_timestamp(ts_secs, ts_nanos)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string());

        if role == "tool" {
            // Tool result: show tool_name and truncated content
            let name = tool_name.unwrap_or_default();
            let body = content.unwrap_or_default();
            let truncated = if body.len() > 300 {
                format!("{}…", &body[..300])
            } else {
                body
            };
            let text = if name.is_empty() {
                format!("[Tool result]\n{}", truncated)
            } else {
                format!("[Tool: {}]\n{}", name, truncated)
            };
            messages.push(ChatMessage { role: "assistant".to_string(), text, timestamp });
            continue;
        }

        if role == "assistant" {
            // Check for tool_calls (function calls the model wants to make)
            if let Some(ref tc_str) = tool_calls {
                if let Ok(tc_arr) = serde_json::from_str::<Vec<serde_json::Value>>(tc_str) {
                    for tc in &tc_arr {
                        let fn_name = tc.get("function")
                            .and_then(|f| f.get("name"))
                            .and_then(|n| n.as_str())
                            .unwrap_or("unknown");
                        let fn_args = tc.get("function")
                            .and_then(|f| f.get("arguments"))
                            .and_then(|a| a.as_str())
                            .unwrap_or("");
                        let args_truncated = if fn_args.len() > 200 {
                            format!("{}…", &fn_args[..200])
                        } else {
                            fn_args.to_string()
                        };
                        let text = format!("🔧 {}\n```\n{}\n```", fn_name, args_truncated);
                        messages.push(ChatMessage { role: "assistant".to_string(), text, timestamp: timestamp.clone() });
                    }
                }
            }
            // Also include text content if present
            if let Some(ref text) = content {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    messages.push(ChatMessage { role: "assistant".to_string(), text: trimmed.to_string(), timestamp: timestamp.clone() });
                }
            }
            continue;
        }

        if role == "user" || role == "human" {
            let text = content.unwrap_or_default();
            let trimmed = text.trim();
            if trimmed.is_empty() { continue; }
            messages.push(ChatMessage { role: "user".to_string(), text: trimmed.to_string(), timestamp });
            continue;
        }

        // system or other roles: skip
    }

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
# The hook runs inside the CC terminal, so the focused tab is the right one
# — but only when CC was actually launched from a terminal. Skip when CC is
# spawned by Claude Desktop (its CLI lives under `Claude-3p` in Application
# Support and its parent chain runs through `/Applications/Claude.app/`); in
# that case the AppleScript would happily return whatever Ghostty tab is
# currently focused, and the resulting terminal_id pollutes the session so
# `jump_to_claude_terminal` would surface Ghostty instead of Claude.app.
_PARENT_EXE=$(ps -p $PPID -o comm= 2>/dev/null)
case "$_PARENT_EXE" in
    *Claude-3p*|*/Applications/Claude.app/*)
        GHOSTTY_TID=""
        ;;
    *)
        _TID_CACHE="/tmp/ooclaw-tid-$PPID"
        if [ -f "$_TID_CACHE" ]; then
            export GHOSTTY_TID=$(cat "$_TID_CACHE" 2>/dev/null)
        else
            export GHOSTTY_TID=$(osascript -e 'try
tell application "Ghostty" to return id of first terminal of selected tab of front window as text
end try' 2>/dev/null || echo "")
            [ -n "$GHOSTTY_TID" ] && echo "$GHOSTTY_TID" > "$_TID_CACHE" 2>/dev/null
        fi
        ;;
esac

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
    # Walk the parent chain to detect CC Desktop and capture the CC CLI PID.
    # The hook is invoked as: powershell <- bash <- claude.exe (CLI) <- [optional] claude.exe (Electron desktop in WindowsApps).
    # CC Desktop bundles its CLI under a `claude-3p\claude-code` directory,
    # and the desktop Electron app lives in `WindowsApps\...\claude.exe`.
    # We treat either marker as 'desktop'. The first claude.exe encountered
    # going upward is the CC CLI process; its PID lets oc-claw run PID-alive
    # checks (and a deeper parent walk via Win32) to clear stale sessions.
    $ccDesktop = $false
    $ccPid = 0
    try {
        $current = $PID
        for ($i = 0; $i -lt 10; $i++) {
            $proc = Get-CimInstance Win32_Process -Filter "ProcessId=$current"
            if (-not $proc) { break }
            $parentId = $proc.ParentProcessId
            if (-not $parentId -or $parentId -eq 0 -or $parentId -eq $current) { break }
            $parent = Get-CimInstance Win32_Process -Filter "ProcessId=$parentId"
            if (-not $parent) { break }
            $exe = ''
            if ($parent.ExecutablePath) { $exe = $parent.ExecutablePath.ToLower() }
            if ($exe) {
                if ($exe.Contains('claude-3p')) { $ccDesktop = $true }
                if ($exe.Contains('windowsapps') -and $exe.EndsWith('\claude.exe')) { $ccDesktop = $true }
                if ($ccPid -eq 0 -and $exe.EndsWith('\claude.exe')) { $ccPid = [int]$parent.ProcessId }
            }
            $current = $parentId
        }
    } catch {}
    if ($raw.StartsWith('{')) {
        $prefix = '{'
        if ($ccPid -ne 0) { $prefix += '"pid":' + $ccPid + ',' }
        if ($ccDesktop) { $prefix += '"host":"claude_desktop",' }
        if ($prefix.Length -gt 1) { $raw = $prefix + $raw.Substring(1) }
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

    // Codex support is dropped on Windows. Same as the cursor branch above:
    // proactively delete any previously-installed hook script and strip our
    // entries from hooks.json so the codex CLI cannot reach the oc-claw
    // socket on this machine anymore.
    #[cfg(windows)]
    {
        let _ = std::fs::remove_file(hooks_dir.join("ooclaw-codex-hook.ps1"));
        // Codex's home is conventionally `.codex` on Windows but the install
        // path used `.Codex` historically — the file system is case-
        // insensitive so we clean the same dir, but also catch the
        // lowercase variant explicitly in case both ever exist.
        let alt = home.join(".codex").join("hooks").join("ooclaw-codex-hook.ps1");
        if alt.exists() {
            let _ = std::fs::remove_file(&alt);
        }
        for hooks_json_path in [codex_dir.join("hooks.json"), home.join(".codex").join("hooks.json")] {
            if !hooks_json_path.exists() { continue; }
            let Ok(content) = std::fs::read_to_string(&hooks_json_path) else { continue; };
            let Ok(mut config) = serde_json::from_str::<serde_json::Value>(&content) else { continue; };
            if let Some(hooks) = config.get_mut("hooks").and_then(|v| v.as_object_mut()) {
                let event_names: Vec<String> = hooks.keys().cloned().collect();
                for name in event_names {
                    if let Some(arr) = hooks.get_mut(&name).and_then(|v| v.as_array_mut()) {
                        arr.retain(|entry| {
                            let cmd_match = entry.get("command").and_then(|c| c.as_str())
                                .map(|c| c.contains("ooclaw-codex-hook"))
                                .unwrap_or(false);
                            let nested_match = entry.get("hooks").and_then(|hs| hs.as_array())
                                .map(|hs| hs.iter().any(|inner| {
                                    inner.get("command").and_then(|c| c.as_str())
                                        .map(|c| c.contains("ooclaw-codex-hook"))
                                        .unwrap_or(false)
                                }))
                                .unwrap_or(false);
                            !(cmd_match || nested_match)
                        });
                        if arr.is_empty() {
                            hooks.remove(&name);
                        }
                    }
                }
            }
            if let Ok(json_str) = serde_json::to_string_pretty(&config) {
                let _ = std::fs::write(&hooks_json_path, json_str);
            }
        }
        log::info!("[codex_hooks] codex support disabled on windows; cleaned previously installed hooks");
        return Ok(());
    }

    #[cfg(not(windows))]
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
        $ccPid = $null; try { $cur = $PID; for ($i = 0; $i -lt 8; $i++) { $p = Get-CimInstance Win32_Process -Filter "ProcessId=$cur"; if (-not $p) { break }; $cur = $p.ParentProcessId; if ($cur -le 0) { break }; $pp = Get-CimInstance Win32_Process -Filter "ProcessId=$cur"; if ($pp -and $pp.Name -eq 'claude.exe' -and $pp.ExecutablePath -and $pp.ExecutablePath.ToLower().Contains('windowsapps')) { $ccPid = $cur; break } } } catch {}
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

    // Hard guard: this helper exists only for Codex events. CC's
    // PreToolUse payload may carry overlapping field names (e.g. a future
    // CC release adding a `justification` field), and previous iterations
    // of the looser checks below already mis-classified CC's Bash calls
    // as needing approval. Bail out immediately for anything that isn't
    // unambiguously a Codex event so the function name and behaviour
    // stay aligned, no matter what gets added inside it later.
    let is_codex_event = event.get("turn_id").is_some()
        || read_string(event, &["source"]).unwrap_or("").eq_ignore_ascii_case("codex");
    if !is_codex_event {
        return false;
    }

    // Only trust explicit approval/escalation fields that Codex itself sets
    // on the event or inside tool_input. Anything beyond that is a guess.
    //
    // The previous fallback inspected the bash command string and flagged
    // anything containing `/Users/`, `$HOME/`, `Desktop/` or a redirect
    // operator as needing approval. That heuristic was meant to catch
    // out-of-workspace writes in `default` permission mode, but on macOS
    // virtually every read command (`sed -n '/Users/...'`, `ls /Users/...`,
    // `cat /Users/...`) tripped it. Skills like hatch-pet that live under
    // `~/.codex/skills/` would fire `is_wait_event` on every Bash tool call
    // and play the waiting sound dozens of times per task.
    //
    // Codex already owns the real permission flow: when approval is
    // actually required it fires a separate `PermissionRequest` hook event,
    // which `is_wait_event` picks up via its `hook_event == "PermissionRequest"`
    // branch. We don't need to second-guess based on command shape.
    if has_explicit_escalation_markers(event) {
        return true;
    }
    if let Some(tool_input) = parse_tool_input(event) {
        if has_explicit_escalation_markers(&tool_input) {
            return true;
        }
    }
    false
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
    // Char-boundary-safe truncation for the diagnostic log. Plain byte slicing
    // (`&buf[..500]`) panics if byte 500 lands inside a multi-byte UTF-8 char,
    // which is guaranteed for any CC Desktop Stop event whose `last_assistant_message`
    // contains CJK characters (e.g. AI reply "我为你创建了 xxx.html"). The panic
    // killed the per-connection handler thread before status could transition
    // to "stopped", leaving the session stuck in `processing` for 120s until
    // the stale-detection fallback fired.
    let preview_end = buf
        .char_indices()
        .map(|(i, c)| i + c.len_utf8())
        .take_while(|&end| end <= 500)
        .last()
        .unwrap_or(0);
    log::info!("[claude_event] raw buf len={} content={}", buf.len(), &buf[..preview_end]);
    // Defensive: strip a leading UTF-8 BOM (U+FEFF) plus any whitespace.
    // Cursor on Windows emits hook stdin with a BOM and the hook script may
    // forward it raw; serde_json refuses BOM with "expected value at column 1".
    let buf_trimmed = buf.trim_start_matches('\u{feff}').trim_start();

    // Cursor on CJK Windows emits hook payloads where CJK characters have been
    // mojibake'd through a GBK→UTF-8 round-trip upstream. Reverse it before
    // parsing so the prompt/text fields contain the original Chinese text.
    // Only applies to cursor source on Windows; CC/codex paths are untouched.
    #[cfg(target_os = "windows")]
    let buf_owned: String = if source_override == Some("cursor") {
        try_recover_cursor_mojibake(buf_trimmed).unwrap_or_else(|| buf_trimmed.to_string())
    } else {
        buf_trimmed.to_string()
    };
    #[cfg(target_os = "windows")]
    let buf_for_parse: &str = &buf_owned;
    #[cfg(not(target_os = "windows"))]
    let buf_for_parse: &str = buf_trimmed;

    if let Ok(event) = serde_json::from_str::<serde_json::Value>(buf_for_parse) {
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
        // Normalize event names from various agents to CC's PascalCase.
        // Cursor uses camelCase, Gemini CLI uses PascalCase (BeforeAgent/AfterAgent etc.),
        // CC uses PascalCase (UserPromptSubmit, Stop, etc.).
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
            // Gemini CLI events → map to CC equivalents
            "BeforeAgent" => "UserPromptSubmit".to_string(),
            "AfterAgent" => "Stop".to_string(),
            "BeforeTool" => "PreToolUse".to_string(),
            "AfterTool" => "PostToolUse".to_string(),
            "Notification" => "Stop".to_string(),
            "PreCompress" => "PreCompact".to_string(),
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
            "SessionStart" => "stopped".to_string(),
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
        let session_host_terminal: Option<String>;
        let stop_was_interrupted;

        {
            let mut sessions = state.lock().unwrap();
            let prev_status = sessions.get(&session_id).map(|s| s.status.clone()).unwrap_or_default();
            was_processing = matches!(prev_status.as_str(), "processing" | "tool_running" | "compacting");
            was_compacting = prev_status == "compacting";

            if hook_event == "SessionEnd" {
                let prev = sessions.get(&session_id);
                session_source = prev.map(|s| s.source.clone()).unwrap_or_else(|| "cc".to_string());
                session_host_terminal = prev.and_then(|s| s.host_terminal.clone());
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
                    platform: None,
                    cursor_port: None,
                    cursor_workspace_root: None,
                    cursor_workspace_name: None,
                    cursor_native_handle: None,
                });
                // Only upgrade source, never downgrade:
                // cc < codex < gemini < cursor.
                // Once a session is identified as a higher-rank source, later
                // generic CC events (source=cc) for the same sessionId must not
                // overwrite it, otherwise active-tab/staleness logic regresses.
                let source_rank = |s: &str| -> u8 {
                    match s {
                        "cc" => 1,
                        "codex" => 2,
                        "gemini" => 3,
                        "hermes" => 4,
                        "cursor" => 5,
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
                let mut incoming_cwd = event
                    .get("cwd")
                    .or_else(|| event.get("workdir"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                // Cursor's hook payload omits `cwd` entirely on Windows; it
                // exposes the workspace as URI-style `/g:/Desktop/code` under
                // `workspace_roots`. Derive cwd from there so the session list
                // (which filters out empty cwd) and workspace binding both work.
                if incoming_cwd.is_empty() && session.source == "cursor" {
                    if let Some(roots) = event.get("workspace_roots").and_then(|v| v.as_array()) {
                        if let Some(first) = roots.first().and_then(|v| v.as_str()) {
                            incoming_cwd = normalize_cursor_path(first);
                        }
                    }
                }
                if !incoming_cwd.is_empty() || session.cwd.is_empty() {
                    session.cwd = incoming_cwd;
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
                    #[cfg(target_os = "windows")]
                    if session.host_terminal.is_none() && session.source != "cursor" {
                        session.host_terminal = find_host_app_for_pid_win(pid_u32);
                        log::info!("[claude_event] session={} host_terminal={:?}",
                            &session_id[..session_id.len().min(8)], session.host_terminal);
                    }
                }

                // Store Hermes platform from hook event (e.g. "feishu", "slack")
                if let Some(p) = event.get("platform").and_then(|v| v.as_str()) {
                    if !p.is_empty() {
                        session.platform = Some(p.to_string());
                    }
                }

                // Read "host" field injected by the hook script (e.g. "claude_desktop")
                if session.host_terminal.is_none() {
                    if let Some(host) = event.get("host").and_then(|v| v.as_str()) {
                        let ht = match host {
                            "claude_desktop" => "Claude Desktop",
                            _ => host,
                        };
                        session.host_terminal = Some(ht.to_string());
                        log::info!("[claude_event] session={} host_terminal={:?} (from hook host field)",
                            &session_id[..session_id.len().min(8)], session.host_terminal);
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
                    if is_tab_active || interrupted || session.source == "hermes" {
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
                session_host_terminal = session.host_terminal.clone();
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
            if cfg!(debug_assertions) {
                log::info!(
                    "[claude_event] emit claude-task-complete session={} waiting={} source={} host={:?}",
                    &session_id[..session_id.len().min(8)],
                    is_waiting,
                    session_source,
                    session_host_terminal,
                );
            }
            let _ = app.emit("claude-task-complete", serde_json::json!({
                "sessionId": session_id,
                "waiting": is_waiting,
                "source": session_source,
                "hostTerminal": session_host_terminal,
            }));
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
    } else if let Err(e) = serde_json::from_str::<serde_json::Value>(buf_for_parse) {
        let tail: String = buf_for_parse.chars().rev().take(300).collect::<String>().chars().rev().collect();
        log::warn!("[claude_event] JSON parse failed: err={}, len={}, tail=...{}", e, buf_for_parse.len(), tail);
    }
    None
}

// ─── Gemini CLI Integration ────────────────────────────────────────────

/// Install hooks for Gemini CLI.
/// Creates ~/.gemini/hooks/ooclaw-gemini-hook.sh and registers it in
/// ~/.gemini/settings.json for all Gemini hook events.
/// Gemini CLI uses PascalCase event names (SessionStart, BeforeAgent, etc.)
/// and expects JSON stdout for gating hooks (BeforeTool/AfterTool → {"decision":"allow"}).
#[tauri::command]
async fn install_gemini_hooks() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let gemini_dir = home.join(".gemini");

    // Skip if ~/.gemini/ doesn't exist (Gemini CLI not installed)
    if !gemini_dir.exists() {
        log::info!("[gemini_hooks] ~/.gemini/ not found — skipping");
        return Ok(());
    }

    let hooks_dir = gemini_dir.join("hooks");
    std::fs::create_dir_all(&hooks_dir).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    let hook_path = hooks_dir.join("ooclaw-gemini-hook.sh");
    #[cfg(windows)]
    let hook_path = hooks_dir.join("ooclaw-gemini-hook.ps1");

    #[cfg(unix)]
    {
        // Gemini CLI hook script: reads stdin JSON, maps event to oc-claw state,
        // forwards to Unix socket, returns appropriate stdout for gating hooks.
        let hook_script = r#"#!/bin/bash
# ooclaw Gemini CLI hook - forwards events to /tmp/ooclaw-gemini.sock
SOCKET_PATH="/tmp/ooclaw-gemini.sock"
[ -S "$SOCKET_PATH" ] || { echo '{}'; exit 0; }

# Capture Gemini CLI PID for terminal jump support
export GEMINI_PID=$PPID

# Cache Ghostty terminal ID per Gemini PID (so jump-to-terminal works)
_TID_CACHE="/tmp/ooclaw-gemini-tid-$PPID"
if [ -f "$_TID_CACHE" ]; then
    export GHOSTTY_TID=$(cat "$_TID_CACHE" 2>/dev/null)
else
    _TID=$(osascript -e 'tell application "Ghostty" to return id of current terminal of front window as text' 2>/dev/null || true)
    if [ -n "$_TID" ]; then
        echo "$_TID" > "$_TID_CACHE"
        export GHOSTTY_TID="$_TID"
    fi
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

hook_event = data.get('hook_event_name', '')
mapped_event = hook_event

# Notification with notification_type=ToolPermission means Gemini is
# showing an approval prompt (e.g. 'Apply this change?').  Map it to
# PermissionRequest so oc-claw shows the 'waiting' pet state.
# Non-permission notifications are informational only; skip them to
# avoid state flicker.
if hook_event == 'Notification':
    if data.get('notification_type') == 'ToolPermission':
        mapped_event = 'PermissionRequest'
    else:
        print('{}')
        sys.exit(0)

status_map = {
    'SessionStart': 'waiting_for_input',
    'SessionEnd': 'ended',
    'BeforeAgent': 'processing',
    'AfterAgent': 'waiting_for_input',
    'BeforeTool': 'running_tool',
    'AfterTool': 'processing',
    'PermissionRequest': 'waiting',
    'PreCompress': 'compacting',
}

details = data.get('details', {}) or {}
tool_name = details.get('tool_name', '') or data.get('tool_name', '')

gemini_pid = int(os.environ.get('GEMINI_PID', '0'))
ghostty_tid = os.environ.get('GHOSTTY_TID', '')

output = {
    'sessionId': data.get('session_id', ''),
    'cwd': data.get('cwd', ''),
    'event': mapped_event,
    'claudeStatus': status_map.get(mapped_event, 'unknown'),
    'source': 'gemini',
}
if tool_name:
    output['tool'] = tool_name
if gemini_pid:
    output['pid'] = gemini_pid
if ghostty_tid:
    output['terminalId'] = ghostty_tid

payload = json.dumps(output)

gating_response = '{}'
if hook_event in ('BeforeTool', 'AfterTool'):
    gating_response = json.dumps({'decision': 'allow'})

try:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect('$SOCKET_PATH')
    sock.sendall(payload.encode('utf-8'))
    sock.shutdown(socket.SHUT_WR)
    sock.close()
except:
    pass

sys.stdout.write(gating_response)
"
"#;
        std::fs::write(&hook_path, hook_script).map_err(|e| e.to_string())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }

    #[cfg(windows)]
    {
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

    $hookEvent = ''
    if ($obj -ne $null -and $obj.hook_event_name) { $hookEvent = [string]$obj.hook_event_name }

    $mappedEvent = $hookEvent
    if ($hookEvent -eq 'Notification') {
        if ($obj.notification_type -eq 'ToolPermission') {
            $mappedEvent = 'PermissionRequest'
        } else {
            [Console]::Out.Write('{}')
            [Console]::Out.Flush()
            exit 0
        }
    }

    $statusMap = @{
        'SessionStart' = 'waiting_for_input'
        'SessionEnd' = 'ended'
        'BeforeAgent' = 'processing'
        'AfterAgent' = 'waiting_for_input'
        'BeforeTool' = 'running_tool'
        'AfterTool' = 'processing'
        'PermissionRequest' = 'waiting'
        'PreCompress' = 'compacting'
    }

    $toolName = ''
    if ($obj.details -and $obj.details.tool_name) { $toolName = [string]$obj.details.tool_name }
    if (-not $toolName -and $obj.tool_name) { $toolName = [string]$obj.tool_name }

    $output = @{
        sessionId = if ($obj.session_id) { $obj.session_id } else { '' }
        cwd = if ($obj.cwd) { $obj.cwd } else { '' }
        event = $mappedEvent
        claudeStatus = if ($statusMap.ContainsKey($mappedEvent)) { $statusMap[$mappedEvent] } else { 'unknown' }
        source = 'gemini'
    }
    if ($toolName) { $output['tool'] = $toolName }

    $payload = $output | ConvertTo-Json -Compress -Depth 10

    $gatingResponse = '{}'
    if ($hookEvent -eq 'BeforeTool' -or $hookEvent -eq 'AfterTool') {
        $gatingResponse = '{"decision":"allow"}'
    }

    $client = [System.Net.Sockets.TcpClient]::new('127.0.0.1', 19285)
    $stream = $client.GetStream()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($payload)
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush()
    $client.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send)
    $client.Close()

    [Console]::Out.Write($gatingResponse)
    [Console]::Out.Flush()
} catch {
    try { [Console]::Out.Write('{}'); [Console]::Out.Flush() } catch {}
}
"#;
        std::fs::write(&hook_path, ps1_script).map_err(|e| e.to_string())?;
    }

    // Register hooks in ~/.gemini/settings.json
    // Gemini CLI uses a different config format:
    //   { "hooks": { "EventName": [{ "matcher": "*", "hooks": [{ "name": "ooclaw", "type": "command", "command": "..." }] }] } }
    let settings_path = gemini_dir.join("settings.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    if settings.get("hooks").is_none() {
        settings["hooks"] = serde_json::json!({});
    }
    let hooks = settings["hooks"].as_object_mut().ok_or("hooks is not an object")?;

    #[cfg(windows)]
    let hook_command = format!(
        "powershell.exe -NoProfile -ExecutionPolicy Bypass -File '{}'",
        hook_path.to_string_lossy().replace('\\', "/"),
    );
    #[cfg(not(windows))]
    let hook_command = hook_path.to_string_lossy().to_string();

    let is_our_hook = |cmd: &str| -> bool {
        cmd == hook_command || cmd.contains("ooclaw-gemini-hook")
    };

    let has_our_entry = |entry: &serde_json::Value| -> bool {
        if let Some(cmd) = entry.get("command").and_then(|c| c.as_str()) {
            if is_our_hook(cmd) { return true; }
        }
        if let Some(inner_hooks) = entry.get("hooks").and_then(|h| h.as_array()) {
            return inner_hooks.iter().any(|h| {
                h.get("command").and_then(|c| c.as_str()).map_or(false, |c| is_our_hook(c))
            });
        }
        false
    };

    let gemini_events = [
        "SessionStart", "SessionEnd", "BeforeAgent", "AfterAgent",
        "BeforeTool", "AfterTool", "Notification", "PreCompress",
    ];

    let hook_entry = serde_json::json!({
        "matcher": "*",
        "hooks": [{ "name": "ooclaw", "type": "command", "command": hook_command }]
    });

    for event_name in gemini_events {
        let arr = hooks.entry(event_name.to_string()).or_insert(serde_json::json!([]));
        let list = arr.as_array_mut().ok_or("hook event is not an array")?;
        list.retain(|entry| !has_our_entry(entry));
        list.push(hook_entry.clone());
    }

    // Gemini CLI requires enableHooks to be true for hooks to fire
    if settings.get("tools").is_none() {
        settings["tools"] = serde_json::json!({});
    }
    settings["tools"]["enableHooks"] = serde_json::json!(true);

    let json_str = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    std::fs::write(&settings_path, json_str).map_err(|e| e.to_string())?;
    log::info!("[gemini_hooks] installed hooks to {:?}", settings_path);

    Ok(())
}

// ─── Hermes Agent Integration ──────────────────────────────────────────

/// Install a Python plugin for Hermes Agent.
/// Creates ~/.hermes/plugins/ooclaw/{plugin.yaml,__init__.py} that
/// forwards lifecycle events to the oc-claw Unix socket.
#[tauri::command]
async fn install_hermes_hooks() -> Result<(), String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let hermes_dir = home.join(".hermes");

    if !hermes_dir.exists() {
        log::info!("[hermes_hooks] ~/.hermes/ not found — skipping");
        return Ok(());
    }

    let plugin_dir = hermes_dir.join("plugins").join("ooclaw");
    std::fs::create_dir_all(&plugin_dir).map_err(|e| e.to_string())?;

    let plugin_yaml = r#"name: ooclaw
version: 0.1.0
description: "Forward Hermes Agent events to oc-claw desktop pet."
author: "oc-claw"
hooks:
  - on_session_start
  - pre_llm_call
  - post_llm_call
  - pre_tool_call
  - post_tool_call
  - on_session_end
  - on_session_finalize
  - on_session_reset
  - pre_approval_request
  - post_approval_response
"#;
    std::fs::write(plugin_dir.join("plugin.yaml"), plugin_yaml).map_err(|e| e.to_string())?;

    // The Python plugin is written as a static string with a placeholder for
    // the connection target (Unix socket path or TCP port).
    #[cfg(unix)]
    let connect_code = r#"SOCKET_PATH = "/tmp/ooclaw-hermes.sock"

def _send(payload):
    import os as _os
    if not _os.path.exists(SOCKET_PATH):
        return
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(0.5)
        s.connect(SOCKET_PATH)
        s.sendall(json.dumps(payload).encode("utf-8"))
        s.shutdown(socket.SHUT_WR)
        s.close()
    except Exception:
        pass"#;

    #[cfg(windows)]
    let connect_code = r#"TCP_PORT = 19286

def _send(payload):
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(0.5)
        s.connect(("127.0.0.1", TCP_PORT))
        s.sendall(json.dumps(payload).encode("utf-8"))
        s.shutdown(socket.SHUT_WR)
        s.close()
    except Exception:
        pass"#;

    let init_py = format!(
        r##"# ooclaw plugin for Hermes Agent - forwards events to oc-claw.
from __future__ import annotations
import json, os, socket
from typing import Any, Dict, Tuple

{connect_code}

HOOK_TO_EVENT: Dict[str, Tuple[str, str]] = {{
    "on_session_start": ("waiting_for_input", "SessionStart"),
    "pre_llm_call": ("processing", "UserPromptSubmit"),
    "post_llm_call": ("waiting_for_input", "Stop"),
    "pre_tool_call": ("running_tool", "PreToolUse"),
    "post_tool_call": ("processing", "PostToolUse"),
    "on_session_end": ("waiting_for_input", "Stop"),
    "on_session_finalize": ("ended", "SessionEnd"),
    "on_session_reset": ("waiting_for_input", "SessionStart"),
    "pre_approval_request": ("waiting", "PermissionRequest"),
    "post_approval_response": ("processing", "PostToolUse"),
}}

def _handle(event_name, **kwargs):
    mapping = HOOK_TO_EVENT.get(event_name)
    if not mapping:
        return
    status, cc_event = mapping
    if event_name == "post_tool_call":
        result = kwargs.get("result")
        if isinstance(result, dict) and (result.get("error") or
            (isinstance(result.get("exit_code"), int) and result["exit_code"] != 0)):
            status, cc_event = "processing", "PostToolUse"
    session_id = kwargs.get("session_id", "") or kwargs.get("conversation_id", "")
    if not session_id:
        return
    tool_name = ""
    if event_name in ("pre_tool_call", "post_tool_call", "pre_approval_request"):
        tool_name = kwargs.get("tool_name", "") or kwargs.get("tool", "") or ""
    platform = kwargs.get("platform", "") or ""
    try:
        cwd = os.getcwd()
    except Exception:
        cwd = os.path.expanduser("~/.hermes")
    if not cwd:
        cwd = os.path.expanduser("~/.hermes")
    payload = {{
        "sessionId": session_id,
        "cwd": cwd,
        "event": cc_event,
        "claudeStatus": status,
        "source": "hermes",
        "pid": os.getpid(),
        "platform": platform,
    }}
    if tool_name:
        payload["tool"] = tool_name
    if event_name == "pre_llm_call":
        prompt = kwargs.get("prompt", "") or kwargs.get("message", "")
        if prompt:
            payload["userPrompt"] = prompt[:500]
    elif event_name == "post_llm_call":
        resp = kwargs.get("response", "") or kwargs.get("result", "")
        if isinstance(resp, dict):
            resp = resp.get("content", "") or resp.get("text", "")
        if resp and isinstance(resp, str):
            payload["lastResponse"] = resp[:2000]
    _send(payload)

def _make_cb(event_name):
    def cb(**kwargs):
        try:
            _handle(event_name, **kwargs)
        except Exception:
            pass
        return None
    cb.__name__ = "ooclaw_" + event_name
    return cb

def register(ctx):
    for hook_name in HOOK_TO_EVENT:
        ctx.register_hook(hook_name, _make_cb(hook_name))
"##, connect_code = connect_code);

    std::fs::write(plugin_dir.join("__init__.py"), init_py).map_err(|e| e.to_string())?;
    log::info!("[hermes_hooks] installed plugin to {:?}", plugin_dir);

    // Hermes plugins are opt-in. Run `hermes plugins enable ooclaw` to
    // register the plugin so it loads on the next session.  If the CLI
    // is not on PATH we fall back to patching config.yaml directly.
    let hermes_bin = which_hermes();
    if let Some(bin) = hermes_bin {
        let out = std::process::Command::new(&bin)
            .args(["plugins", "enable", "ooclaw"])
            .output();
        match out {
            Ok(o) => log::info!("[hermes_hooks] plugins enable: {}", String::from_utf8_lossy(&o.stdout)),
            Err(e) => log::warn!("[hermes_hooks] plugins enable failed: {}", e),
        }
    } else {
        // Fallback: patch config.yaml plugins.enabled list directly
        let config_path = hermes_dir.join("config.yaml");
        if config_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&config_path) {
                if !content.contains("ooclaw") {
                    let patched = if content.contains("plugins:") {
                        content.replace(
                            "plugins:\n  enabled: []",
                            "plugins:\n  enabled:\n  - ooclaw",
                        ).replace(
                            "plugins:\n  enabled:",
                            "plugins:\n  enabled:\n  - ooclaw",
                        )
                    } else {
                        format!("{}\nplugins:\n  enabled:\n  - ooclaw\n  disabled: []\n", content.trim_end())
                    };
                    let _ = std::fs::write(&config_path, patched);
                    log::info!("[hermes_hooks] patched config.yaml to enable ooclaw plugin");
                }
            }
        }
    }

    // ── Gateway hook (for Feishu / Telegram / Slack / etc.) ──
    // Gateway uses a separate hook system under ~/.hermes/hooks/<name>/
    let hooks_dir = hermes_dir.join("hooks").join("ooclaw");
    std::fs::create_dir_all(&hooks_dir).map_err(|e| e.to_string())?;

    let hook_yaml = r#"name: ooclaw
description: Forward Hermes gateway events to oc-claw desktop pet
events:
  - session:start
  - agent:start
  - agent:end
  - session:end
  - session:reset
"#;
    std::fs::write(hooks_dir.join("HOOK.yaml"), hook_yaml).map_err(|e| e.to_string())?;

    #[cfg(unix)]
    let gw_connect_code = r#"SOCKET_PATH = "/tmp/ooclaw-hermes.sock"

def _send_to_ooclaw(payload):
    raw = json.dumps(payload) + "\n"
    if not os.path.exists(SOCKET_PATH):
        _log("socket not found: " + SOCKET_PATH)
        return
    try:
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(SOCKET_PATH)
        s.sendall(raw.encode("utf-8"))
        s.close()
        _log(f"sent OK: {payload.get('event')} status={payload.get('claudeStatus')}")
    except Exception as e:
        _log(f"send FAILED: {e}")"#;

    #[cfg(windows)]
    let gw_connect_code = r#"TCP_PORT = 19286

def _send_to_ooclaw(payload):
    raw = json.dumps(payload) + "\n"
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(2)
        s.connect(("127.0.0.1", TCP_PORT))
        s.sendall(raw.encode("utf-8"))
        s.close()
        _log(f"sent OK: {payload.get('event')} status={payload.get('claudeStatus')}")
    except Exception as e:
        _log(f"send FAILED: {e}")"#;

    let handler_py = format!(
        r##""""
Gateway hook handler for oc-claw integration.
Forwards Hermes gateway lifecycle events to oc-claw via socket.
"""
import json, os, socket, sys, datetime

LOG_DIR = os.path.expanduser("~/.hermes/logs")
LOG_FILE = os.path.join(LOG_DIR, "ooclaw-hook.log")

def _log(msg):
    os.makedirs(LOG_DIR, exist_ok=True)
    ts = datetime.datetime.now().isoformat()
    with open(LOG_FILE, "a") as f:
        f.write(f"[{{ts}}] {{msg}}\n")

{gw_connect_code}

EVENT_MAP = {{
    "session:start": ("SessionStart", "waiting_for_input"),
    "agent:start":   ("UserPromptSubmit", "processing"),
    "agent:end":     ("Stop", "waiting_for_input"),
    "session:end":   ("Stop", "waiting_for_input"),
    "session:reset": ("SessionStart", "waiting_for_input"),
}}

def handle(event_type, context):
    _log(f"hook called: {{event_type}} context_keys={{list(context.keys())}}")
    mapping = EVENT_MAP.get(event_type)
    if not mapping:
        _log(f"unmapped event: {{event_type}}")
        return
    oc_event, claude_status = mapping
    platform = context.get("platform", "")
    user_id = context.get("user_id", "")
    session_id = context.get("session_id", "")
    if not session_id:
        session_id = f"gw_{{platform}}_{{user_id}}" if user_id else f"gw_{{platform}}"
    payload = {{
        "sessionId": session_id,
        "cwd": os.path.expanduser("~/.hermes"),
        "event": oc_event,
        "claudeStatus": claude_status,
        "source": "hermes",
        "pid": os.getpid(),
        "platform": platform or "gateway",
    }}
    if event_type == "agent:start":
        msg = context.get("message", "")
        if msg:
            payload["userPrompt"] = msg[:500]
    elif event_type == "agent:end":
        resp = context.get("response", "")
        if resp:
            payload["lastResponse"] = resp[:2000]
    _log(f"payload: {{json.dumps(payload)}}")
    _send_to_ooclaw(payload)
"##, gw_connect_code = gw_connect_code);

    std::fs::write(hooks_dir.join("handler.py"), handler_py).map_err(|e| e.to_string())?;
    log::info!("[hermes_hooks] installed gateway hook to {:?}", hooks_dir);

    Ok(())
}

/// Try to find the hermes binary on PATH or known locations.
fn which_hermes() -> Option<std::path::PathBuf> {
    // Check PATH first
    if let Ok(output) = std::process::Command::new("which").arg("hermes").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                return Some(std::path::PathBuf::from(path));
            }
        }
    }
    // Known locations
    if let Some(home) = dirs::home_dir() {
        let candidates = [
            home.join(".local/bin/hermes"),
            home.join(".hermes/hermes-agent/venv/bin/hermes"),
        ];
        for c in &candidates {
            if c.exists() {
                return Some(c.clone());
            }
        }
    }
    None
}

/// Diagnose the Hermes integration by checking plugin installation,
/// enabled state, and sending a test event through the socket.
/// Returns a structured JSON result so the frontend can display status.
#[tauri::command]
async fn test_hermes_hook() -> Result<serde_json::Value, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let hermes_dir = home.join(".hermes");
    let mut checks: Vec<serde_json::Value> = Vec::new();

    // 1. Check if ~/.hermes exists
    if !hermes_dir.exists() {
        checks.push(serde_json::json!({"step": "hermes_dir", "ok": false, "msg": "~/.hermes not found — Hermes Agent is not installed"}));
        return Ok(serde_json::json!({"ok": false, "checks": checks}));
    }
    checks.push(serde_json::json!({"step": "hermes_dir", "ok": true, "msg": "~/.hermes exists"}));

    // 2. Check plugin files
    let plugin_dir = hermes_dir.join("plugins").join("ooclaw");
    let has_yaml = plugin_dir.join("plugin.yaml").exists();
    let has_py = plugin_dir.join("__init__.py").exists();
    if has_yaml && has_py {
        checks.push(serde_json::json!({"step": "plugin_files", "ok": true, "msg": "Plugin files installed"}));
    } else {
        checks.push(serde_json::json!({"step": "plugin_files", "ok": false, "msg": format!("Missing: {}", if !has_yaml && !has_py { "plugin.yaml + __init__.py" } else if !has_yaml { "plugin.yaml" } else { "__init__.py" })}));
    }

    // 3. Check plugin enabled in config.yaml
    let config_path = hermes_dir.join("config.yaml");
    let enabled = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        content.contains("ooclaw")
    } else {
        false
    };
    if enabled {
        checks.push(serde_json::json!({"step": "plugin_enabled", "ok": true, "msg": "Plugin enabled in config.yaml"}));
    } else {
        checks.push(serde_json::json!({"step": "plugin_enabled", "ok": false, "msg": "Plugin not enabled — run: hermes plugins enable ooclaw"}));
    }

    // 4. Check socket exists
    #[cfg(unix)]
    let socket_ok = std::path::Path::new("/tmp/ooclaw-hermes.sock").exists();
    #[cfg(windows)]
    let socket_ok = std::net::TcpStream::connect("127.0.0.1:19286").is_ok();

    if socket_ok {
        checks.push(serde_json::json!({"step": "socket", "ok": true, "msg": "Socket server running"}));
    } else {
        checks.push(serde_json::json!({"step": "socket", "ok": false, "msg": "Socket not found — oc-claw server may not be running"}));
    }

    // 5. Send a test event through the socket to verify end-to-end
    let test_session_id = format!("hermes-test-{}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis());
    let test_payload = serde_json::json!({
        "sessionId": test_session_id,
        "cwd": hermes_dir.to_string_lossy(),
        "event": "UserPromptSubmit",
        "claudeStatus": "processing",
        "source": "hermes",
        "pid": std::process::id(),
    });

    #[cfg(unix)]
    {
        match std::os::unix::net::UnixStream::connect("/tmp/ooclaw-hermes.sock") {
            Ok(mut stream) => {
                use std::io::Write;
                let data = serde_json::to_string(&test_payload).unwrap();
                let _ = stream.write_all(data.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                checks.push(serde_json::json!({"step": "test_event", "ok": true, "msg": format!("Test event sent (session: {})", &test_session_id[..20])}));

                // Wait a tiny bit, then send stop to clean up
                std::thread::sleep(std::time::Duration::from_millis(500));
                let stop_payload = serde_json::json!({
                    "sessionId": test_session_id,
                    "cwd": hermes_dir.to_string_lossy(),
                    "event": "Stop",
                    "claudeStatus": "waiting_for_input",
                    "source": "hermes",
                    "pid": std::process::id(),
                });
                if let Ok(mut s2) = std::os::unix::net::UnixStream::connect("/tmp/ooclaw-hermes.sock") {
                    let d2 = serde_json::to_string(&stop_payload).unwrap();
                    let _ = s2.write_all(d2.as_bytes());
                    let _ = s2.shutdown(std::net::Shutdown::Write);
                }
            }
            Err(e) => {
                checks.push(serde_json::json!({"step": "test_event", "ok": false, "msg": format!("Socket connect failed: {}", e)}));
            }
        }
    }

    #[cfg(windows)]
    {
        match std::net::TcpStream::connect("127.0.0.1:19286") {
            Ok(mut stream) => {
                use std::io::Write;
                let data = serde_json::to_string(&test_payload).unwrap();
                let _ = stream.write_all(data.as_bytes());
                let _ = stream.shutdown(std::net::Shutdown::Write);
                checks.push(serde_json::json!({"step": "test_event", "ok": true, "msg": format!("Test event sent (session: {})", &test_session_id[..20])}));

                std::thread::sleep(std::time::Duration::from_millis(500));
                let stop_payload = serde_json::json!({
                    "sessionId": test_session_id,
                    "cwd": hermes_dir.to_string_lossy(),
                    "event": "Stop",
                    "claudeStatus": "waiting_for_input",
                    "source": "hermes",
                    "pid": std::process::id(),
                });
                if let Ok(mut s2) = std::net::TcpStream::connect("127.0.0.1:19286") {
                    let d2 = serde_json::to_string(&stop_payload).unwrap();
                    let _ = s2.write_all(d2.as_bytes());
                    let _ = s2.shutdown(std::net::Shutdown::Write);
                }
            }
            Err(e) => {
                checks.push(serde_json::json!({"step": "test_event", "ok": false, "msg": format!("TCP connect failed: {}", e)}));
            }
        }
    }

    let all_ok = checks.iter().all(|c| c["ok"].as_bool().unwrap_or(false));
    Ok(serde_json::json!({"ok": all_ok, "checks": checks}))
}

// ─── Hermes SSH Remote ────────────────────────────────────────────────

/// Read Hermes stats from a remote server's ~/.hermes/state.db via SSH.
/// Uses python3 on the remote to query SQLite (avoids sqlite3 CLI dependency).
#[tauri::command]
async fn get_hermes_remote_stats(ssh_host: String, ssh_user: String) -> Result<ClaudeStats, String> {
    let py_script = r#"
import json, sqlite3, time, os
db = os.path.expanduser('~/.hermes/state.db')
if not os.path.exists(db):
    print('{"error":"no_db"}')
    exit(0)
conn = sqlite3.connect(db)
cutoff = time.time() - 14*86400
cur = conn.execute(
    'SELECT started_at, input_tokens, output_tokens, cache_read_tokens, '
    'cache_write_tokens, message_count, model '
    'FROM sessions WHERE started_at > ? ORDER BY started_at DESC',
    (cutoff,))
rows = [dict(zip(['ts','inp','out','cr','cw','msgs','model'], r)) for r in cur.fetchall()]
conn.close()
print(json.dumps(rows))
"#;
    let cmd = format!("python3 -c {}", shell_escape_single(py_script));
    let output = ssh_exec(&ssh_host, &ssh_user, &cmd).await?;
    let trimmed = output.trim();

    if trimmed.contains("\"error\"") && trimmed.contains("no_db") {
        return Ok(empty_claude_stats());
    }

    let rows: Vec<serde_json::Value> = serde_json::from_str(trimmed)
        .map_err(|e| format!("parse hermes remote stats: {} (output: {})", e, &trimmed[..trimmed.len().min(200)]))?;

    let now = chrono::Local::now();
    let mut daily_map: std::collections::BTreeMap<String, ClaudeDailyStats> = std::collections::BTreeMap::new();
    for i in (0..14).rev() {
        let day = (now - chrono::Duration::days(i)).format("%Y-%m-%d").to_string();
        daily_map.insert(day.clone(), ClaudeDailyStats {
            date: day, input_tokens: 0, output_tokens: 0,
            cache_read_tokens: 0, cache_write_tokens: 0, messages: 0, sessions: 0,
        });
    }

    let mut total_input = 0u64;
    let mut total_output = 0u64;
    let mut total_cache_read = 0u64;
    let mut total_cache_write = 0u64;
    let mut total_messages = 0u64;
    let mut total_sessions = 0u64;
    let mut model = String::new();

    for row in &rows {
        let ts = row["ts"].as_f64().unwrap_or(0.0);
        let inp = row["inp"].as_i64().unwrap_or(0) as u64;
        let out = row["out"].as_i64().unwrap_or(0) as u64;
        let cr = row["cr"].as_i64().unwrap_or(0) as u64;
        let cw = row["cw"].as_i64().unwrap_or(0) as u64;
        let msgs = row["msgs"].as_i64().unwrap_or(0) as u64;
        let mdl = row["model"].as_str().unwrap_or("");

        let day = chrono::DateTime::from_timestamp(ts as i64, 0)
            .map(|dt| dt.with_timezone(&chrono::Local).format("%Y-%m-%d").to_string())
            .unwrap_or_default();

        if let Some(entry) = daily_map.get_mut(&day) {
            entry.input_tokens += inp;
            entry.output_tokens += out;
            entry.cache_read_tokens += cr;
            entry.cache_write_tokens += cw;
            entry.messages += msgs;
            entry.sessions += 1;
        }

        total_input += inp;
        total_output += out;
        total_cache_read += cr;
        total_cache_write += cw;
        total_messages += msgs;
        total_sessions += 1;
        if model.is_empty() && !mdl.is_empty() {
            model = mdl.to_string();
        }
    }

    let daily_stats: Vec<ClaudeDailyStats> = daily_map.into_values().collect();
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

/// Poll active Hermes sessions on a remote server via SSH.
/// Reads ~/.hermes/sessions/sessions.json for gateway session list,
/// and checks gateway.log for recent activity.
#[tauri::command]
async fn get_hermes_remote_sessions(ssh_host: String, ssh_user: String) -> Result<Vec<serde_json::Value>, String> {
    let py_script = r#"
import json, os, time
sj = os.path.expanduser('~/.hermes/sessions/sessions.json')
db = os.path.expanduser('~/.hermes/state.db')
seen = set()
results = []
now = time.time()
# Helper: check if session is actually active via DB
db_conn = None
if os.path.exists(db):
    try:
        import sqlite3
        db_conn = sqlite3.connect(db)
    except: pass
def check_active(sid):
    if not db_conn: return True
    try:
        row = db_conn.execute('SELECT ended_at FROM sessions WHERE id=?', (sid,)).fetchone()
        if row:
            ended = row[0]
            if ended and isinstance(ended, (int,float)) and ended > 0:
                return False
        last_ts = db_conn.execute('SELECT MAX(timestamp) FROM messages WHERE session_id=?', (sid,)).fetchone()[0]
        if last_ts and (now - last_ts) > 120:
            return False
    except: pass
    return True
# Active gateway sessions from sessions.json
if os.path.exists(sj):
    try:
        data = json.load(open(sj))
        for key, v in data.items():
            sid = v.get('session_id','')
            if sid: seen.add(sid)
            plat = v.get('platform','')
            if plat in ('cli','terminal',''): continue
            updated = v.get('updated_at','')
            active = check_active(sid) if sid else True
            results.append({'sessionId': sid, 'platform': plat, 'updatedAt': updated,
                            'displayName': v.get('display_name',''), 'active': active, 'source': 'hermes',
                            'startedAt': now})
    except: pass
# Recent sessions from state.db (last 2h)
if db_conn:
    try:
        cutoff = now - 7200
        cur = db_conn.execute(
            'SELECT id, source, model, started_at, ended_at, message_count, input_tokens, output_tokens '
            'FROM sessions WHERE started_at > ? ORDER BY started_at DESC LIMIT 20', (cutoff,))
        for r in cur.fetchall():
            sid = r[0]
            if sid in seen: continue
            plat_db = r[1] or ''
            if plat_db in ('cli','terminal',''): continue
            seen.add(sid)
            ended = r[4]
            active = ended is None or (isinstance(ended, (int,float)) and ended == 0)
            if active:
                try:
                    last_ts = db_conn.execute(
                        'SELECT MAX(timestamp) FROM messages WHERE session_id=?', (sid,)).fetchone()[0]
                    if last_ts and (now - last_ts) > 120:
                        active = False
                except: pass
            results.append({'sessionId': sid, 'platform': r[1] or '', 'model': r[2] or '',
                            'startedAt': r[3], 'messageCount': r[5] or 0,
                            'inputTokens': r[6] or 0, 'outputTokens': r[7] or 0,
                            'active': active, 'source': 'hermes'})
    except: pass
if db_conn:
    try: db_conn.close()
    except: pass
print(json.dumps(results))
"#;
    let cmd = format!("python3 -c {}", shell_escape_single(py_script));
    let output = ssh_exec(&ssh_host, &ssh_user, &cmd).await?;
    let trimmed = output.trim();
    let sessions: Vec<serde_json::Value> = serde_json::from_str(trimmed)
        .map_err(|e| format!("parse hermes remote sessions: {}", e))?;
    Ok(sessions)
}

/// Test Hermes SSH connectivity: check if ~/.hermes exists, state.db readable,
/// gateway is running.
#[tauri::command]
async fn test_hermes_ssh(ssh_host: String, ssh_user: String) -> Result<serde_json::Value, String> {
    let py_script = r#"
import json, os, subprocess
checks = {}
hermes_dir = os.path.expanduser('~/.hermes')
checks['hermes_installed'] = os.path.isdir(hermes_dir)
checks['state_db'] = os.path.exists(os.path.join(hermes_dir, 'state.db'))
checks['sessions_json'] = os.path.exists(os.path.join(hermes_dir, 'sessions', 'sessions.json'))
checks['config_yaml'] = os.path.exists(os.path.join(hermes_dir, 'config.yaml'))
# Check gateway process
try:
    out = subprocess.check_output(['pgrep', '-f', 'hermes gateway'], stderr=subprocess.DEVNULL).decode().strip()
    checks['gateway_running'] = len(out.split()) > 0
    checks['gateway_pids'] = out.split()
except: checks['gateway_running'] = False
# Check session count in state.db
if checks['state_db']:
    try:
        import sqlite3, time
        conn = sqlite3.connect(os.path.join(hermes_dir, 'state.db'))
        cutoff = time.time() - 14*86400
        cnt = conn.execute('SELECT COUNT(*) FROM sessions WHERE started_at > ?', (cutoff,)).fetchone()[0]
        checks['sessions_14d'] = cnt
        total = conn.execute('SELECT COUNT(*) FROM sessions').fetchone()[0]
        checks['sessions_total'] = total
        conn.close()
    except Exception as e: checks['db_error'] = str(e)
print(json.dumps(checks))
"#;
    let cmd = format!("python3 -c {}", shell_escape_single(py_script));
    let output = ssh_exec(&ssh_host, &ssh_user, &cmd).await?;
    let trimmed = output.trim();
    let result: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("parse hermes ssh test: {}", e))?;
    Ok(result)
}

/// Escape a string for use inside single quotes in a shell command.
fn shell_escape_single(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
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
        // Read stdin as raw bytes via OpenStandardInput() and decode as UTF-8
        // explicitly. PowerShell's `[Console]::In.ReadToEnd()` snapshots its
        // encoding at first access — setting `[Console]::InputEncoding` later
        // (or even immediately before) is unreliable on CJK Windows where the
        // default is GBK/Shift-JIS. UTF-8 mojibake here would corrupt
        // `last_assistant_message` / `text` fields and crash JSON parsing on
        // the Rust side ("expected ',' or '}' at column N").
        //
        // Forward the raw JSON unchanged. The cursor socket server passes
        // source_override="cursor" to process_claude_event, so we don't need
        // to inject `source` here — and trying to inject via PowerShell string
        // concatenation has bitten us with off-by-one errors in the past.
        let hook_script = r#"$ErrorActionPreference = 'SilentlyContinue'
try {
    $stdin = [System.Console]::OpenStandardInput()
    $ms = New-Object System.IO.MemoryStream
    $buffer = New-Object byte[] 8192
    while (($n = $stdin.Read($buffer, 0, $buffer.Length)) -gt 0) {
        $ms.Write($buffer, 0, $n)
    }
    $bytes = $ms.ToArray()
    if ($bytes.Length -eq 0) { Write-Output '{}'; exit 0 }

    # Strip UTF-8 BOM (EF BB BF). Cursor writes hook stdin as UTF-8 + BOM on
    # Windows; the previous text-mode reader stripped this implicitly, but
    # OpenStandardInput() returns the raw bytes including the BOM. Forwarding
    # the BOM trips serde_json with "expected value at line 1 column 1".
    $offset = 0
    $count = $bytes.Length
    if ($count -ge 3 -and $bytes[0] -eq 0xEF -and $bytes[1] -eq 0xBB -and $bytes[2] -eq 0xBF) {
        $offset = 3
        $count = $count - 3
    }

    # Decode for hook-event sniffing only; the wire payload stays raw bytes.
    $raw = [System.Text.Encoding]::UTF8.GetString($bytes, $offset, $count)
    $hookName = ''
    try { $hookName = ($raw | ConvertFrom-Json).hook_event_name } catch {}

    try {
        $client = [System.Net.Sockets.TcpClient]::new('127.0.0.1', 19284)
        $stream = $client.GetStream()
        $stream.Write($bytes, $offset, $count)
        $stream.Flush()
        $client.Client.Shutdown([System.Net.Sockets.SocketShutdown]::Send)
        $client.Close()
    } catch {}

    # Cursor's required stdout response per hook event type.
    if ($hookName -eq 'beforeSubmitPrompt') {
        Write-Output '{"continue":true}'
    } elseif ($hookName -eq 'beforeShellExecution' -or $hookName -eq 'beforeMCPExecution' -or $hookName -eq 'beforeReadFile') {
        Write-Output '{"permission":"allow"}'
    } else {
        Write-Output '{}'
    }
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

/// Start the Gemini CLI IPC server.
/// On macOS/Linux: Unix domain socket at /tmp/ooclaw-gemini.sock
/// On Windows: TCP server on localhost:19285
fn start_gemini_socket_server(
    claude_state: Arc<Mutex<HashMap<String, ClaudeSession>>>,
    app: tauri::AppHandle,
) {
    #[cfg(unix)]
    {
        let socket_path = "/tmp/ooclaw-gemini.sock";
        let _ = std::fs::remove_file(socket_path);
        let listener = match std::os::unix::net::UnixListener::bind(socket_path) {
            Ok(l) => l,
            Err(e) => { log::warn!("[gemini_socket] bind failed: {}", e); return; }
        };
        log::info!("[gemini_socket] listening on {}", socket_path);

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
                            // Gemini events never block (no PermissionRequest)
                            process_claude_event(&buf, &state, &app, Some("gemini"));
                        }
                    });
                }
            }
        });
    }

    #[cfg(windows)]
    {
        let listener = match std::net::TcpListener::bind("127.0.0.1:19285") {
            Ok(l) => l,
            Err(e) => { log::warn!("[gemini_socket] TCP bind failed: {}", e); return; }
        };
        log::info!("[gemini_socket] listening on 127.0.0.1:19285");

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
                            process_claude_event(&buf, &state, &app, Some("gemini"));
                        }
                    });
                }
            }
        });
    }
}

/// Start the Hermes Agent IPC server.
/// On macOS/Linux: Unix domain socket at /tmp/ooclaw-hermes.sock
/// On Windows: TCP server on localhost:19286
fn start_hermes_socket_server(
    claude_state: Arc<Mutex<HashMap<String, ClaudeSession>>>,
    dismissed: Arc<Mutex<std::collections::HashSet<String>>>,
    app: tauri::AppHandle,
) {
    #[cfg(unix)]
    {
        let socket_path = "/tmp/ooclaw-hermes.sock";
        let _ = std::fs::remove_file(socket_path);
        let listener = match std::os::unix::net::UnixListener::bind(socket_path) {
            Ok(l) => l,
            Err(e) => { log::warn!("[hermes_socket] bind failed: {}", e); return; }
        };
        log::info!("[hermes_socket] listening on {}", socket_path);

        let state = Arc::clone(&claude_state);
        let dismissed = Arc::clone(&dismissed);
        let app2 = app.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    let state = Arc::clone(&state);
                    let dismissed = Arc::clone(&dismissed);
                    let app = app2.clone();
                    std::thread::spawn(move || {
                        use std::io::Read;
                        let mut buf = String::new();
                        let _ = stream.read_to_string(&mut buf);
                        if !buf.is_empty() {
                            // Re-admit dismissed sessions on new activity
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&buf) {
                                if let Some(sid) = parsed.get("sessionId").and_then(|v| v.as_str()) {
                                    if let Ok(mut d) = dismissed.lock() {
                                        d.remove(sid);
                                    }
                                }
                            }
                            process_claude_event(&buf, &state, &app, Some("hermes"));
                        }
                    });
                }
            }
        });
    }

    #[cfg(windows)]
    {
        let listener = match std::net::TcpListener::bind("127.0.0.1:19286") {
            Ok(l) => l,
            Err(e) => { log::warn!("[hermes_socket] TCP bind failed: {}", e); return; }
        };
        log::info!("[hermes_socket] listening on 127.0.0.1:19286");

        let state = Arc::clone(&claude_state);
        let dismissed = Arc::clone(&dismissed);
        let app2 = app.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                if let Ok(mut stream) = stream {
                    let _ = stream.set_read_timeout(Some(std::time::Duration::from_secs(5)));
                    let state = Arc::clone(&state);
                    let dismissed = Arc::clone(&dismissed);
                    let app = app2.clone();
                    std::thread::spawn(move || {
                        use std::io::Read;
                        let mut buf = String::new();
                        let _ = stream.read_to_string(&mut buf);
                        if !buf.is_empty() {
                            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&buf) {
                                if let Some(sid) = parsed.get("sessionId").and_then(|v| v.as_str()) {
                                    if let Ok(mut d) = dismissed.lock() {
                                        d.remove(sid);
                                    }
                                }
                            }
                            process_claude_event(&buf, &state, &app, Some("hermes"));
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
                                    let source = {
                                        let sessions = state.lock().unwrap();
                                        sessions
                                            .get(&session_id)
                                            .map(|session| session.source.clone())
                                            .unwrap_or_else(|| "cc".to_string())
                                    };
                                    if source == "codex" {
                                        // Codex decisions are made in Codex native UI.
                                        // Return an empty hook payload immediately so
                                        // Codex can continue with its own approval flow.
                                        let _ = s.write_all(b"{}");
                                        let _ = s.flush();
                                        return;
                                    }
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
                            // Cursor events should arrive via the dedicated
                            // cursor TCP socket (port 19284). Anything that
                            // reaches the CC socket while still tagged as
                            // cursor (e.g. legacy hook config left behind by
                            // a previous install) is dropped here so we don't
                            // double-process and play two completion sounds.
                            if text.contains("\"cursor_version\"")
                                || text.contains("\"source\":\"cursor\"")
                                || text.contains("\"source\": \"cursor\"")
                            {
                                log::info!(
                                    "[claude_tcp] dropping cursor-originated event on cc socket (len={})",
                                    text.len()
                                );
                                return;
                            }
                            // Codex on Windows is still unsupported; keep the
                            // drop guard until that integration is ported.
                            if text.contains("\"source\":\"codex\"")
                                || text.contains("\"source\": \"codex\"")
                            {
                                log::info!("[claude_tcp] dropping codex-originated event on windows (len={})", text.len());
                                return;
                            }
                            if let Some((session_id, hook_event)) = process_claude_event(&text, &state, &app, None) {
                                if hook_event == "PermissionRequest" {
                                    let source = {
                                        let sessions = state.lock().unwrap();
                                        sessions
                                            .get(&session_id)
                                            .map(|session| session.source.clone())
                                            .unwrap_or_else(|| "cc".to_string())
                                    };
                                    if source == "codex" {
                                        // Keep behavior aligned with Unix implementation:
                                        // Codex approvals should remain in Codex UI.
                                        let _ = s.write_all(b"{}");
                                        let _ = s.flush();
                                        return;
                                    }
                                    let (tx, rx) = std::sync::mpsc::channel::<String>();
                                    {
                                        let mut map = pending.lock().unwrap();
                                        map.insert(session_id.clone(), tx);
                                    }
                                    s.set_read_timeout(None).ok();
                                    match rx.recv_timeout(std::time::Duration::from_secs(600)) {
                                        Ok(response_json) => {
                                            let bytes = response_json.as_bytes();
                                            let write_result = s.write_all(bytes);
                                            let flush_result = s.flush();
                                            // Only emit a log line if anything looked off — successful
                                            // permission round-trips are silent in release to avoid noise.
                                            if write_result.is_err() || flush_result.is_err() {
                                                log::warn!(
                                                    "[claude_tcp] permission response write failed session={} bytes={} write_ok={} flush_ok={}",
                                                    &session_id[..session_id.len().min(8)],
                                                    bytes.len(),
                                                    write_result.is_ok(),
                                                    flush_result.is_ok(),
                                                );
                                            } else if cfg!(debug_assertions) {
                                                log::info!(
                                                    "[claude_tcp] permission response written session={} bytes={}",
                                                    &session_id[..session_id.len().min(8)],
                                                    bytes.len(),
                                                );
                                            }
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

    // Always accept the first mouse event. By default NSView returns NO,
    // which means the first click on an inactive floating window only
    // activates the app — pointerdown is never delivered to the webview,
    // breaking direct drag on the mini mascot. Returning YES delivers
    // every click to the view immediately.
    unsafe extern "C-unwind" fn accepts_first_mouse(
        _this: &AnyObject,
        _cmd: Sel,
        _event: *mut AnyObject,
    ) -> bool {
        true
    }

    fn patch_class(class_name: &'static std::ffi::CStr, text_input_protocol: Option<&'static AnyProtocol>) {
        let Some(cls) = AnyClass::get(class_name) else {
            log::warn!("[ime] class not found: {}", class_name.to_string_lossy());
            return;
        };

        let cls_ptr = cls as *const AnyClass as *mut AnyClass;
        let level_encoding = CString::new("q@:").unwrap();
        let bool_arg_encoding = CString::new("c@:@").unwrap();
        unsafe {
            if let Some(protocol) = text_input_protocol {
                let _ = ffi::class_addProtocol(cls_ptr, protocol);
            }
            let _ = ffi::class_addMethod(
                cls_ptr,
                sel!(windowLevel),
                std::mem::transmute::<unsafe extern "C-unwind" fn(&AnyObject, Sel) -> isize, Imp>(window_level),
                level_encoding.as_ptr(),
            );
            // Use class_replaceMethod so we win even when the class (or one
            // of its superclasses, via class_addMethod's behavior) already
            // implements acceptsFirstMouse:.
            let _ = ffi::class_replaceMethod(
                cls_ptr,
                sel!(acceptsFirstMouse:),
                std::mem::transmute::<
                    unsafe extern "C-unwind" fn(&AnyObject, Sel, *mut AnyObject) -> bool,
                    Imp,
                >(accepts_first_mouse),
                bool_arg_encoding.as_ptr(),
            );
            log::info!(
                "[first-mouse] patched {} with acceptsFirstMouse:=YES",
                class_name.to_string_lossy()
            );
        }
    }

    INSTALL_ONCE.call_once(|| {
        let text_input_protocol = AnyProtocol::get(c"NSTextInputClient");
        patch_class(c"WryWebView", text_input_protocol);
        patch_class(c"WKWebView", text_input_protocol);
        // Patch NSView itself so EVERY subclass (including private/leaf
        // WebKit views whose names we cannot rely on across macOS versions)
        // returns YES from acceptsFirstMouse:. acceptsFirstMouse: is only
        // queried when the click target's window is not the key window, so
        // patching the base class is safe for normal activating windows.
        patch_class(c"NSView", None);
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
    #[cfg(target_os = "windows")]
    {
        // WebView2 hardware video decode can drop VP9 alpha; force software decode.
        let key = "WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS";
        let flag = "--disable-accelerated-video-decode";
        let merged = match std::env::var(key) {
            Ok(existing) if !existing.contains(flag) && !existing.trim().is_empty() => format!("{} {}", existing, flag),
            Ok(existing) if existing.contains(flag) => existing,
            _ => flag.to_string(),
        };
        std::env::set_var(key, merged);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .plugin(tauri_plugin_dialog::init())
        // Autostart: enabled via the settings toggle. We pass
        // `--launched-at-login` so future code can detect login-launches and
        // skip noisy first-run UI if needed.
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            Some(vec!["--launched-at-login"]),
        ))
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
        .register_uri_scheme_protocol("codexpet", |_ctx, req| {
            // Custom codex pets the user dropped into `~/.codex/pets`.
            // Avatars are loaded through this protocol so the picker can
            // display sprites that live outside the bundled assets dir.
            let raw_path = req.uri().path();
            let path = percent_decode_str(raw_path).decode_utf8_lossy();
            let root = codex_pets_dir().unwrap_or_default();
            let file_path = root.join(path.trim_start_matches('/'));
            build_asset_response(&req, path.as_ref(), &file_path, cfg!(target_os = "windows"), "codexpet")
        })
        .setup(|app| {
            // Fix PATH so openclaw (Node.js script) and node are both reachable
            fix_path();

            // Install Claude + Codex hooks on every startup (idempotent)
            if let Err(e) = tauri::async_runtime::block_on(install_claude_hooks()) {
                log::warn!("Failed to install Claude hooks on startup: {}", e);
            }
            // Install Cursor hooks + terminal-focus extension on startup (idempotent)
            if let Err(e) = tauri::async_runtime::block_on(install_cursor_hooks()) {
                log::warn!("Failed to install Cursor hooks on startup: {}", e);
            }
            // Install Gemini CLI hooks on startup (idempotent, skips if ~/.gemini/ missing)
            if let Err(e) = tauri::async_runtime::block_on(install_gemini_hooks()) {
                log::warn!("Failed to install Gemini hooks on startup: {}", e);
            }
            // Install Hermes Agent plugin on startup (idempotent, skips if ~/.hermes/ missing)
            if let Err(e) = tauri::async_runtime::block_on(install_hermes_hooks()) {
                log::warn!("Failed to install Hermes hooks on startup: {}", e);
            }

            // One log file per app run, named with a startup timestamp so we
            // never lose the previous session's logs to rotation. Goes to the
            // OS-standard logs dir alongside the rolling `oc-claw.log`.
            // On Windows: %LOCALAPPDATA%\com.openclaw.ooclaw\logs\run-*.log
            //
            // Drop the default Stdout target — `pnpm tauri dev` would
            // otherwise mirror every log line into the terminal, drowning
            // out the actual rust/vite output. The file targets keep the
            // diagnostic trail intact for offline inspection.
            let run_log_name = format!(
                "run-{}.log",
                chrono::Local::now().format("%Y-%m-%d_%H-%M-%S")
            );
            app.handle().plugin(
                tauri_plugin_log::Builder::default()
                    .level(log::LevelFilter::Info)
                    .max_file_size(64 * 1024 * 1024)
                    .clear_targets()
                    .target(tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::LogDir { file_name: None },
                    ))
                    .target(tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::LogDir {
                            file_name: Some(run_log_name),
                        },
                    ))
                    .target(tauri_plugin_log::Target::new(
                        tauri_plugin_log::TargetKind::Webview,
                    ))
                    .build(),
            )?;

            // Run the WKWebView swizzle AFTER the log plugin is initialized so
            // its [first-mouse] / IME log lines are actually visible in the
            // tauri-plugin-log stream. Order vs window creation is fine —
            // setup() runs after the mini webview already exists.
            #[cfg(target_os = "macos")]
            install_wry_webview_ime_fix();

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
                            // Deliver mouse-moved / mouse-entered events to
                            // the mini window even when it is not the key
                            // window. Without this, hovering the unfocused
                            // mascot does not fire mouseenter on the webview
                            // and the codex sprite never starts its jump
                            // until the user clicks once to activate.
                            let _: () = msg_send![obj, setAcceptsMouseMovedEvents: true];
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
                            let y = sy + sh - win_h - MASCOT_TOP_INSET;
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
                    let _ = win.set_position(tauri::LogicalPosition::new(x, MASCOT_TOP_INSET));
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

            // Start Cursor socket server (shares ClaudeState for unified session tracking).
            // Unix uses /tmp/occlaw-cursor.sock, Windows uses TCP 127.0.0.1:19284.
            {
                let claude_state = app.state::<ClaudeState>();
                let sessions_arc = Arc::clone(&claude_state.sessions);
                start_cursor_socket_server(sessions_arc, app.handle().clone());
            }

            // Start Gemini CLI socket server (shares ClaudeState for unified session tracking).
            // Unix uses /tmp/ooclaw-gemini.sock, Windows uses TCP 127.0.0.1:19285.
            {
                let claude_state = app.state::<ClaudeState>();
                let sessions_arc = Arc::clone(&claude_state.sessions);
                start_gemini_socket_server(sessions_arc, app.handle().clone());
            }

            // Start Hermes Agent socket server.
            // Unix uses /tmp/ooclaw-hermes.sock, Windows uses TCP 127.0.0.1:19286.
            {
                let claude_state = app.state::<ClaudeState>();
                let sessions_arc = Arc::clone(&claude_state.sessions);
                let dismissed_arc = Arc::clone(&claude_state.dismissed);
                start_hermes_socket_server(sessions_arc, dismissed_arc, app.handle().clone());
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
        .invoke_handler(tauri::generate_handler![get_status, send_chat, open_detail_panel, save_character_gif, delete_character_assets, delete_character_gif, get_agents, get_health, get_agent_metrics, interrupt_agent, scan_characters, get_agent_extra_info, open_mini, close_mini, set_mini_expanded, set_mini_size, set_efficiency_hover_tracking, resize_mini_height, move_mini_by, get_mini_origin, get_mini_monitor_rect, set_mini_origin, set_ime_mode, get_agent_sessions, get_session_preview, get_session_messages, get_active_sessions, proxy_post, play_sound, get_claude_sessions, get_claude_conversation, install_claude_hooks, install_cursor_hooks, install_gemini_hooks, install_hermes_hooks, test_hermes_hook, get_hermes_remote_stats, get_hermes_remote_sessions, get_hermes_sessions_summary, get_hermes_recent_activity, get_hermes_remote_recent_activity, test_hermes_ssh, remove_claude_session, resolve_claude_permission, get_claude_stats, open_url, activate_app, focus_cursor_terminal, check_ax_permission, request_ax_permission, jump_to_claude_terminal, check_for_update, run_update, close_ssh, read_local_file, list_backgrounds, save_background, get_background_data, exit_app, get_ssh_key_info, reset_ssh, get_ui_scale, list_custom_codex_pets, open_codex_pets_dir, import_codex_pet, pick_codex_pet_folder, reassert_floating, spawn_demo_mascot, close_demo_mascot, close_demo_mascots, debug_log, update_tray_language, set_pet_mode_window, set_pet_context_menu, set_pet_pomodoro_active, get_now_playing, get_system_idle_time, get_keyboard_idle_secs])
        .manage(ActiveAgentPid { pid: Mutex::new(None) })
        .manage(ClaudeState { sessions: Arc::new(Mutex::new(HashMap::new())), pending_permissions: Arc::new(Mutex::new(HashMap::new())), dismissed: Arc::new(Mutex::new(std::collections::HashSet::new())) })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
