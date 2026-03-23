use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

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
fn fix_path() {
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
pub struct AgentHealth {
    #[serde(rename = "agentId")]
    pub agent_id: String,
    pub active: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct HealthResult {
    pub agents: Vec<AgentHealth>,
}

/// Returns the full set of open .jsonl file paths across all agents.
async fn lsof_open_jsonl_paths() -> std::collections::HashSet<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
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

/// Single `lsof +D` over the entire agents dir → set of active agent directory names.
/// A .jsonl being held open by a process = that agent is working.
async fn lsof_active_agents() -> std::collections::HashSet<String> {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let agents_dir = format!("{}/.openclaw/agents", home);
    let mut active = std::collections::HashSet::new();

    // Use full path for lsof (macOS: /usr/sbin/lsof)
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

/// Ensure an SSH ControlMaster socket is established (called once, reused by all ssh_exec).
async fn ensure_ssh_master(ssh_host: &str, ssh_user: &str) -> Result<(), String> {
    use std::sync::OnceLock;
    use tokio::sync::Mutex as TokioMutex;
    static LOCK: OnceLock<TokioMutex<()>> = OnceLock::new();
    let lock = LOCK.get_or_init(|| TokioMutex::new(()));
    let _guard = lock.lock().await;

    let control_path = format!("/tmp/oc-claw-ssh-{}@{}:22", ssh_user, ssh_host);
    if std::path::Path::new(&control_path).exists() { return Ok(()); }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let key_path = format!("{}/.ssh/id_ed25519", home);
    let target = format!("{}@{}", ssh_user, ssh_host);
    let cp = format!("ControlPath={}", control_path);
    let output = tokio::process::Command::new("ssh")
        .args([
            "-o", "StrictHostKeyChecking=no",
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=10",
            "-o", "ControlMaster=yes",
            "-o", &cp,
            "-o", "ControlPersist=300",
            "-i", &key_path,
            "-fN",
            &target,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("ssh master: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("SSH connection failed (check key auth): {}", stderr));
    }
    Ok(())
}

/// Execute a command on remote host via SSH, reusing ControlMaster socket.
async fn ssh_exec(ssh_host: &str, ssh_user: &str, cmd: &str) -> Result<String, String> {
    ensure_ssh_master(ssh_host, ssh_user).await?;
    let control_path = format!("/tmp/oc-claw-ssh-{}@{}:22", ssh_user, ssh_host);
    let target = format!("{}@{}", ssh_user, ssh_host);
    let cp = format!("ControlPath={}", control_path);
    let output = tokio::process::Command::new("ssh")
        .args([
            "-o", "BatchMode=yes",
            "-o", "ConnectTimeout=5",
            "-o", &cp,
            &target,
            cmd,
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await
        .map_err(|e| format!("ssh: {}", e))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ssh cmd failed: {}", stderr));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// Close an active SSH ControlMaster socket.
async fn close_ssh_master(ssh_host: &str, ssh_user: &str) -> Result<(), String> {
    let control_path = format!("/tmp/oc-claw-ssh-{}@{}:22", ssh_user, ssh_host);
    if !std::path::Path::new(&control_path).exists() { return Ok(()); }
    let target = format!("{}@{}", ssh_user, ssh_host);
    let cp = format!("ControlPath={}", control_path);
    let _ = tokio::process::Command::new("ssh")
        .args(["-o", &cp, "-O", "exit", &target])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .await;
    // Also remove stale socket file if it remains
    let _ = tokio::fs::remove_file(&control_path).await;
    log::info!("[close_ssh_master] closed socket for {}@{}", ssh_user, ssh_host);
    Ok(())
}

#[tauri::command]
async fn close_ssh(ssh_host: Option<String>, ssh_user: Option<String>) -> Result<(), String> {
    let sh = ssh_host.unwrap_or_default();
    let su = ssh_user.unwrap_or_default();
    if sh.is_empty() || su.is_empty() {
        // Try to close any oc-claw ssh sockets
        if let Ok(mut entries) = tokio::fs::read_dir("/tmp").await {
            while let Ok(Some(entry)) = entries.next_entry().await {
                let name = entry.file_name().to_string_lossy().to_string();
                if name.starts_with("oc-claw-ssh-") {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                    log::info!("[close_ssh] removed stale socket: {}", name);
                }
            }
        }
        return Ok(());
    }
    close_ssh_master(&sh, &su).await
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
    let mut last_role = "";
    let mut has_usage = false;
    for line in output.lines().rev() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(line) {
            if val["type"].as_str() == Some("message") {
                last_role = if val["message"]["role"].as_str() == Some("user") { "user" } else { "assistant" };
                has_usage = val["message"]["usage"].is_object();
                break;
            }
        }
    }
    // user message at the end = agent is processing; assistant with usage = done
    last_role == "user" || (last_role == "assistant" && !has_usage)
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
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let agent_dir = if agent_id.is_empty() { "main" } else { agent_id };
    PathBuf::from(home).join(format!(".openclaw/agents/{}/sessions/sessions.json", agent_dir))
}

#[tauri::command]
async fn get_status(_gateway_url: String, _token: String, agent_id: String) -> Result<GatewayStatus, String> {
    // Step 1: pgrep -x openclaw-gateway → check gateway is running
    let pgrep_gw = tokio::process::Command::new("pgrep")
        .args(["-x", "openclaw-gateway"])
        .output()
        .await
        .map_err(|e| format!("pgrep: {}", e))?;

    if !pgrep_gw.status.success() {
        return Err("gateway not running".into());
    }

    // Step 2: lsof check — is any .jsonl held open for this agent?
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

fn assets_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    // In dev mode, write to public/assets so vite can serve them.
    // In production, use resource_dir.
    if cfg!(debug_assertions) {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        // exe is in src-tauri/target/debug/ooclaw → go up to frontend/public/assets
        let project_root = exe
            .parent() // debug
            .and_then(|p| p.parent()) // target
            .and_then(|p| p.parent()) // src-tauri
            .and_then(|p| p.parent()) // frontend
            .ok_or("cannot resolve project root")?;
        Ok(project_root.join("public").join("assets"))
    } else {
        app.path().resource_dir().map(|p| p.join("assets")).map_err(|e| e.to_string())
    }
}

#[tauri::command]
async fn scan_characters(app: tauri::AppHandle) -> Result<Vec<serde_json::Value>, String> {
    let base = assets_dir(&app)?;
    let mut results = vec![];
    // In dev mode, use /assets/ (served by vite). In production, use localasset:// protocol.
    let url_prefix = if cfg!(debug_assertions) { "/assets" } else { "localasset://localhost" };

    let entries = std::fs::read_dir(&base).map_err(|e| e.to_string())?;
    for entry in entries.filter_map(|e| e.ok()) {
        if !entry.path().is_dir() { continue; }
        let name = entry.file_name().to_string_lossy().to_string();

        // Scan pet gifs
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

        // Scan mini gifs
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

    Ok(results)
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

    let base = assets_dir(&app)?;
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
    if name.contains("..") || name.contains('/') || name.contains('\\') || name == "keli" {
        return Err("invalid or protected name".into());
    }
    let base = assets_dir(&app)?;
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
    let base = assets_dir(&app)?;
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
            let dirs = ssh_exec(sh, su, "ls -1 $HOME/.openclaw/agents/ 2>/dev/null").await.unwrap_or_default();
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

    // === local mode (original) ===
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

#[tauri::command]
async fn get_health(mode: Option<String>, url: Option<String>, token: Option<String>, ssh_host: Option<String>, ssh_user: Option<String>) -> Result<HealthResult, String> {
    log::info!("[get_health] mode={:?} ssh_host={:?}", mode, ssh_host);
    if mode.as_deref() == Some("remote") {
        let sh = ssh_host.as_deref().unwrap_or("");
        let su = ssh_user.as_deref().unwrap_or("");
        if !sh.is_empty() && !su.is_empty() {
            let dirs_output = ssh_exec(sh, su, "ls -1 $HOME/.openclaw/agents/ 2>/dev/null").await.unwrap_or_default();
            let agent_ids: Vec<String> = dirs_output.lines()
                .filter(|l| !l.trim().is_empty())
                .map(|id| id.trim().to_string())
                .collect();
            let mut agents = Vec::new();
            for id in &agent_ids {
                let active = ssh_is_agent_active(sh, su, id).await;
                agents.push(AgentHealth { agent_id: id.clone(), active });
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
        let agents = agent_active.into_iter().map(|(agent_id, active)| AgentHealth { agent_id, active }).collect();
        return Ok(HealthResult { agents });
    }

    // === local mode (original) ===
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let agents_dir = std::path::PathBuf::from(&home).join(".openclaw/agents");

    let active_set = lsof_active_agents().await;

    let agents = std::fs::read_dir(&agents_dir)
        .map_err(|e| format!("read agents dir: {}", e))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|entry| {
            let agent_id = entry.file_name().to_string_lossy().to_string();
            let active = active_set.contains(&agent_id);
            AgentHealth { agent_id, active }
        })
        .collect();

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
    // Strategy 1: SIGINT the tracked openclaw agent subprocess (pet-window turns)
    let tracked_pid = *state.pid.lock().unwrap();
    if let Some(pid) = tracked_pid {
        let killed = unsafe { libc::kill(pid as i32, libc::SIGINT) == 0 };
        if killed {
            return Ok(format!("已向 openclaw agent 进程 (pid={}) 发送中断信号", pid));
        }
    }

    // Strategy 2: WebSocket chat.abort (channel-based turns like Feishu/Telegram)
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());

    // 1. Read gateway config
    let config_path = format!("{}/.openclaw/openclaw.json", home);
    let config_str = tokio::fs::read_to_string(&config_path).await
        .map_err(|e| format!("读取 openclaw.json 失败: {}", e))?;
    let config: serde_json::Value = serde_json::from_str(&config_str)
        .map_err(|e| format!("解析 openclaw.json 失败: {}", e))?;
    let port = config["gateway"]["port"].as_u64().unwrap_or(18789) as u16;
    let token = config["gateway"]["auth"]["token"].as_str().unwrap_or("").to_string();
    if token.is_empty() {
        return Err("openclaw.json 中未找到 gateway token".into());
    }

    // 2. Find the ACTIVE session key by checking which .jsonl file is currently open (lsof).
    //    This is more reliable than using updatedAt, because multiple sessions may exist
    //    (e.g. Feishu session vs pet-window session) and we need the one with a live run.
    let sess_path = sessions_json_path(&agent_id);
    let content = tokio::fs::read_to_string(&sess_path).await
        .map_err(|e| format!("读取 sessions.json 失败: {}", e))?;
    let sess_map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    // Use lsof to find which .jsonl file is currently held open (active run)
    let lsof_bin = if std::path::Path::new("/usr/sbin/lsof").exists() { "/usr/sbin/lsof" } else { "lsof" };
    let agent_dir_name = if agent_id.is_empty() { "main" } else { &agent_id };
    let search_path = format!("{}/.openclaw/agents/{}", home, agent_dir_name);
    let lsof_stdout_owned = tokio::process::Command::new(lsof_bin)
        .args(["+D", &search_path])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    let lsof_stdout = lsof_stdout_owned.as_str();

    // Collect open .jsonl file paths from node processes
    let open_jsonl_paths: std::collections::HashSet<String> = lsof_stdout.lines()
        .filter(|l| l.contains(".jsonl") && l.split_whitespace().next().map(|c| c.starts_with("node")).unwrap_or(false))
        .filter_map(|l| l.split_whitespace().last().map(|s| s.to_string()))
        .collect();

    // Match open .jsonl paths against sessionFile entries in sessions.json
    let session_key = sess_map.iter()
        .find(|(_, v)| {
            if let Some(sf) = v["sessionFile"].as_str() {
                // sessionFile may be exact path or may contain the uuid; check if any open path starts with or equals it
                open_jsonl_paths.iter().any(|p| p.starts_with(sf) || sf.starts_with(p.as_str()))
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
            let mut daily_calls: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
            let mut daily_tokens: std::collections::HashMap<String, u64> = std::collections::HashMap::new();
            let cat_cmd = format!("cat $HOME/.openclaw/agents/{}/sessions/*.jsonl 2>/dev/null", agent_dir);
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
            use chrono::{Local, Duration};
            let today = Local::now().date_naive();
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

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
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
                    let screen_frame: Option<(f64, f64, f64, f64)> = unsafe {
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
                        Some((frame.origin.x, frame.origin.y, frame.size.width, frame.size.height))
                    };
                    if let Some((sx, sy, sw, sh)) = screen_frame {
                        let win_w = 60.0;
                        let win_h = 45.0;
                        let x = sx + sw / 2.0 + 80.0;
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
        #[cfg(not(target_os = "macos"))]
        {
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

                // Elevate window level above menu bar first (NSMainMenuWindowLevel=24, +3=27)
                unsafe {
                    let _: () = msg_send![obj, setLevel: 27isize];
                    let behavior: usize = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 6);
                    let _: () = msg_send![obj, setCollectionBehavior: behavior];
                }

                // Get screen frame and position window directly via NSWindow setFrame
                let screen_frame: Option<(f64, f64, f64, f64)> = unsafe {
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
                    Some((frame.origin.x, frame.origin.y, frame.size.width, frame.size.height))
                };

                if let Some((sx, sy, sw, sh)) = screen_frame {
                    // Start collapsed: small window right of notch
                    let win_w = 60.0;
                    let win_h = 45.0;
                    let x = sx + sw / 2.0 + 80.0;
                    let y = sy + sh - win_h;
                    let frame = NSRect::new(
                        NSPoint::new(x, y),
                        NSSize::new(win_w, win_h),
                    );
                    unsafe {
                        let _: () = msg_send![obj, setFrame: frame, display: true];
                    }
                }

                // Show after positioning
                unsafe {
                    let _: () = msg_send![obj, orderFrontRegardless];
                }
            }
        });
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

/// Resize/reposition the mini window between collapsed (small, right of notch)
/// and expanded (larger, centered on notch) states.
#[tauri::command]
async fn set_mini_expanded(app: tauri::AppHandle, expanded: bool) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;

    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        let _ = app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};

            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };

                let screen_frame: Option<(f64, f64, f64, f64)> = unsafe {
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
                    Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height))
                };

                if let Some((sx, sy, sw, sh)) = screen_frame {
                    unsafe {
                        let _: () = msg_send![obj, setLevel: 27isize];
                    }
                    if expanded {
                        let win_w = 400.0;
                        let win_h = 560.0;
                        let x = sx + (sw - win_w) / 2.0;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                    } else {
                        let win_w = 60.0;
                        let win_h = 45.0;
                        let x = sx + sw / 2.0 + 80.0;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                    }
                }
            }
        });
    }

    Ok(())
}

/// Resize the mini window to 3/4 of screen, centered, with normal window level.
/// Used for settings panel mode. Pass `restore: true` to go back to mini mode.
#[tauri::command]
async fn set_mini_size(app: tauri::AppHandle, restore: bool) -> Result<(), String> {
    let win = app.get_webview_window("mini").ok_or("mini window not found")?;

    #[cfg(target_os = "macos")]
    {
        let win_clone = win.clone();
        let _ = app.run_on_main_thread(move || {
            use objc2::runtime::{AnyClass, AnyObject};
            use objc2::msg_send;
            use objc2_foundation::{NSRect, NSPoint, NSSize};

            if let Ok(ns_win) = win_clone.ns_window() {
                let obj = unsafe { &*(ns_win as *mut AnyObject) };

                let screen_frame: Option<(f64, f64, f64, f64)> = unsafe {
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
                    Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height))
                };

                if let Some((sx, sy, sw, sh)) = screen_frame {
                    if restore {
                        // Restore to collapsed: small window right of notch
                        let win_w = 60.0;
                        let win_h = 45.0;
                        let x = sx + sw / 2.0 + 80.0;
                        let y = sy + sh - win_h;
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setLevel: 27isize];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                    } else {
                        // Expand to 3/4 screen, centered, no native animation (CSS handles visuals)
                        let win_w = (sw * 0.75).round();
                        let win_h = (sh * 0.75).round();
                        let x = sx + (sw - win_w) / 2.0;
                        let y = sy + sh - win_h; // top of screen (macOS y=0 is bottom)
                        let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(win_w, win_h));
                        unsafe {
                            let _: () = msg_send![obj, setLevel: 0isize];
                            let _: () = msg_send![obj, setFrame: frame, display: true, animate: false];
                        }
                    }
                }
            }
        });
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
    if s.contains("Conversation info (untrusted metadata)") || s.contains("[message_id:") {
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
            // SSH-based: full local-like session list with messages
            let sess_path = remote_sessions_json_path(&agent_id);
            log::info!("[get_agent_sessions] SSH reading: {}", sess_path);
            let content = match ssh_read_file(sh, su, &sess_path).await {
                Ok(c) => { log::info!("[get_agent_sessions] SSH read OK, len={}", c.len()); c }
                Err(e) => { log::error!("[get_agent_sessions] SSH read failed: {}", e); return Err(format!("read remote sessions.json: {}", e)); }
            };
            let map: serde_json::Map<String, serde_json::Value> =
                serde_json::from_str(&content).map_err(|e| { log::error!("[get_agent_sessions] parse failed: {}", e); e.to_string() })?;
            log::info!("[get_agent_sessions] sessions.json keys: {:?}", map.keys().collect::<Vec<_>>());

            let mut sessions: Vec<MiniSessionInfo> = Vec::new();
            for (key, val) in map.iter() {
                let session_file = val["sessionFile"].as_str().unwrap_or("").to_string();
                if session_file.is_empty() { log::info!("[get_agent_sessions] skip key={} no sessionFile", key); continue; }

                let is_active = ssh_is_session_file_active(sh, su, &session_file).await;

                let (last_user, last_assistant) = match ssh_read_file(sh, su, &session_file).await {
                    Ok(c) => { log::info!("[get_agent_sessions] read session file OK, len={}", c.len()); extract_last_messages(&c) }
                    Err(e) => { log::error!("[get_agent_sessions] read session file failed: {} path={}", e, session_file); (None, None) }
                };

                log::info!("[get_agent_sessions] key={} last_user={:?} last_assistant={:?}", key, last_user.is_some(), last_assistant.is_some());
                if last_user.is_none() && last_assistant.is_none() { continue; }
                if key.contains(":cron:") { continue; }

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
                });
            }
            log::info!("[get_agent_sessions] SSH result: {} sessions", sessions.len());
            sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            sessions.truncate(20);
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
            })
        }).collect();
        sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
        sessions.truncate(20);
        return Ok(sessions);
    }

    // === local mode (original) ===
    let path = sessions_json_path(&agent_id);
    let content = tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("read sessions.json: {}", e))?;
    let map: serde_json::Map<String, serde_json::Value> =
        serde_json::from_str(&content).map_err(|e| e.to_string())?;

    // Check which sessions are active via lsof
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let agent_dir = if agent_id.is_empty() { "main" } else { &agent_id };
    let search_path = format!("{}/.openclaw/agents/{}", home, agent_dir);

    let lsof_bin = if std::path::Path::new("/usr/sbin/lsof").exists() { "/usr/sbin/lsof" } else { "lsof" };
    let lsof_stdout = tokio::process::Command::new(lsof_bin)
        .args(["+D", &search_path])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output().await
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let open_jsonl: std::collections::HashSet<String> = lsof_stdout.lines()
        .filter(|l| l.contains(".jsonl"))
        .filter_map(|l| l.split_whitespace().last().map(|s| s.to_string()))
        .collect();

    let mut sessions: Vec<MiniSessionInfo> = Vec::new();
    for (key, val) in map.iter() {
        let session_file_raw = val["sessionFile"].as_str().unwrap_or("").to_string();
        let session_id_str = val["sessionId"].as_str().unwrap_or(key.as_str());

        // If sessionFile is empty, try to infer from sessionId
        let session_file = if !session_file_raw.is_empty() {
            session_file_raw
        } else if !session_id_str.is_empty() {
            let sessions_dir = format!("{}/.openclaw/agents/{}/sessions", home, agent_dir);
            format!("{}/{}.jsonl", sessions_dir, session_id_str)
        } else {
            String::new()
        };

        let is_active = if !session_file.is_empty() {
            open_jsonl.iter().any(|p| p.starts_with(&session_file) || session_file.starts_with(p.as_str()))
        } else { false };

        // Read last messages from .jsonl
        let (last_user, last_assistant) = if !session_file.is_empty() {
            match tokio::fs::read_to_string(&session_file).await {
                Ok(c) => extract_last_messages(&c),
                Err(_) => (None, None),
            }
        } else {
            (None, None)
        };

        // Skip sessions with no messages or cron task sessions
        if last_user.is_none() && last_assistant.is_none() { continue; }
        if key.contains(":cron:") { continue; }

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
        });
    }

    sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    sessions.truncate(20);
    Ok(sessions)
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
            let dirs_output = ssh_exec(sh, su, "ls -1 $HOME/.openclaw/agents/ 2>/dev/null").await.unwrap_or_default();
            let mut active_keys: Vec<String> = vec![];
            for agent_id in dirs_output.lines().filter(|l| !l.trim().is_empty()) {
                let agent_id = agent_id.trim();
                let sess_path = remote_sessions_json_path(agent_id);
                let Ok(content) = ssh_read_file(sh, su, &sess_path).await else { continue };
                let Ok(map): Result<serde_json::Map<String, serde_json::Value>, _> = serde_json::from_str(&content) else { continue };
                for (key, val) in map.iter() {
                    let session_file = val["sessionFile"].as_str().unwrap_or("");
                    if session_file.is_empty() { continue; }
                    if ssh_is_session_file_active(sh, su, session_file).await {
                        active_keys.push(format!("{}:{}", agent_id, key));
                    }
                }
            }
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

    // === local mode (original) ===
    let open_paths = lsof_open_jsonl_paths().await;
    if open_paths.is_empty() { return Ok(vec![]); }

    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let agents_dir = std::path::PathBuf::from(&home).join(".openclaw/agents");
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

            if open_paths.iter().any(|p| p.starts_with(&file_path) || file_path.starts_with(p.as_str())) {
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

// ─── Play macOS system sound ───
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
    Ok(())
}

