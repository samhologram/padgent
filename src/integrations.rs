//! Things that touch the rest of the Mac: opening a session, Claude Code hooks and model
//! roles, launch at login.

use crate::model::AgentDef;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::process::Command;

/// Opens the exact conversation: the followed app's conversation (through Accessibility),
/// the terminal tab the agent runs in, else the agent's link or app.
pub fn open_exact(agent: Option<&AgentDef>, s: &crate::model::Session) {
    if !s.app_bundle.is_empty() {
        let (bundle, title) = (s.app_bundle.clone(), s.title.clone());
        let electron = agent.map(|a| a.electron).unwrap_or(false) || bundle.contains("claude");
        std::thread::spawn(move || crate::watch::focus_conversation(&bundle, &title, electron));
        return;
    }
    if !s.tty.is_empty() && focus_terminal(&s.term, &s.tty) {
        return;
    }
    open_session(agent, &s.id);
}

fn osascript(script: &str) -> bool {
    Command::new("/usr/bin/osascript").args(["-e", script]).output().map(|o| o.status.success()).unwrap_or(false)
}

/// Brings the terminal tab whose device is `tty` to the front (Terminal and iTerm2 by
/// Apple Events, needs Automation consent); other terminals are brought forward.
pub fn focus_terminal(term: &str, tty: &str) -> bool {
    let tty = tty.replace('"', "");
    match term {
        "Apple_Terminal" | "" => {
            let script = format!(
                r#"tell application "Terminal"
  repeat with w in windows
    repeat with t in tabs of w
      if tty of t is "{tty}" then
        set selected of t to true
        set index of w to 1
        activate
        return
      end if
    end repeat
  end repeat
end tell
error "no tab""#
            );
            osascript(&script)
        }
        "iTerm.app" => {
            let script = format!(
                r#"tell application "iTerm2"
  repeat with w in windows
    repeat with t in tabs of w
      repeat with s in sessions of t
        if tty of s is "{tty}" then
          select w
          select t
          select s
          activate
          return
        end if
      end repeat
    end repeat
  end repeat
end tell
error "no session""#
            );
            osascript(&script)
        }
        other => {
            let app = match other {
                "vscode" => "Visual Studio Code",
                "ghostty" => "Ghostty",
                "WarpTerminal" => "Warp",
                "WezTerm" => "WezTerm",
                "Hyper" => "Hyper",
                "tmux" => "Terminal",
                x => x,
            };
            Command::new("open").args(["-a", app]).status().map(|s| s.success()).unwrap_or(false)
        }
    }
}

/// Older hooks didn't send the terminal; refresh our own entries so taps can find the tab.
pub fn upgrade_claude_hooks(port: u16) {
    let Ok(v) = read_json(&claude_settings_path()) else { return };
    let text = v.to_string();
    if text.contains("/hook/claude-code") && !text.contains("X-TTY") {
        let _ = install_claude_hooks(port);
    }
}

pub fn open_session(agent: Option<&AgentDef>, session: &str) {
    let Some(a) = agent else { return };
    if !a.open_url.trim().is_empty() {
        let url = a.open_url.replace("{session}", session);
        if is_safe_link(&url) {
            let _ = Command::new("open").arg(url).spawn();
            return;
        }
    }
    if !a.app_name.trim().is_empty() {
        let _ = Command::new("open").args(["-a", a.app_name.trim()]).spawn();
    }
}

pub fn is_safe_link(url: &str) -> bool {
    let l = url.trim().to_ascii_lowercase();
    !(l.starts_with("file:") || l.starts_with("javascript:") || l.starts_with("data:")) && l.contains(':')
}

pub fn open_url(url: &str) {
    if is_safe_link(url) {
        let _ = Command::new("open").arg(url.trim()).spawn();
    }
}

/// The command an agent's hooks run to report to this app.
pub fn hook_command(agent_id: &str, port: u16) -> String {
    format!(
        "curl -s -m 2 -X POST -H 'Content-Type: application/json' -H \"X-TTY: $(ps -o tty= -p $$ 2>/dev/null)\" -H \"X-Term: $TERM_PROGRAM\" --data-binary @- http://127.0.0.1:{port}/hook/{agent_id} >/dev/null 2>&1 || true"
    )
}

pub fn example_event(agent_id: &str, port: u16) -> String {
    format!(
        "curl -s -X POST http://127.0.0.1:{port}/event/{agent_id} -d '{{\"session\":\"demo\",\"state\":\"thinking\",\"title\":\"My task\",\"text\":\"Working on it\"}}'"
    )
}

fn claude_settings_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".claude").join("settings.json")
}

fn read_json(path: &PathBuf) -> Result<Value, String> {
    match std::fs::read_to_string(path) {
        Ok(t) if t.trim().is_empty() => Ok(json!({})),
        Ok(t) => serde_json::from_str(&t).map_err(|_| format!("{} isn't valid JSON, so it was left unchanged.", path.display())),
        Err(_) => Ok(json!({})),
    }
}

