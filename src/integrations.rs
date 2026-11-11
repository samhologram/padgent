//! Things that touch the rest of the Mac: opening a session, Claude Code hooks and model
//! roles, launch at login.

use crate::model::AgentDef;
use serde_json::{Map, Value, json};
use std::path::PathBuf;
use std::process::Command;

/// Opens the exact conversation and puts its app in front, the way legacy does: activate the
/// app (HostAppActivator.bringForward), then select the conversation (Accessibility) or the
/// terminal tab (Apple Events). Call on the main thread.
pub fn open_exact(agent: Option<&AgentDef>, s: &crate::model::Session) {
    crate::mac::log(&format!(
        "open {}:{} app={:?} pid={} tty={:?} term={:?}",
        s.agent, s.id, s.app_bundle, s.pid, s.tty, s.term
    ));
    // A Codex thread: codex://threads/<id> (CodexAppLink.openURL 0x1006174d0).
    if s.agent == "codex" {
        if let Some(url) = codex_thread_url(&s.id) {
            crate::mac::log(&format!("codex link {url}"));
            let _ = Command::new("/usr/bin/open").arg(&url).spawn();
            if let Some(pid) = crate::mac::pid_for_bundle("com.openai.codex") {
                crate::mac::bring_forward(pid);
            }
            return;
        }
    }
    // A followed desktop app: bring it forward, then open the conversation inside it.
    if !s.app_bundle.is_empty() {
        let (bundle, title) = (s.app_bundle.clone(), s.title.clone());
        let electron = agent.map(|a| a.electron).unwrap_or(false) || bundle.contains("claude");
        let running = crate::mac::pid_for_bundle(&bundle);
        match running {
            Some(pid) => {
                crate::mac::bring_forward(pid);
            }
            None => {
                let _ = Command::new("/usr/bin/open").args(["-b", &bundle]).spawn();
            }
        }
        std::thread::spawn(move || {
            if running.is_none() {
                std::thread::sleep(std::time::Duration::from_millis(1200));
            }
            let found = crate::watch::focus_conversation(&bundle, &title, electron);
            crate::mac::log(&format!("conversation {title:?} in {bundle}: {found}"));
            if found {
                // Selecting the conversation can leave another window in front: activate again.
                crate::mac::on_main(Box::new(move || {
                    if let Some(pid) = crate::mac::pid_for_bundle(&bundle) {
                        crate::mac::bring_forward(pid);
                    }
                }));
            }
        });
        return;
    }
    // Claude Code hosted by the Claude desktop app: open the exact conversation by its link.
    if s.agent == "claude-code" && s.pid > 1 {
        if let Some(url) = claude_desktop_url(s.pid, &s.id) {
            crate::mac::log(&format!("claude desktop link {url}"));
            let _ = Command::new("/usr/bin/open").arg(&url).spawn();
            if let Some(pid) = crate::mac::pid_for_bundle("com.anthropic.claudefordesktop") {
                crate::mac::bring_forward(pid);
            }
            return;
        }
    }
    // A terminal agent: the app it runs in (walking up from its process), then its tab.
    let host = if s.pid > 1 { crate::mac::host_app_pid(s.pid) } else { None }.or_else(|| tty_host(&s.tty));
    crate::mac::log(&format!("host app pid {host:?}"));
    if let Some(host) = host {
        crate::mac::bring_forward(host);
        if !s.tty.is_empty() {
            // The tab script is chosen by the host app's bundle id, like TerminalTabFocus.
            let app = crate::mac::bundle_id(host).unwrap_or_else(|| s.term.clone());
            let tty = s.tty.clone();
            std::thread::spawn(move || {
                let ok = focus_terminal(&app, &tty);
                crate::mac::log(&format!("tab script {app} {tty}: {ok}"));
            });
        }
        return;
    }
    if !s.tty.is_empty() && focus_terminal(&s.term, &s.tty) {
        return;
    }
    // Last: the agent's own desktop app, if it has one and it's running.
    if let Some(pid) = host_bundle_id(&s.agent).and_then(crate::mac::pid_for_bundle) {
        crate::mac::bring_forward(pid);
        return;
    }
    open_session(agent, &s.id);
}