// ─── Claude Code session state ───
use std::sync::Arc;
use std::collections::HashMap;

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
    let project_dir = cwd.replace('/', "-").replace('.', "-");
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
                let file_truncated = new_size < *prev;
                *prev = new_size;

                let mut sessions_guard = sessions2.lock().unwrap();
                let session = match sessions_guard.get_mut(&sid2) {
                    Some(s) => s,
                    None => return,
                };

                let mut changed = false;

                // Compact detection: file was rewritten (truncated) while in compacting state
                if session.status == "compacting" && file_truncated {
                    log::info!("File watcher: compact done for session {}, file truncated", sid2);
                    session.status = "stopped".to_string();
                    session.is_processing = false;
                    changed = true;
                }

                // Interruption detection: working but file shows interrupted
                if !changed && (session.status == "processing" || session.status == "tool_running") {
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
                    let _ = app2.emit("claude-task-complete", &sid2);
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
    let mut list: Vec<ClaudeSession> = sessions.values().cloned().collect();
    list.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(list)
}

#[tauri::command]
async fn remove_claude_session(session_id: String, state: tauri::State<'_, ClaudeState>) -> Result<(), String> {
    let mut sessions = state.sessions.lock().map_err(|e| e.to_string())?;
    sessions.remove(&session_id);
    Ok(())
}

