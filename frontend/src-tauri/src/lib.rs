use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use percent_encoding::percent_decode_str;

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

/// Read the last `n` lines of a file using pure Rust (cross-platform replacement for `tail -n`).
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
            // Use both '/' and '\\' to handle cross-platform paths
            let basename = sf.rsplit(|c: char| c == '/' || c == '\\').next().unwrap_or("");
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
        // Windows: ControlMaster not supported. Validate the connection by running
        // a simple test command, then create a marker file to skip future validation.
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            tokio::process::Command::new("ssh")
                .args([
                    "-o", "StrictHostKeyChecking=no",
                    "-o", "BatchMode=yes",
                    "-o", "ConnectTimeout=10",
                    &host_key,
                    "echo ok",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .output(),
        ).await;

        let output = match result {
            Ok(Ok(o)) => o,
            Ok(Err(e)) => {
                ssh_backoff_record_failure(&host_key);
                return Err(format!("ssh test: {}", e));
            }
            Err(_) => {
                ssh_backoff_record_failure(&host_key);
                return Err(format!("ssh to {} timed out after 15s", host_key));
            }
        };

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            ssh_backoff_record_failure(&host_key);
            let count = ssh_backoff_map().lock().unwrap().get(&host_key).map(|s| s.fail_count).unwrap_or(0);
            let code = output.status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
            log::warn!("[ssh] connection to {} failed (attempt {}), entering backoff", host_key, count);
            return Err(format!("SSH connection failed [exit {}]: {}", code, stderr));
        }

        // Create marker file so subsequent calls skip the test
        let _ = std::fs::write(&control_path, "connected");
    }

    // Detect which key was used by querying ssh config for this host.
    if let Ok(cfg_output) = tokio::process::Command::new("ssh")
        .args(["-G", &host_key])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .await
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
/// On Windows: opens a new SSH connection for each command (no ControlMaster support).
/// If the command fails (e.g. stale socket), removes the socket and retries once.
async fn ssh_exec(ssh_host: &str, ssh_user: &str, cmd: &str) -> Result<String, String> {
    ensure_ssh_master(ssh_host, ssh_user).await?;
    let target = format!("{}@{}", ssh_user, ssh_host);
    // Prepend a safe system PATH so basic commands (ls, cat, tail…) are always
    // available regardless of how the remote server's login environment is configured.
    let safe_cmd = format!(
        "export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:$PATH && {}",
        cmd
    );

    #[cfg(unix)]
    let control_path = ssh_control_path(ssh_user, ssh_host);
    #[cfg(unix)]
    let cp = format!("ControlPath={}", control_path);

    let mut ssh_args: Vec<&str> = vec![
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=5",
    ];
    #[cfg(unix)]
    { ssh_args.extend_from_slice(&["-o", &cp]); }
    #[cfg(windows)]
    { ssh_args.extend_from_slice(&["-o", "StrictHostKeyChecking=no"]); }

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
    // Exit 255 = SSH transport error. Any other non-zero exit code comes from
    // the remote command itself and should be returned as-is.
    if exit_code != 255 {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let mut msg = format!("ssh cmd failed [exit {}]", exit_code);
        if !stderr.trim().is_empty() { msg.push_str(&format!("\nstderr: {}", stderr.trim())); }
        if !stdout.trim().is_empty() { msg.push_str(&format!("\nstdout: {}", stdout.trim())); }
        return Err(msg);
    }

    // SSH transport failure — remove the stale socket/marker and retry once.
    log::warn!("[ssh] transport error (exit 255), removing stale socket and retrying");
    #[cfg(unix)]
    { let _ = tokio::fs::remove_file(&control_path).await; }
    #[cfg(windows)]
    { let _ = tokio::fs::remove_file(&ssh_control_path(ssh_user, ssh_host)).await; }

    ensure_ssh_master(ssh_host, ssh_user).await?;

    // Rebuild args for retry
    let mut ssh_args2: Vec<&str> = vec![
        "-o", "BatchMode=yes",
        "-o", "ConnectTimeout=5",
    ];
    #[cfg(unix)]
    { ssh_args2.extend_from_slice(&["-o", &cp]); }
    #[cfg(windows)]
    { ssh_args2.extend_from_slice(&["-o", "StrictHostKeyChecking=no"]); }
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

/// Close an active SSH ControlMaster socket (macOS/Linux) or remove marker (Windows).
async fn close_ssh_master(ssh_host: &str, ssh_user: &str) -> Result<(), String> {
    let control_path = ssh_control_path(ssh_user, ssh_host);
    if !std::path::Path::new(&control_path).exists() {
        ssh_backoff_reset(&format!("{}@{}", ssh_user, ssh_host));
        return Ok(());
    }
    #[cfg(unix)]
    {
        let target = format!("{}@{}", ssh_user, ssh_host);
        let cp = format!("ControlPath={}", control_path);
        let _ = tokio::process::Command::new("ssh")
            .args(["-o", &cp, "-O", "exit", &target])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .await;
    }
    let _ = tokio::fs::remove_file(&control_path).await;
    ssh_backoff_reset(&format!("{}@{}", ssh_user, ssh_host));
    log::info!("[close_ssh_master] closed socket for {}@{}", ssh_user, ssh_host);
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
        let listening = tokio::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "(Get-NetTCPConnection -LocalPort 18789 -State Listen -ErrorAction SilentlyContinue | Measure-Object).Count",
            ])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output()
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
        app.path().resource_dir().map(|p| p.join("assets").join("builtin")).map_err(|e| e.to_string())
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
            results.push(serde_json::Value::Object(char_obj));
        }
    }

    // Scan built-in assets
    let builtin_prefix = if cfg!(debug_assertions) { "/assets/builtin" } else { "localasset://localhost" };
    if let Ok(builtin_dir) = builtin_assets_dir(&app) {
        scan_dir(&builtin_dir, builtin_prefix, true, &ip_map, &mut results);
    }

    // Scan custom assets
    let custom_prefix = if cfg!(debug_assertions) { "/assets/custom" } else { "customasset://localhost" };
    if let Ok(custom_dir) = custom_assets_dir(&app) {
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

    // === local mode — read ~/.openclaw/agents/ directly (no CLI dependency) ===
    let home = home_dir_string();
    let agents_dir = PathBuf::from(&home).join(".openclaw").join("agents");
    log::info!("[get_agents] local mode, agents_dir={:?}, exists={}", agents_dir, agents_dir.exists());

    let entries = std::fs::read_dir(&agents_dir)
        .map_err(|e| { log::error!("[get_agents] read_dir failed: {}", e); format!("read agents dir: {}", e) })?;

    let mut agents: Vec<AgentInfo> = Vec::new();
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        log::info!("[get_agents] found entry: {:?} is_dir={}", path, path.is_dir());
        if !path.is_dir() { continue; }
        let id = entry.file_name().to_string_lossy().to_string();
        let config_path = path.join("agent.json");
        log::info!("[get_agents] agent id={}, config exists={}", id, config_path.exists());
        let (name, emoji) = if config_path.exists() {
            match std::fs::read_to_string(&config_path) {
                Ok(c) => {
                    log::info!("[get_agents] agent.json content len={}", c.len());
                    let val: serde_json::Value = serde_json::from_str(&c).unwrap_or_default();
                    (
                        val.get("identityName").or_else(|| val.get("identity_name")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                        val.get("identityEmoji").or_else(|| val.get("identity_emoji")).and_then(|v| v.as_str()).map(|s| s.to_string()),
                    )
                }
                Err(e) => { log::error!("[get_agents] read agent.json failed: {}", e); (None, None) }
            }
        } else {
            (None, None)
        };
        log::info!("[get_agents] pushing agent: id={} name={:?} emoji={:?}", id, name, emoji);
        agents.push(AgentInfo { id, identity_name: name, identity_emoji: emoji });
    }
    log::info!("[get_agents] returning {} agents", agents.len());
    Ok(agents)
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
            return Ok(HealthResult { agents });
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
        return Ok(HealthResult { agents });
    }

    // === local mode — content-based detection with session-level data ===
    let home = home_dir_string();
    let agents_dir = std::path::PathBuf::from(&home).join(".openclaw").join("agents");

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
                        let lines = tail_lines_from_file(&p, 5);
                        if !lines.is_empty() {
                            if let Some(fname) = p.file_name() {
                                tails.insert(fname.to_string_lossy().to_string(), lines);
                            }
                        }
                    }
                }
                let agent = build_agent_health_from_meta(&agent_id, &meta_str, &tails);
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
            let lines = tail_lines_from_file(&f.path(), 5);
            check_agent_active_from_lines(&lines)
        } else { false };
        agents.push(AgentHealth { agent_id, active, sessions: vec![] });
    }

    Ok(HealthResult { agents })
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
                        let win_w = 60.0;
                        let win_h = 45.0;
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
                let win_w = (60.0 * ui).round();
                let win_h = (45.0 * ui).round();
                let notch_off = (80.0 * ui).round();
                let x = sw / 2.0 + notch_off;
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
            }
            win.show().map_err(|e| e.to_string())?;
            win.set_focus().map_err(|e| e.to_string())?;
        }
        return Ok(());
    }

    let builder = WebviewWindowBuilder::new(&app, "mini", WebviewUrl::App("index.html#/mini".into()))
        .title("ooclaw Mini")
        .inner_size(60.0, 45.0)
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
                    let win_w = 60.0;
                    let win_h = 45.0;
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
            let win_w = (60.0 * ui).round();
            let win_h = (45.0 * ui).round();
            let notch_off = (80.0 * ui).round();
            let x = sw / 2.0 + notch_off;
            let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
            let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
        }
        let _ = win.show();
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
            use objc2::runtime::AnyObject;
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};
            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };
                let frame: NSRect = unsafe { msg_send![obj, frame] };
                let new_frame = NSRect::new(
                    NSPoint::new(x, y),
                    NSSize::new(frame.size.width, frame.size.height),
                );
                unsafe {
                    let _: () = msg_send![obj, setFrame: new_frame, display: true, animate: false];
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

/// Resize/reposition the mini window between collapsed (small, right of notch)
/// and expanded (larger, centered on notch) states.
#[tauri::command]
async fn set_mini_expanded(app: tauri::AppHandle, expanded: bool, position: Option<String>) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    let pos = position.unwrap_or_else(|| "right".to_string());

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
                    unsafe {
                        let _: () = msg_send![obj, setLevel: 27isize];
                    }
                    if expanded {
                        let win_w = 400.0;
                        let win_h = 400.0;
                        let x = sx + (sw - win_w) / 2.0;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                    } else {
                        let win_w = 60.0;
                        let win_h = 45.0;
                        let x = collapsed_x(sx, sw, win_w, &pos, notch_off);
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                    }
                }
            }
        }).map_err(|e| e.to_string())?;
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows: DPI-aware positioning and sizing.
        // Collapsed → top-center (simulating macOS notch); Expanded → centered.
        if let Ok(Some(monitor)) = win.current_monitor() {
            let scale = monitor.scale_factor();
            let sw = monitor.size().width as f64 / scale;
            let ui = win_ui_scale(&monitor);
            if expanded {
                let win_w = (400.0 * ui).round();
                let win_h = (400.0 * ui).round();
                let x = (sw - win_w) / 2.0;
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
            } else {
                let win_w = (60.0 * ui).round();
                let win_h = (45.0 * ui).round();
                let notch_off = (80.0 * ui).round();
                let x = if pos == "left" { sw / 2.0 - notch_off - win_w } else { sw / 2.0 + notch_off };
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
            }
        }
        let _ = win.set_always_on_top(true);
    }

    Ok(())
}