fn write_json(path: &PathBuf, v: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let bak = path.with_extension("json.legacy-rs-bak");
    if path.exists() && !bak.exists() {
        let _ = std::fs::copy(path, &bak);
    }
    let tmp = path.with_extension("json.legacy-rs-tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

const CLAUDE_EVENTS: [&str; 10] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "PreCompact",
    "Stop",
    "SubagentStop",
    "SessionEnd",
    "PermissionRequest",
];

fn is_ours(cmd: &str, agent: &str) -> bool {
    cmd.contains("127.0.0.1:") && cmd.contains(&format!("/hook/{agent}"))
}

pub fn claude_hooks_installed() -> bool {
    let Ok(v) = read_json(&claude_settings_path()) else { return false };
    v.get("hooks").and_then(Value::as_object).map(|h| {
        h.values().any(|list| list.to_string().contains("/hook/claude-code"))
    }).unwrap_or(false)
}

/// Adds this app's status hooks to ~/.claude/settings.json (other hooks are kept;
/// the first original is backed up beside it).
pub fn install_claude_hooks(port: u16) -> Result<String, String> {
    let path = claude_settings_path();
    let mut v = read_json(&path)?;
    remove_hooks_from(&mut v, "claude-code");
    let obj = v.as_object_mut().ok_or("settings.json isn't an object")?;
    let hooks = obj.entry("hooks").or_insert_with(|| json!({}));
    let hooks = hooks.as_object_mut().ok_or("“hooks” isn't an object")?;
    for ev in CLAUDE_EVENTS {
        let list = hooks.entry(ev).or_insert_with(|| json!([]));
        if let Some(arr) = list.as_array_mut() {
            let mut entry = Map::new();
            if matches!(ev, "PreToolUse" | "PostToolUse" | "PermissionRequest") {
                entry.insert("matcher".into(), json!("*"));
            }
            entry.insert("hooks".into(), json!([{ "type": "command", "command": hook_command("claude-code", port), "timeout": 3 }]));
            arr.push(Value::Object(entry));
        }
    }
    write_json(&path, &v)?;
    Ok("Hooks added to Claude Code. Start a new Claude Code session and it gets a key.".into())
}

fn remove_hooks_from(v: &mut Value, agent: &str) {
    if let Some(hooks) = v.get_mut("hooks").and_then(Value::as_object_mut) {
        for list in hooks.values_mut() {
            if let Some(arr) = list.as_array_mut() {
                for entry in arr.iter_mut() {
                    if let Some(inner) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
                        inner.retain(|h| !h.get("command").and_then(Value::as_str).map(|c| is_ours(c, agent)).unwrap_or(false));
                    }
                }
                arr.retain(|e| e.get("hooks").and_then(Value::as_array).map(|a| !a.is_empty()).unwrap_or(true));
            }
        }
        hooks.retain(|_, l| l.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }
}

pub fn remove_claude_hooks() -> Result<String, String> {
    let path = claude_settings_path();
    let mut v = read_json(&path)?;
    remove_hooks_from(&mut v, "claude-code");
    write_json(&path, &v)?;
    Ok("Hooks removed from Claude Code. Your other hooks were kept.".into())
}

/// Haiku for the swarm (sub-agents), Sonnet for code, Opus for orchestration.
pub fn apply_claude_models(roles: &std::collections::BTreeMap<String, String>) -> Result<String, String> {
    let path = claude_settings_path();
    let mut v = read_json(&path)?;
    let obj = v.as_object_mut().ok_or("settings.json isn't an object")?;
    let get = |k: &str| roles.get(k).cloned().unwrap_or_default();
    if !get("orchestration").is_empty() {
        obj.insert("model".into(), json!(get("orchestration")));
    }
    let env = obj.entry("env").or_insert_with(|| json!({}));
    if let Some(env) = env.as_object_mut() {
        if !get("swarm").is_empty() {
            env.insert("CLAUDE_CODE_SUBAGENT_MODEL".into(), json!(get("swarm")));
            env.insert("ANTHROPIC_DEFAULT_HAIKU_MODEL".into(), json!(get("swarm")));
        }
        if !get("code").is_empty() {
            env.insert("ANTHROPIC_DEFAULT_SONNET_MODEL".into(), json!(get("code")));
        }
        if !get("orchestration").is_empty() {
            env.insert("ANTHROPIC_DEFAULT_OPUS_MODEL".into(), json!(get("orchestration")));
        }
    }
    write_json(&path, &v)?;
    Ok("Claude Code now uses these models: orchestration as the main model, the swarm model for sub-agents, the code model for Sonnet tasks.".into())
}

fn launch_agent_path() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join("Library/LaunchAgents/com.agentpad.app.plist")
}

pub fn set_launch_at_login(on: bool) -> Result<(), String> {
    let path = launch_agent_path();
    if !on {
        let _ = std::fs::remove_file(&path);
        return Ok(());
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let exe = exe.to_string_lossy().replace('&', "&amp;").replace('<', "&lt;");
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>com.agentpad.app</string>
<key>ProgramArguments</key><array><string>{exe}</string></array>
<key>RunAtLoad</key><true/>
</dict></plist>
"#
    );
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, plist).map_err(|e| e.to_string())
}