#[tauri::command]
async fn open_url(url: String) -> Result<(), String> {
    std::process::Command::new("open")
        .arg(&url)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn check_for_update(app: tauri::AppHandle) -> Result<serde_json::Value, String> {
    let current = app.config().version.clone().unwrap_or_default();
    let client = reqwest::Client::builder()
        .user_agent("oc-claw")
        .build()
        .map_err(|e| e.to_string())?;
    let resp = client
        .get("https://api.github.com/repos/rainnoon/oc-claw/releases/latest")
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let json: serde_json::Value = resp.json().await.map_err(|e| e.to_string())?;
    let latest = json["tag_name"].as_str().unwrap_or("").trim_start_matches('v');
    let has_update = version_cmp(latest, &current);
    Ok(serde_json::json!({
        "current": current,
        "latest": latest,
        "hasUpdate": has_update,
        "url": json["html_url"].as_str().unwrap_or(""),
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

#[tauri::command]
async fn run_update() -> Result<(), String> {
    let script = r#"
        set -e
        REPO="rainnoon/oc-claw"
        APP_NAME="oc-claw"
        INSTALL_DIR="/Applications"
        DMG_URL=$(curl -sL "https://api.github.com/repos/${REPO}/releases/latest" \
          | grep "browser_download_url.*\.dmg" \
          | head -1 \
          | cut -d '"' -f 4)
        [ -z "$DMG_URL" ] && exit 1
        TMPDIR=$(mktemp -d)
        DMG_PATH="${TMPDIR}/${APP_NAME}.dmg"
        curl -sL "$DMG_URL" -o "$DMG_PATH"
        MOUNT_POINT=$(hdiutil attach "$DMG_PATH" -nobrowse -quiet | tail -1 | sed 's/.*\(\/Volumes\/.*\)/\1/' | xargs)
        rm -rf "${INSTALL_DIR}/${APP_NAME}.app"
        cp -R "${MOUNT_POINT}/${APP_NAME}.app" "${INSTALL_DIR}/"
        hdiutil detach "$MOUNT_POINT" -quiet
        xattr -cr "${INSTALL_DIR}/${APP_NAME}.app"
        rm -rf "$TMPDIR"
        open "${INSTALL_DIR}/${APP_NAME}.app"
    "#;
    tokio::process::Command::new("bash")
        .args(["-c", script])
        .spawn()
        .map_err(|e| e.to_string())?;
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

    // Write hook script
    let hook_script = r#"#!/bin/bash
# ooclaw Claude Code hook - forwards events to /tmp/ooclaw-claude.sock
SOCKET_PATH="/tmp/ooclaw-claude.sock"
[ -S "$SOCKET_PATH" ] || exit 0

/usr/bin/python3 -c "
import json, socket, sys

try:
    input_data = json.load(sys.stdin)
except:
    sys.exit(0)

hook_event = input_data.get('hook_event_name', '')

output = {
    'sessionId': input_data.get('session_id', ''),
    'cwd': input_data.get('cwd', ''),
    'event': hook_event,
    'claudeStatus': input_data.get('status', 'unknown'),
    'interactive': True,
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

    let hook_path = hooks_dir.join("ooclaw-hook.sh");
    std::fs::write(&hook_path, hook_script).map_err(|e| e.to_string())?;

    // chmod +x
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
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

    // Hook registration configs matching notchi's HookInstaller approach:
    // - PreToolUse/PostToolUse/PermissionRequest: matcher "*" (match all tools)
    // - PreCompact: matcher "auto" + "manual" (both compact types)
    // - Others: no matcher
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
        let already = arr.iter().any(|h| has_our_hook(h));
        if !already {
            for config in configs {
                arr.push(config.clone());
            }
        }
    }

    std::fs::write(&settings_path, serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;

    Ok(())
}

fn start_claude_socket_server(claude_state: Arc<Mutex<HashMap<String, ClaudeSession>>>, app_handle: tauri::AppHandle) {
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
                    let state = claude_state.clone();
                    let app = app_handle.clone();
                    std::thread::spawn(move || {
                        use std::io::Read;
                        let mut buf = String::new();
                        let _ = s.read_to_string(&mut buf);
                        if let Ok(event) = serde_json::from_str::<serde_json::Value>(&buf) {
                            let session_id = event.get("sessionId").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            if session_id.is_empty() { return; }

                            let hook_event = event.get("event").and_then(|v| v.as_str()).unwrap_or("").to_string();
                            let claude_status = event.get("claudeStatus").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();

                            // Matching notchi's SessionStore.process:
                            // isProcessing is derived from Claude's own status field
                            let is_processing = claude_status != "waiting_for_input";

                            // Event-based task determination (matching notchi's SessionStore.process)
                            let mut status = match hook_event.as_str() {
                                "UserPromptSubmit" => "processing".to_string(),
                                "PreCompact" => "compacting".to_string(),
                                "PreToolUse" => "tool_running".to_string(),
                                "PostToolUse" => "processing".to_string(),
                                "Stop" | "SubagentStop" => "stopped".to_string(),
                                "SessionEnd" => "ended".to_string(),
                                "PermissionRequest" => "waiting".to_string(),
                                "SessionStart" => {
                                    if is_processing { "processing".to_string() } else { "stopped".to_string() }
                                }
                                _ => {
                                    if !is_processing { "stopped".to_string() } else { claude_status.clone() }
                                }
                            };

                            // Notchi default: if Claude reports waiting_for_input,
                            // any active task should be overridden to idle
                            if !is_processing && matches!(status.as_str(), "compacting" | "processing" | "tool_running") {
                                status = "stopped".to_string();
                            }

                            let was_processing;

                            {
                                let mut sessions = state.lock().unwrap();
                                was_processing = sessions.get(&session_id)
                                    .map(|s| s.is_processing)
                                    .unwrap_or(false);

                                // SessionEnd: remove session entirely
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

                                    // Clear tool info on Stop (task finished)
                                    if hook_event == "Stop" || hook_event == "SubagentStop" {
                                        session.tool = None;
                                        session.tool_input = None;
                                    }
                                }
                            }

                            // Emit event to frontend
                            let _ = app.emit("claude-session-update", &session_id);

                            // If transitioned from processing to stopped/waiting, emit completion
                            if was_processing && (status == "stopped" || status == "waiting" || status == "ended") {
                                let _ = app.emit("claude-task-complete", &session_id);
                            }

                            // File watcher: start on UserPromptSubmit, stop on Stop/SessionEnd
                            // (matching notchi's NotchiStateMachine approach)
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
                    });
                }
                Err(e) => { log::error!("Claude socket accept error: {}", e); }
            }
        }
    });
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_store::Builder::default().build())
        .register_uri_scheme_protocol("localasset", |ctx, req| {
            let path = req.uri().path();
            // path is like /charname/mini/top/work.gif
            let resource_dir = ctx.app_handle().path().resource_dir().unwrap_or_default();
            let file_path = resource_dir.join("assets").join(path.trim_start_matches('/'));
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

            if cfg!(debug_assertions) {
                app.handle().plugin(
                    tauri_plugin_log::Builder::default()
                        .level(log::LevelFilter::Info)
                        .build(),
                )?;
            }

            // Hide from Dock, show only in menu bar
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

            // Position mini window to right of notch (collapsed state)
            #[cfg(target_os = "macos")]
            if let Some(win) = app.get_webview_window("mini") {
                let win_clone = win.clone();
                let _ = app.handle().run_on_main_thread(move || {
                    use objc2::runtime::{AnyClass, AnyObject};
                    use objc2::msg_send;
                    use objc2_foundation::{NSRect, NSPoint, NSSize};

                    if let Ok(ns_win) = win_clone.ns_window() {
                        let obj = unsafe { &*(ns_win as *mut AnyObject) };

                        // Elevate window level above menu bar
                        unsafe {
                            let _: () = msg_send![obj, setLevel: 27isize];
                            let behavior: usize = (1 << 0) | (1 << 4) | (1 << 8) | (1 << 6);
                            let _: () = msg_send![obj, setCollectionBehavior: behavior];
                        }

                        let screen_frame: Option<(f64, f64, f64, f64)> = unsafe {
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
                            Some((sf.origin.x, sf.origin.y, sf.size.width, sf.size.height))
                        };

                        if let Some((sx, sy, sw, sh)) = screen_frame {
                            let win_w = 60.0;
                            let win_h = 45.0;
                            let x = sx + sw / 2.0 + 80.0;
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
        .invoke_handler(tauri::generate_handler![get_status, send_chat, open_detail_panel, save_character_gif, delete_character_assets, delete_character_gif, get_agents, get_health, get_agent_metrics, interrupt_agent, scan_characters, get_agent_extra_info, open_mini, close_mini, set_mini_expanded, set_mini_size, get_agent_sessions, get_session_messages, get_active_sessions, proxy_post, play_sound, get_claude_sessions, get_claude_conversation, install_claude_hooks, remove_claude_session, open_url, check_for_update, run_update, close_ssh])
        .manage(ActiveAgentPid { pid: Mutex::new(None) })
        .manage(ClaudeState { sessions: Arc::new(Mutex::new(HashMap::new())) })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