/// Resize the expanded mini window height while keeping it top-aligned.
/// macOS: bottom-left origin, so adjust y to keep bottom aligned.
/// Windows: top-left origin, so just resize height.
#[tauri::command]
async fn resize_mini_height(app: tauri::AppHandle, height: f64) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    // Scale height limits on Windows to match DPI-aware window sizes
    #[cfg(target_os = "windows")]
    let h = {
        let ui = if let Ok(Some(m)) = win.current_monitor() { win_ui_scale(&m) } else { 1.0 };
        (height * ui).round().max(45.0 * ui).min(400.0 * ui)
    };
    #[cfg(not(target_os = "windows"))]
    let h = height.max(45.0).min(400.0);

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
                let new_frame = NSRect::new(
                    NSPoint::new(cur.origin.x, sf.origin.y + sf.size.height - h),
                    NSSize::new(cur.size.width, h),
                );
                unsafe {
                    let _: () = msg_send![obj, setFrame: new_frame, display: true, animate: false];
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

/// Resize the mini window to 3/4 of screen, centered, with normal window level.
/// Used for settings panel mode. Pass `restore: true` to go back to mini mode.
#[tauri::command]
async fn set_mini_size(app: tauri::AppHandle, restore: bool, position: Option<String>) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;
    let pos = position.unwrap_or_else(|| "right".to_string());

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
                    if restore {
                        let win_w = 60.0;
                        let win_h = 45.0;
                        let x = collapsed_x(sx, sw, win_w, &pos, notch_off);
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setLevel: 27isize];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                            let _: () = msg_send![obj, orderFrontRegardless];
                        }
                    } else {
                        let win_w = (sw * 0.85).round();
                        let win_h = (sh * 0.85).round();
                        let x = sx + (sw - win_w) / 2.0;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setLevel: 0isize];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
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
            let sw = monitor.size().width as f64 / scale;
            let sh = monitor.size().height as f64 / scale;
            let ui = win_ui_scale(&monitor);
            if restore {
                let win_w = (60.0 * ui).round();
                let win_h = (45.0 * ui).round();
                let notch_off = (80.0 * ui).round();
                let x = if pos == "left" { sw / 2.0 - notch_off - win_w } else { sw / 2.0 + notch_off };
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                let _ = win.set_always_on_top(true);
                let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
            } else {
                let win_w = (sw * 0.85).round();
                let win_h = (sh * 0.85).round();
                let x = (sw - win_w) / 2.0;
                let _ = win.set_always_on_top(false);
                let _ = win.set_size(tauri::LogicalSize::new(win_w, win_h));
                // Top-aligned like macOS (not center)
                let _ = win.set_position(tauri::LogicalPosition::new(x, 0.0));
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

    // Check which sessions are active (cross-platform)
    let home = home_dir_string();
    let agent_dir = if agent_id.is_empty() { "main" } else { &agent_id };

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

        // Check 1: lsof / modification-time based detection
        let mut is_active = open_jsonl.iter().any(|p| {
            p.starts_with(&session_file) || session_file.starts_with(p.as_str())
            // Cross-platform path normalization
            || p.replace('\\', "/").starts_with(&session_file.replace('\\', "/"))
            || session_file.replace('\\', "/").starts_with(&p.replace('\\', "/"))
        });

        // Check 2: content-based detection (cross-platform, more reliable on Windows)
        if !is_active {
            let lines = tail_lines_from_file(std::path::Path::new(&session_file), 5);
            is_active = check_agent_active_from_lines(&lines);
        }

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
                    if cleaned.chars().count() > 500 {
                        format!("{}...", cleaned.chars().take(500).collect::<String>())
                    } else { cleaned }
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
            let truncated = if content.chars().count() > 500 {
                format!("{}...", content.chars().take(500).collect::<String>())
            } else { content };
            messages.push(ChatMessage { role: role.to_string(), text: truncated, timestamp: ts });
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
            if cleaned.chars().count() > 500 {
                let s: String = cleaned.chars().take(500).collect();
                format!("{}...", s)
            } else {
                cleaned
            }
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
            let lines = tail_lines_from_file(std::path::Path::new(&file_path), 5);
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
        .title("ooclaw - Detail")
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
}