/// ClaudeSessionFile.configDirectory (0x1000c8b70): $CLAUDE_CONFIG_DIR, else ~/.claude.
pub fn claude_config_dir() -> PathBuf {
    std::env::var("CLAUDE_CONFIG_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
}

/// Claude Code's own record of a running session: `<config>/sessions/<pid>.json`, read only
/// when it is at most 64 KB (ClaudeSessionFile.desktopURL 0x10009e994).
pub fn claude_session_file(pid: i32) -> Option<serde_json::Value> {
    let path = claude_config_dir().join("sessions").join(format!("{pid}.json"));
    if std::fs::metadata(&path).ok()?.len() > 0x10000 {
        return None;
    }
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// ClaudeSessionFile.continueURL (0x1000c8cd8): `claude://code/continue?session=<id>` for a
/// host session id "local_" + 1 to 64 letters, digits or dashes.
pub fn claude_continue_url(host_session: &str) -> Option<String> {
    let rest = host_session.strip_prefix("local_")?;
    let ok = (1..=64).contains(&rest.len()) && rest.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| format!("claude://code/continue?session={host_session}"))
}

/// ClaudeSessionFile.desktopURL: the Claude desktop link for a Claude Code session, when the
/// session file for its process names the same session and a desktop host session.
pub fn claude_desktop_url(pid: i32, session_id: &str) -> Option<String> {
    let v = claude_session_file(pid)?;
    if v.get("sessionId")?.as_str()? != session_id {
        return None;
    }
    claude_continue_url(v.get("hostSessionId")?.as_str()?)
}

/// CodexAppLink.isThreadID (0x10061a884): 36 characters, dashes at 8, 13, 18 and 23, the rest
/// lowercase hex; then `codex://threads/<id>`.
pub fn codex_thread_url(id: &str) -> Option<String> {
    let ok = id.len() == 36
        && id.char_indices().all(|(i, c)| if matches!(i, 8 | 13 | 18 | 23) { c == '-' } else { c.is_ascii_digit() || ('a'..='f').contains(&c) });
    ok.then(|| format!("codex://threads/{id}"))
}

/// AppController.hostBundleID(for:) (0x1000e2af0): the desktop app an agent's sessions live in.
pub fn host_bundle_id(agent: &str) -> Option<&'static str> {
    match agent {
        "codex" => Some("com.openai.codex"),
        "zcode" => Some("dev.zcode.app"),
        "antigravity" => Some("com.google.antigravity"),
        "kimi-code" => Some("com.kimi.code.desktop"),
        _ => None,
    }
}

/// The app a terminal device belongs to: a process on that tty, then up to its host app.
fn tty_host(tty: &str) -> Option<i32> {
    let name = tty.trim().trim_start_matches("/dev/");
    if name.is_empty() {
        return None;
    }
    let out = Command::new("/bin/ps").args(["-t", name, "-o", "pid="]).output().ok()?;
    let pid: i32 = String::from_utf8_lossy(&out.stdout).lines().next()?.trim().parse().ok()?;
    crate::mac::host_app_pid(pid)
}

fn osascript_true(script: &str) -> bool {
    Command::new("/usr/bin/osascript")
        .args(["-e", script])
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).trim() == "true")
        .unwrap_or(false)
}

/// TerminalFocusScript.make (0x10084d6e0): the AppleScript that selects the tab (Terminal)
/// or session (iTerm2) on `tty`. Only for a device "/dev/tty…" whose name matches
/// \Atty[A-Za-z0-9]*\z; un-minimizes the window, selects it, activates, 2-second timeout.
pub fn terminal_script(app_id: &str, tty: &str) -> Option<String> {
    let name = tty.strip_prefix("/dev/")?;
    if !name.starts_with("tty") || !name[3..].chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let body = match app_id {
        "com.apple.Terminal" => format!(
            "        if tty of targetTab is \"{tty}\" then\n            set selected of targetTab to true\n            set miniaturized of targetWindow to false\n            set index of targetWindow to 1\n            activate\n            return true\n        end if"
        ),
        "com.googlecode.iterm2" => format!(
            "        repeat with targetSession in sessions of targetTab\n            if tty of targetSession is \"{tty}\" then\n                select targetSession\n                select targetTab\n                set miniaturized of targetWindow to false\n                select targetWindow\n                activate\n                return true\n            end if\n        end repeat"
        ),
        _ => return None,
    };
    Some(format!(
        "if application id \"{app_id}\" is not running then return false\nwith timeout of 2 seconds\n    tell application id \"{app_id}\"\n        repeat with targetWindow in windows\n            repeat with targetTab in tabs of targetWindow\n{body}\n            end repeat\n        end repeat\n    end tell\nend timeout\nreturn false"
    ))
}

/// Selects the agent's tab in the terminal app `app` (a bundle id, or the TERM_PROGRAM the
/// hook reported). Needs Automation consent for Terminal / iTerm2.
pub fn focus_terminal(app: &str, tty: &str) -> bool {
    let app_id = match app {
        "Apple_Terminal" | "" => "com.apple.Terminal",
        "iTerm.app" => "com.googlecode.iterm2",
        other => other,
    };
    match terminal_script(app_id, tty) {
        Some(script) => osascript_true(&script),
        None => false,
    }
}

/// Older hooks didn't send the terminal; refresh our own entries so taps can find the tab.
pub fn upgrade_claude_hooks(port: u16) {
    let Ok(v) = read_json(&claude_settings_path()) else { return };
    let text = v.to_string();
    if text.contains("/hook/claude-code") && !text.contains("X-PID") {
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
        "curl -s -m 2 -X POST -H 'Content-Type: application/json' -H \"X-TTY: $(ps -o tty= -p $$ 2>/dev/null)\" -H \"X-Term: $TERM_PROGRAM\" -H \"X-PID: $PPID\" --data-binary @- http://127.0.0.1:{port}/hook/{agent_id} >/dev/null 2>&1 || true"
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

/// Puts text on the macOS pasteboard (pbcopy), independent of the window's own clipboard.
pub fn copy_to_clipboard(text: &str) -> bool {
    use std::io::Write;
    let Ok(mut child) = Command::new("/usr/bin/pbcopy").stdin(std::process::Stdio::piped()).spawn() else { return false };
    let wrote = child.stdin.take().map(|mut i| i.write_all(text.as_bytes()).is_ok()).unwrap_or(false);
    wrote && child.wait().map(|s| s.success()).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::terminal_script;

    #[test]
    fn tab_script_only_for_real_ttys() {
        assert!(terminal_script("com.apple.Terminal", "/dev/ttys003").unwrap().contains("set index of targetWindow to 1"));
        assert!(terminal_script("com.googlecode.iterm2", "/dev/ttys012").unwrap().contains("select targetSession"));
        assert!(terminal_script("com.apple.Terminal", "ttys003").is_none());
        assert!(terminal_script("com.apple.Terminal", "/dev/ttys0\" & x").is_none());
        assert!(terminal_script("com.mitchellh.ghostty", "/dev/ttys003").is_none());
    }

    #[test]
    fn claude_continue_link() {
        use super::claude_continue_url;
        assert_eq!(claude_continue_url("local_523941ab-cd").as_deref(), Some("claude://code/continue?session=local_523941ab-cd"));
        assert!(claude_continue_url("523941").is_none());
        assert!(claude_continue_url("local_").is_none());
        assert!(claude_continue_url("local_a b").is_none());
    }

    #[test]
    fn codex_thread_link() {
        use super::codex_thread_url;
        assert_eq!(codex_thread_url("01a1cb6f-8865-7912-9a2a-fd33459a242b").as_deref(), Some("codex://threads/01a1cb6f-8865-7912-9a2a-fd33459a242b"));
        assert!(codex_thread_url("01A1CB6F-8865-7912-9a2a-fd33459a242b").is_none());
        assert!(codex_thread_url("Investigate rate-limited recipient alert").is_none());
    }
}