struct ClaudeState {
    sessions: Arc<Mutex<HashMap<String, ClaudeSession>>>,
}

/// Compute the JSONL session file path (matching notchi's ConversationParser.sessionFilePath)
fn claude_session_file_path(session_id: &str, cwd: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_default();
    let project_dir = cwd.replace('/', "-").replace('\\', "-").replace('.', "-");
    home.join(".claude").join("projects").join(project_dir).join(format!("{}.jsonl", session_id))
}

/// Check if a JSONL file indicates an interrupted session
fn check_interrupted(path: &std::path::Path) -> bool {
    if let Ok(content) = std::fs::read_to_string(path) {
        // Check last few lines for interruption marker (like notchi's ConversationParser)
        for line in content.lines().rev().take(20) {
            if line.contains("\"type\":\"user\"") && line.contains("[Request interrupted by user") {
                return true;
            }
        }
    }
    false
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

                // Interruption detection: working but file shows interrupted
                if session.status == "processing" || session.status == "tool_running" {
                    if check_interrupted(&path2) {
                        log::info!("File watcher: interrupted session {}", sid2);
                        session.status = "stopped".to_string();
                        session.is_processing = false;
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

#[tauri::command]
async fn get_claude_sessions(state: tauri::State<'_, ClaudeState>) -> Result<Vec<ClaudeSession>, String> {
    let sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    let mut list: Vec<ClaudeSession> = sessions.values()
        .filter(|s| !s.cwd.is_empty())
        .cloned()
        .collect();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(list)
}

#[tauri::command]
async fn remove_claude_session(session_id: String, state: tauri::State<'_, ClaudeState>) -> Result<(), String> {
    let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    sessions.remove(&session_id);
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
async fn get_claude_stats() -> Result<ClaudeStats, String> {
    let home = dirs::home_dir().ok_or("no home dir")?;
    let claude_dir = home.join(".claude").join("projects");
    if !claude_dir.exists() {
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

    if let Ok(project_dirs) = std::fs::read_dir(&claude_dir) {
        for project_entry in project_dirs.flatten() {
            if !project_entry.path().is_dir() { continue; }
            if let Ok(files) = std::fs::read_dir(project_entry.path()) {
                for file_entry in files.flatten() {
                    let path = file_entry.path();
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }

                    // Use file modification time to skip old files quickly
                    if let Ok(meta) = path.metadata() {
                        if let Ok(modified) = meta.modified() {
                            let mod_time: chrono::DateTime<chrono::Utc> = modified.into();
                            if mod_time < cutoff { continue; }
                        }
                    }

                    let content = match std::fs::read_to_string(&path) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };

                    let mut session_counted = false;
                    for line in content.lines() {
                        if line.trim().is_empty() { continue; }
                        let parsed: serde_json::Value = match serde_json::from_str(line) {
                            Ok(v) => v,
                            Err(_) => continue,
                        };

                        if parsed.get("type").and_then(|v| v.as_str()) != Some("assistant") { continue; }
                        let msg = match parsed.get("message") {
                            Some(m) => m,
                            None => continue,
                        };
                        let usage = match msg.get("usage") {
                            Some(u) => u,
                            None => continue,
                        };

                        // Extract date from timestamp
                        let date = parsed.get("timestamp")
                            .and_then(|v| v.as_str())
                            .and_then(|ts| ts.get(..10))
                            .unwrap_or("")
                            .to_string();

                        if date < cutoff_str { continue; }

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
                    }

                    // Count session per day (use first message date)
                    if session_counted {
                        if let Some(first_day) = daily_map.keys().next().cloned() {
                            // Actually count in the day the session was modified
                            if let Ok(meta) = path.metadata() {
                                if let Ok(modified) = meta.modified() {
                                    let mod_time: chrono::DateTime<chrono::Utc> = modified.into();
                                    let day = mod_time.format("%Y-%m-%d").to_string();
                                    if let Some(entry) = daily_map.get_mut(&day) {
                                        entry.sessions += 1;
                                    } else if let Some(entry) = daily_map.get_mut(&first_day) {
                                        entry.sessions += 1;
                                    }
                                }
                            }
                        }
                    }
                }
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

/// Check for updates by fetching the version manifest from the official website.
/// The manifest is a static JSON file hosted on Vercel at /update/latest.json,
/// which is manually updated on each release — giving us full control over
/// when users see an update prompt (independent of GitHub Releases).
///
/// Expected manifest format:
///   { "version": "1.6.0", "url": "https://github.com/.../oc-claw_1.6.0_aarch64.dmg", "notes": "..." }
#[tauri::command]
async fn check_for_update(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let current = app.config().version.clone().unwrap_or_default();

    let update_url = if cfg!(debug_assertions) {
        "http://[::1]:4321/update/latest.json"
    } else {
        "https://www.oc-claw.ai/update/latest.json"
    };
    log::info!("[update] checking {} (current={})", update_url, current);
    // On macOS, reqwest can inherit system proxies. That breaks local update checks
    // in dev because localhost requests may be forwarded to the proxy and return 502.
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
    log::info!("[update] latest={} hasUpdate={}", json["version"], version_cmp(json["version"].as_str().unwrap_or(""), &current));
    let latest = json["version"].as_str().unwrap_or("");
    let has_update = version_cmp(latest, &current);
    Ok(serde_json::json!({
        "current": current,
        "latest": latest,
        "hasUpdate": has_update,
        // The DMG download URL from the manifest (points to GitHub Release asset)
        "url": json["url"].as_str().unwrap_or(""),
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
$pid = {pid}
$installerPath = '{installer_path}'
$logPath = '{log_path}'

function Log($msg) {{
    "$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss') $msg" | Out-File -Append $logPath
}}

Log "Waiting for app pid $pid to exit"
$sw = [System.Diagnostics.Stopwatch]::StartNew()
while ($sw.Elapsed.TotalSeconds -lt 60) {{
    try {{
        $p = Get-Process -Id $pid -ErrorAction SilentlyContinue
        if (-not $p) {{ break }}
    }} catch {{ break }}
    Start-Sleep -Milliseconds 500
}}

Log "Installing update from $installerPath"
if ($installerPath.EndsWith('.msi')) {{
    Start-Process msiexec.exe -ArgumentList '/i', "`"$installerPath`"", '/quiet', '/norestart' -Wait -NoNewWindow
}} else {{
    Start-Process $installerPath -ArgumentList '/S', '/norestart' -Wait -NoNewWindow
}}

Log "Launching updated app"
$appPath = Join-Path $env:LOCALAPPDATA 'oc-claw\oc-claw.exe'
if (-not (Test-Path $appPath)) {{
    $appPath = Join-Path $env:ProgramFiles 'oc-claw\oc-claw.exe'
}}
if (Test-Path $appPath) {{
    Start-Process $appPath
}} else {{
    Log "Warning: could not find oc-claw.exe to relaunch"
}}
"#,
            pid = std::process::id(),
            installer_path = dmg_path.display().to_string().replace('\\', "\\\\").replace('\'', "''"),
            log_path = log_path.display().to_string().replace('\\', "\\\\").replace('\'', "''"),
        );
        std::fs::write(&helper_path, &script).map_err(|e| format!("failed to write helper script: {e}"))?;

        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|e| format!("failed to open helper log: {e}"))?;
        let log_file_err = log_file
            .try_clone()
            .map_err(|e| format!("failed to clone helper log: {e}"))?;
        std::process::Command::new("powershell")
            .args(["-ExecutionPolicy", "Bypass", "-File"])
            .arg(&helper_path)
            .stdout(std::process::Stdio::from(log_file))
            .stderr(std::process::Stdio::from(log_file_err))
            .spawn()
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
    let home = dirs::home_dir().ok_or("no home dir")?;
    let claude_dir = home.join(".claude").join("projects");
    if !claude_dir.exists() {
        return Ok(vec![]);
    }

    // Find the JSONL file for this session across project dirs
    let mut jsonl_path: Option<PathBuf> = None;
    if let Ok(entries) = std::fs::read_dir(&claude_dir) {
        for entry in entries.flatten() {
            let p = entry.path().join(format!("{}.jsonl", session_id));
            if p.exists() {
                jsonl_path = Some(p);
                break;
            }
        }
    }

    let path = match jsonl_path {
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
        if msg_type != "assistant" && msg_type != "user" && msg_type != "human" { continue; }
        if parsed.get("isMeta").and_then(|v| v.as_bool()).unwrap_or(false) { continue; }

        let role = if msg_type == "assistant" { "assistant" } else { "user" };

        // Extract text content
        let text = if let Some(s) = parsed.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_str()) {
            s.to_string()
        } else if let Some(arr) = parsed.get("message").and_then(|m| m.get("content")).and_then(|c| c.as_array()) {
            arr.iter()
                .filter(|b| {
                    let t = b.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    t == "text"
                })
                .filter_map(|b| b.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            continue;
        };

        if text.trim().is_empty() { continue; }
        if text.starts_with("<command-name>") || text.starts_with("[Request interrupted") { continue; }
        if text.starts_with("<task-notification>") || text.starts_with("<local-command") { continue; }

        // Replace compaction summary with short indicator
        let text = if text.starts_with("This session is being continued from a previous conversation") {
            "/compact".to_string()
        } else { text };

        let timestamp = parsed.get("timestamp").and_then(|t| t.as_str()).map(String::from);
        messages.push(ChatMessage { role: role.to_string(), text, timestamp });
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
    let hook_path = hooks_dir.join("ooclaw-hook.cmd");

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
}

if hook_event == 'UserPromptSubmit':
    prompt = input_data.get('prompt', '')
    if prompt:
        output['userPrompt'] = prompt[:200]

tool = input_data.get('tool_name', '')
if tool:
    output['tool'] = tool

tool_input = input_data.get('tool_input', {})
if tool_input:
    output['toolInput'] = json.dumps(tool_input)[:200]

try:
    sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    sock.connect('$SOCKET_PATH')
    sock.sendall(json.dumps(output).encode())
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
        // Windows hook: uses PowerShell instead of Python (Python may not be installed).
        // The logic is in a .ps1 file; the .cmd wrapper invokes it.
        let ps1_path = hooks_dir.join("ooclaw-hook.ps1");
        let ps1_script = r#"$ErrorActionPreference = 'SilentlyContinue'
try {
    $inputData = [Console]::In.ReadToEnd() | ConvertFrom-Json
} catch {
    exit 0
}
if (-not $inputData) { exit 0 }

$hookEvent = $inputData.hook_event_name
if (-not $hookEvent) { $hookEvent = '' }

$statusMap = @{
    'UserPromptSubmit'  = 'processing'
    'PreCompact'        = 'compacting'
    'SessionStart'      = 'waiting_for_input'
    'SessionEnd'        = 'ended'
    'PreToolUse'        = 'running_tool'
    'PostToolUse'       = 'processing'
    'PermissionRequest' = 'waiting_for_input'
    'Stop'              = 'waiting_for_input'
    'SubagentStop'      = 'waiting_for_input'
}

$claudeStatus = $inputData.status
if (-not $claudeStatus) {
    $claudeStatus = $statusMap[$hookEvent]
    if (-not $claudeStatus) { $claudeStatus = 'unknown' }
}

$output = @{
    sessionId    = if ($inputData.session_id) { $inputData.session_id } else { '' }
    cwd          = if ($inputData.cwd) { $inputData.cwd } else { '' }
    event        = $hookEvent
    claudeStatus = $claudeStatus
    interactive  = $true
}

if ($hookEvent -eq 'UserPromptSubmit' -and $inputData.prompt) {
    $p = $inputData.prompt
    if ($p.Length -gt 200) { $p = $p.Substring(0, 200) }
    $output['userPrompt'] = $p
}

if ($inputData.tool_name) {
    $output['tool'] = $inputData.tool_name
}

if ($inputData.tool_input) {
    $ti = $inputData.tool_input | ConvertTo-Json -Compress
    if ($ti.Length -gt 200) { $ti = $ti.Substring(0, 200) }
    $output['toolInput'] = $ti
}

try {
    $json = $output | ConvertTo-Json -Compress
    $client = [System.Net.Sockets.TcpClient]::new()
    $client.SendTimeout = 2000
    $client.ReceiveTimeout = 2000
    $client.Connect('127.0.0.1', 19283)
    $stream = $client.GetStream()
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($json)
    $stream.Write($bytes, 0, $bytes.Length)
    $stream.Flush()
    $client.Close()
} catch {}
"#;
        std::fs::write(&ps1_path, ps1_script).map_err(|e| e.to_string())?;

        // The .cmd wrapper invokes the .ps1 script
        let hook_script = "@echo off\r\npowershell -NoProfile -ExecutionPolicy Bypass -File \"%~dp0ooclaw-hook.ps1\" 2>nul\r\n";
        std::fs::write(&hook_path, hook_script).map_err(|e| e.to_string())?;
    }

    // Update ~/.claude/settings.json to register hooks
    let settings_path = claude_dir.join("settings.json");
    let mut settings: serde_json::Value = if settings_path.exists() {
        let content = std::fs::read_to_string(&settings_path).map_err(|e| e.to_string())?;
        serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

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

    let has_our_hook = |entry: &serde_json::Value| -> bool {
        entry.get("command").and_then(|c| c.as_str()) == Some(&hook_path_str)
        || entry.get("hooks").and_then(|hs| hs.as_array()).map_or(false, |hs| {
            hs.iter().any(|inner| inner.get("command").and_then(|c| c.as_str()) == Some(&hook_path_str))
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

    Ok(())
}

/// Process a Claude hook event (shared logic between Unix socket and TCP server).
fn process_claude_event(
    buf: &str,
    state: &Arc<Mutex<HashMap<String, ClaudeSession>>>,
    app: &tauri::AppHandle,
) {
    if let Ok(event) = serde_json::from_str::<serde_json::Value>(buf) {
        let session_id = event.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if session_id.is_empty() { return; }

        let hook_event = event.get("event").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let claude_status = event.get("claudeStatus").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

        let is_processing = claude_status != "waiting_for_input";

        let user_prompt = event.get("userPrompt").and_then(|v| v.as_str()).unwrap_or("");
        let is_local_slash = if user_prompt.starts_with('/') {
            let cmd = user_prompt.split_whitespace().next().unwrap_or("");
            matches!(cmd, "/clear" | "/compact" | "/help" | "/cost" | "/status" | "/vim" | "/fast" | "/model" | "/login" | "/logout")
        } else { false };

        let mut status = match hook_event.as_str() {
            "UserPromptSubmit" => {
                if is_local_slash { "stopped".to_string() } else { "processing".to_string() }
            }
            "PreCompact" => "compacting".to_string(),
            "PreToolUse" => {
                let tool = event.get("tool").and_then(|v| v.as_str()).unwrap_or("");
                if tool == "AskUserQuestion" { "waiting".to_string() } else { "tool_running".to_string() }
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

        if !is_processing && matches!(status.as_str(), "processing" | "tool_running") {
            status = "stopped".to_string();
        }

        let was_processing;
        let was_compacting;

        {
            let mut sessions = state.lock().unwrap();
            let prev_status = sessions.get(&session_id).map(|s| s.status.clone()).unwrap_or_default();
            was_processing = matches!(prev_status.as_str(), "processing" | "tool_running" | "compacting");
            was_compacting = prev_status == "compacting";

            if hook_event == "SessionEnd" {
                sessions.remove(&session_id);
            } else {
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
                });

                session.status = status.clone();
                session.is_processing = is_processing;
                session.cwd = event.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
                session.interactive = event.get("interactive").and_then(|v| v.as_bool()).unwrap_or(true);
                session.updated_at = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_millis() as u64;

                if let Some(t) = event.get("tool").and_then(|v| v.as_str()) {
                    if !t.is_empty() { session.tool = Some(t.to_string()); }
                }
                if let Some(t) = event.get("toolInput").and_then(|v| v.as_str()) {
                    if !t.is_empty() { session.tool_input = Some(t.to_string()); }
                }
                if let Some(t) = event.get("userPrompt").and_then(|v| v.as_str()) {
                    if !t.is_empty() { session.user_prompt = Some(t.to_string()); }
                }

                if hook_event == "Stop" || hook_event == "SubagentStop" {
                    session.tool = None;
                    session.tool_input = None;
                }
            }
        }

        let _ = app.emit("claude-session-update", &session_id);

        if was_processing && !was_compacting && (status == "stopped" || status == "waiting" || status == "ended") {
            let is_waiting = status == "waiting";
            let _ = app.emit("claude-task-complete", serde_json::json!({"sessionId": session_id, "waiting": is_waiting}));
        }

        let cwd_str = event.get("cwd").and_then(|v| v.as_str()).unwrap_or("").to_string();
        if hook_event == "UserPromptSubmit" && !cwd_str.is_empty() {
            let jsonl_path = claude_session_file_path(&session_id, &cwd_str);
            if jsonl_path.exists() {
                start_session_file_watcher(
                    session_id.clone(),
                    jsonl_path,
                    state.clone(),
                    app.clone(),
                );
            }
        } else if hook_event == "Stop" || hook_event == "SubagentStop" || hook_event == "SessionEnd" {
            stop_session_file_watcher(&session_id);
        }
    }
}

/// Start the Claude IPC server.
/// On macOS/Linux: Unix domain socket at /tmp/ooclaw-claude.sock
/// On Windows: TCP server on localhost:19283
fn start_claude_socket_server(claude_state: Arc<Mutex<HashMap<String, ClaudeSession>>>, app_handle: tauri::AppHandle) {
    #[cfg(unix)]
    {
        let state = claude_state;
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
                    Ok(mut s) => {
                        let state = state.clone();
                        let app = app.clone();
                        std::thread::spawn(move || {
                            use std::io::Read;
                            let mut buf = String::new();
                            let _ = s.read_to_string(&mut buf);
                            process_claude_event(&buf, &state, &app);
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
                        std::thread::spawn(move || {
                            use std::io::Read;
                            let mut buf = String::new();
                            let _ = s.read_to_string(&mut buf);
                            process_claude_event(&buf, &state, &app);
                        });
                    }
                    Err(e) => { log::error!("Claude TCP accept error: {}", e); }
                }
            }
        });
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
            match std::fs::read(&file_path) {
                Ok(data) => {
                    let mime = if path.ends_with(".gif") { "image/gif" }
                        else if path.ends_with(".png") { "image/png" }
                        else { "application/octet-stream" };
                    tauri::http::Response::builder()
                        .header("Content-Type", mime)
                        .body(data)
                        .unwrap()
                }
                Err(_) => tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .register_uri_scheme_protocol("customasset", |ctx, req| {
            let path = req.uri().path();
            let data_dir = ctx.app_handle().path().app_data_dir().unwrap_or_default();
            let file_path = data_dir.join("characters").join(path.trim_start_matches('/'));
            match std::fs::read(&file_path) {
                Ok(data) => {
                    let mime = if path.ends_with(".gif") { "image/gif" }
                        else if path.ends_with(".png") { "image/png" }
                        else { "application/octet-stream" };
                    tauri::http::Response::builder()
                        .header("Content-Type", mime)
                        .body(data)
                        .unwrap()
                }
                Err(_) => tauri::http::Response::builder()
                    .status(404)
                    .body(Vec::new())
                    .unwrap(),
            }
        })
        .setup(|app| {
            // Fix PATH so openclaw (Node.js script) and node are both reachable
            fix_path();

            // Install Claude Code hooks on every startup (idempotent)
            if let Err(e) = tauri::async_runtime::block_on(install_claude_hooks()) {
                log::warn!("Failed to install Claude hooks on startup: {}", e);
            }

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

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
                            let win_w = 60.0;
                            let win_h = 45.0;
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

            // Start Claude Code socket server
            {
                let claude_state = app.state::<ClaudeState>();
                let sessions_arc = Arc::clone(&claude_state.sessions);
                start_claude_socket_server(sessions_arc, app.handle().clone());
            }

            // System tray
            let show = MenuItem::with_id(app, "show", "显示", true, None::<&str>)?;
            let hide = MenuItem::with_id(app, "hide", "隐藏", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "退出", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show, &hide, &quit])?;

            TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .on_menu_event(|app, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(win) = app.get_webview_window("mini") {
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
        .invoke_handler(tauri::generate_handler![get_status, send_chat, open_detail_panel, save_character_gif, delete_character_assets, delete_character_gif, get_agents, get_health, get_agent_metrics, interrupt_agent, scan_characters, get_agent_extra_info, open_mini, close_mini, set_mini_expanded, set_mini_size, resize_mini_height, move_mini_by, get_mini_origin, set_mini_origin, get_agent_sessions, get_session_preview, get_session_messages, get_active_sessions, proxy_post, play_sound, get_claude_sessions, get_claude_conversation, install_claude_hooks, remove_claude_session, get_claude_stats, open_url, check_for_update, run_update, close_ssh, read_local_file, list_backgrounds, save_background, get_background_data, exit_app, get_ssh_key_info, reset_ssh])
        .manage(ActiveAgentPid { pid: Mutex::new(None) })
        .manage(ClaudeState { sessions: Arc::new(Mutex::new(HashMap::new())) })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
