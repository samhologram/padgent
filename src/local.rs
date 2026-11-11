//! Sessions read from the agents' own local records, the way legacy follows them:
//! Claude Code's live session files (`~/.claude/sessions/<pid>.json`: status busy/idle and
//! the conversation's name) and Codex's thread databases (`~/.codex/state_N.sqlite` and
//! `thread_history_N.sqlite`: user threads, their latest turn, final message and unread
//! turns). Only live or recent sessions are listed; nothing is written.

use crate::model::{Core, Incoming};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn alive(pid: i32) -> bool {
    pid > 1 && unsafe { libc::kill(pid, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

/// Claude Code sessions with a running process.
fn claude_sessions() -> Vec<Value> {
    let dir = crate::integrations::claude_config_dir().join("sessions");
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(pid) = p.file_stem().and_then(|s| s.to_str()).and_then(|s| s.parse::<i32>().ok()) else { continue };
        if !alive(pid) {
            continue;
        }
        if let Some(v) = crate::integrations::claude_session_file(pid) {
            out.push(v);
        }
    }
    out
}

/// The newest `<prefix><N>.sqlite` in a folder (CodexStateStore.databaseURL picks the highest N).
fn newest_db(dir: &Path, prefix: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let n: u32 = name.strip_prefix(prefix)?.strip_suffix(".sqlite")?.parse().ok()?;
            Some((n, e.path()))
        })
        .max_by_key(|(n, _)| *n)
        .map(|(_, p)| p)
}

/// Codex user threads active within `recent_ms`, or with a turn in progress.
fn codex_threads(recent_ms: i64) -> Vec<Value> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let dir = home.join(".codex");
    let (Some(state), Some(hist)) = (newest_db(&dir, "state_"), newest_db(&dir, "thread_history_")) else { return Vec::new() };
    let hist = hist.to_string_lossy().replace('\'', "''");
    let sql = format!(
        "attach 'file:{hist}?mode=ro' as h;
select t.id as id, coalesce(nullif(t.name,''), nullif(t.title,''), nullif(substr(trim(t.first_user_message),1,60),''), nullif(substr(trim(t.preview),1,60),''), '') as name,
 (select status from h.thread_turns x where x.thread_id=t.id order by x.rollout_ordinal desc limit 1) as status,
 (select started_at from h.thread_turns x where x.thread_id=t.id order by x.rollout_ordinal desc limit 1) as started,
 (select json_extract(i.item_json,'$.text') from h.thread_turns x join h.thread_items i on i.thread_id=x.thread_id and i.item_id=x.final_agent_item_id
   where x.thread_id=t.id order by x.rollout_ordinal desc limit 1) as message,
 (select coalesce(x.completed_at, x.started_at) from h.thread_turns x where x.thread_id=t.id and x.final_agent_item_id is not null order by x.rollout_ordinal desc limit 1) as message_at,
 (select i.item_type from h.thread_items i where i.thread_id=t.id and i.turn_id=(select x.turn_id from h.thread_turns x where x.thread_id=t.id order by x.rollout_ordinal desc limit 1)
   order by i.rollout_ordinal desc, i.updated_at_ordinal desc limit 1) as live_type,
 (select i.item_json from h.thread_items i where i.thread_id=t.id and i.turn_id=(select x.turn_id from h.thread_turns x where x.thread_id=t.id order by x.rollout_ordinal desc limit 1)
   order by i.rollout_ordinal desc, i.updated_at_ordinal desc limit 1) as live_json,
 (select i.item_id from h.thread_items i where i.thread_id=t.id and i.turn_id=(select x.turn_id from h.thread_turns x where x.thread_id=t.id order by x.rollout_ordinal desc limit 1)
   order by i.rollout_ordinal desc, i.updated_at_ordinal desc limit 1) as live_id,
 (select count(*) from h.thread_turns x where x.thread_id=t.id and r.first_unread_turn is not null and
   x.rollout_ordinal >= (select y.rollout_ordinal from h.thread_turns y where y.thread_id=t.id and y.turn_id=r.first_unread_turn)) as unread
from threads t left join thread_read_receipts r on r.thread_id=t.id
where t.thread_source='user' and t.archived=0 and (coalesce(t.name,'')<>'' or coalesce(t.title,'')<>'' or trim(coalesce(t.first_user_message,''))<>'') and t.recency_at_ms > (strftime('%s','now')*1000 - {recent_ms})
order by t.recency_at_ms desc limit 24;"
    );
    let uri = format!("file:{}?mode=ro", state.to_string_lossy());
    let out = std::process::Command::new("/usr/bin/sqlite3").args(["-json", "-cmd", ".timeout 500", &uri, &sql]).output();
    match out {
        Ok(o) if o.status.success() => serde_json::from_slice::<Vec<Value>>(&o.stdout).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn now_s() -> f64 {
    crate::model::now()
}

/// Markdown marks off: `**`, backticks, headings, and `[label](link)` → label.
fn plain(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s.trim_start_matches('#').trim_start();
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        match after.find("](").and_then(|mid| after[mid + 2..].find(')').map(|end| (mid, mid + 2 + end))) {
            Some((mid, end)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..mid]);
                rest = &after[end + 1..];
            }
            None => {
                out.push_str(&rest[..open + 1]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out.replace("**", "").replace('`', "")
}

/// First meaningful line of a text, cleaned for a small key.
fn line(s: &str, max: usize) -> String {
    let l = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    plain(l).chars().take(max).collect()
}

/// The last line of a growing text (the newest thought).
fn last_line(s: &str, max: usize) -> String {
    let l = s.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("");
    let chars: Vec<char> = plain(l).chars().collect();
    let start = chars.len().saturating_sub(max);
    chars[start..].iter().collect()
}

/// What a tool call is doing, in a few words.
fn tool_detail(name: &str, input: &Value) -> String {
    let d = ["command", "file_path", "pattern", "url", "query", "description", "path", "prompt"]
        .iter()
        .find_map(|k| input.get(*k).and_then(Value::as_str))
        .map(|s| line(s, 160))
        .unwrap_or_default();
    if d.is_empty() { name.to_string() } else { format!("{name} {d}") }
}

/// A reply that ends by asking the person something (heuristic, like legacy's
/// allowHeuristicWaits for Codex).
fn asks(text: &str) -> bool {
    let t = text.trim_end();
    t.ends_with('?') || t.ends_with('？')
}

/// Whether a transcript was written within the recent-message window.
fn live_is_recent(path: Option<&PathBuf>) -> bool {
    path.and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .and_then(|m| m.elapsed().ok())
        .map(|e| e.as_secs_f64() <= crate::model::RECENT_TEXT_S)
        .unwrap_or(false)
}

/// Live step of a session: (state, text, tool name, signature).
type Live = (&'static str, String, String, String);

/// Claude Code's transcript `projects/*/<sessionId>.jsonl`, found once and remembered.
fn claude_transcript(id: &str, cache: &mut HashMap<String, PathBuf>) -> Option<PathBuf> {
    if let Some(p) = cache.get(id) {
        if p.exists() {
            return Some(p.clone());
        }
    }
    let root = crate::integrations::claude_config_dir().join("projects");
    for e in std::fs::read_dir(root).ok()?.flatten() {
        let p = e.path().join(format!("{id}.jsonl"));
        if p.exists() {
            cache.insert(id.to_string(), p.clone());
            return Some(p);
        }
    }
    None
}

/// The newest assistant step in the transcript's tail: thinking, a reply being written, or a
/// tool call.
fn claude_live(path: &Path, busy: bool) -> Option<Live> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let start = len.saturating_sub(192 * 1024);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf);
    for l in text.lines().rev() {
        let Ok(d) = serde_json::from_str::<Value>(l) else { continue };
        let kind = d.get("type").and_then(Value::as_str).unwrap_or("");
        if kind == "user" && busy {
            // A tool result or a new prompt: still working on it.
            let sig = d.get("uuid").and_then(Value::as_str).unwrap_or("").to_string();
            return Some(("thinking", String::new(), String::new(), sig));
        }
        if kind != "assistant" {
            continue;
        }
        let sig = format!("{}:{}", d.get("uuid").and_then(Value::as_str).unwrap_or(""), l.len());
        let blocks = d.pointer("/message/content").and_then(Value::as_array)?;
        let b = blocks.last()?;
        return Some(match b.get("type").and_then(Value::as_str).unwrap_or("") {
            "thinking" => {
                let t = b.get("thinking").and_then(Value::as_str).unwrap_or("");
                let shown = if t.trim().is_empty() { String::new() } else { last_line(t, 200) };
                ("thinking", shown, String::new(), sig)
            }
            "text" => {
                let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                // A final reply that ends with a question shows that question.
                let shown = if !busy && asks(&last_line(t, 400)) { last_line(t, 240) } else { line(t, 240) };
                (if busy { "streaming" } else { "done" }, shown, String::new(), sig)
            }
            "tool_use" => {
                let name = b.get("name").and_then(Value::as_str).unwrap_or("tool").to_string();
                let input = b.get("input").cloned().unwrap_or(Value::Null);
                if name == "AskUserQuestion" {
                    let q = input.pointer("/questions/0/question").and_then(Value::as_str).unwrap_or("");
                    return Some(("waitingQuestion", line(q, 200), String::new(), sig));
                }
                ("tool", tool_detail(&name, &input), name, sig)
            }
            _ => ("thinking", String::new(), String::new(), sig),
        });
    }
    None
}

/// The newest item of a Codex turn in progress.
fn codex_live(item_type: &str, item: &Value) -> (&'static str, String, String) {
    match item_type {
        "reasoning" => {
            let summary: Vec<String> = item
                .get("summary")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(|x| x.as_str().or_else(|| x.get("text").and_then(Value::as_str))).map(str::to_string).collect())
                .unwrap_or_default();
            ("thinking", last_line(&summary.join("\n"), 200), String::new())
        }
        "agentMessage" => ("streaming", line(item.get("text").and_then(Value::as_str).unwrap_or(""), 240), String::new()),
        "commandExecution" => {
            let cmd = item.get("command").and_then(Value::as_str).unwrap_or("");
            // `/bin/zsh -lc "…"` → the command itself.
            let inner = cmd.split_once("-lc ").map(|(_, c)| c.trim_matches(|c| c == '"' || c == '\'')).unwrap_or(cmd);
            ("tool", format!("shell {}", line(inner, 160)), "Bash".into())
        }
        "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall" => {
            let tool = item.get("tool").and_then(Value::as_str).unwrap_or("tool");
            let server = item.get("server").and_then(Value::as_str).unwrap_or("");
            let name = if server.is_empty() { tool.to_string() } else { format!("{server}.{tool}") };
            ("tool", name.clone(), name)
        }
        "fileChange" => {
            let path = item.pointer("/changes/0/path").and_then(Value::as_str).unwrap_or("files");
            ("tool", format!("Editing {}", Path::new(path).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()), "Edit".into())
        }
        "webSearch" => ("tool", format!("Searching {}", line(item.get("query").and_then(Value::as_str).unwrap_or(""), 120)), "WebSearch".into()),
        "contextCompaction" => ("compacting", String::new(), String::new()),
        "subAgentActivity" => ("tool", "Sub-agent working".into(), "Task".into()),
        _ => ("thinking", String::new(), String::new()),
    }
}

pub fn start(core: Arc<Mutex<Core>>, wake: Arc<dyn Fn() + Send + Sync>) {
    std::thread::Builder::new()
        .name("local-sessions".into())
        .spawn(move || {
            // Last thing reported per session, so only changes are applied.
            let mut last: HashMap<String, String> = HashMap::new();
            let mut transcripts: HashMap<String, PathBuf> = HashMap::new();
            let mut tick: u64 = 0;
            loop {
                let (claude_on, codex_on, recency) = match core.lock() {
                    Ok(c) => (c.agent_enabled("claude-code"), c.agent_enabled("codex"), c.settings.recency_minutes as i64),
                    Err(_) => return,
                };
                let mut events: Vec<Incoming> = Vec::new();
                let mut busy_any = false;
                if claude_on {
                    for v in claude_sessions() {
                        let (Some(id), Some(pid)) = (v.get("sessionId").and_then(Value::as_str), v.get("pid").and_then(Value::as_i64)) else { continue };
                        let status = v.get("status").and_then(Value::as_str).unwrap_or("idle");
                        let busy = status == "busy";
                        busy_any |= busy;
                        let prev = last.get(id).cloned();
                        let was_busy = prev.as_deref().map(|p| p.starts_with("busy")).unwrap_or(false);
                        // Live step while busy (thinking, writing, tool); its last reply when it stops.
                        let live = if busy || was_busy || prev.is_none() {
                            claude_transcript(id, &mut transcripts).and_then(|p| claude_live(&p, busy))
                        } else {
                            None
                        };
                        let sig = format!("{status}|{}", live.as_ref().map(|l| l.3.as_str()).unwrap_or(""));
                        if prev.as_deref() == Some(sig.as_str()) {
                            continue;
                        }
                        last.insert(id.to_string(), sig);
                        let (mut state, text, tool) = match (&live, busy) {
                            (Some((s, t, tool, _)), true) => (*s, t.clone(), tool.clone()),
                            (Some((_, t, _, _)), false) => ("idle", t.clone(), String::new()),
                            (None, true) => ("thinking", String::new(), String::new()),
                            (None, false) => ("idle", String::new(), String::new()),
                        };
                        // Busy then idle: the turn just finished.
                        if !busy && was_busy {
                            state = "done";
                        }
                        // A recent final reply that asks something waits for your answer, and so
                        // does Claude's question tool whatever the status says.
                        if (!busy && asks(&text) && live_is_recent(transcripts.get(id)))
                            || live.as_ref().map(|l| l.0 == "waitingQuestion").unwrap_or(false)
                        {
                            state = "waitingQuestion";
                        }
                        let mut body = json!({ "session": id, "state": state, "_pid": pid.to_string() });
                        if let Some(n) = v.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()) {
                            body["title"] = json!(n);
                        } else if let Some(cwd) = v.get("cwd").and_then(Value::as_str) {
                            body["cwd"] = json!(cwd);
                        }
                        if text.is_empty() && busy {
                            body["_clear_text"] = json!(true);
                        }
                        if !text.is_empty() {
                            body["text"] = json!(text);
                            if prev.is_none() && !busy {
                                // An old reply seen at start-up keeps the transcript's time.
                                if let Some(m) = transcripts.get(id).and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok()) {
                                    let at = m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
                                    body["_text_at"] = json!(at);
                                }
                            }
                        }
                        if !tool.is_empty() {
                            body["tool_name"] = json!(tool);
                        }
                        events.push(Incoming { agent: "claude-code".into(), event: None, body });
                    }
                }
                if codex_on {
                    for v in codex_threads(recency * 60_000) {
                        let Some(id) = v.get("id").and_then(Value::as_str) else { continue };
                        let status = v.get("status").and_then(Value::as_str).unwrap_or("");
                        let started = v.get("started").and_then(Value::as_f64).unwrap_or(0.0);
                        let running = status == "inProgress" && now_s() - started < crate::model::STALE_BUSY_AFTER_S;
                        busy_any |= running;
                        let mut state = match status {
                            "inProgress" if running => "thinking",
                            "completed" => "done",
                            "failed" => "error",
                            _ => "idle",
                        };
                        let unread = v.get("unread").and_then(Value::as_i64).unwrap_or(0);
                        let full = v.get("message").and_then(Value::as_str).unwrap_or("");
                        let mut message = if asks(&last_line(full, 400)) { last_line(full, 240) } else { line(full, 240) };
                        let mut tool = String::new();
                        let mut live_sig = String::new();
                        if running {
                            // The newest step of the turn in progress, as it happens.
                            let item: Value = v.get("live_json").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
                            let (s, t, tl) = codex_live(v.get("live_type").and_then(Value::as_str).unwrap_or(""), &item);
                            state = s;
                            message = t;
                            tool = tl;
                            live_sig = format!("{}:{}", v.get("live_id").and_then(Value::as_str).unwrap_or(""), message.len());
                        }
                        let sig = format!("{state}|{unread}|{}|{live_sig}", message.len());
                        if last.get(id) == Some(&sig) {
                            continue;
                        }
                        let first = !last.contains_key(id);
                        last.insert(id.to_string(), sig);
                        // On first sight a finished thread is just idle, not "just finished".
                        if first && state == "done" {
                            state = "idle";
                        }
                        // A recent final reply that asks something waits for your answer.
                        let message_at = v.get("message_at").and_then(Value::as_f64).unwrap_or(0.0);
                        if !running && asks(&message) && now_s() - message_at <= crate::model::RECENT_TEXT_S {
                            state = "waitingQuestion";
                        }
                        let mut body = json!({ "session": id, "state": state, "_app": "com.openai.codex", "_unread": unread });
                        if let Some(n) = v.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()) {
                            body["title"] = json!(n);
                        }
                        if message.is_empty() && running {
                            body["_clear_text"] = json!(true);
                        }
                        if !message.is_empty() {
                            body["text"] = json!(message);
                            if !running {
                                body["_text_at"] = json!(v.get("message_at").and_then(Value::as_f64).unwrap_or(0.0));
                            }
                        }
                        if !tool.is_empty() {
                            body["tool_name"] = json!(tool);
                        }
                        events.push(Incoming { agent: "codex".into(), event: None, body });
                    }
                }
                if !events.is_empty() {
                    if let Ok(mut c) = core.lock() {
                        for e in events {
                            c.apply(e);
                        }
                    }
                    wake();
                }
                tick += 1;
                // Every second while something works, every three seconds otherwise.
                std::thread::sleep(Duration::from_millis(if busy_any { 1000 } else { 3000 }));
                let _ = tick;
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    #[test]
    fn markdown_marks_are_removed() {
        assert_eq!(super::plain("J'ai committé [develop.md](references/develop.md) **ok**"), "J'ai committé develop.md ok");
        assert_eq!(super::plain("## Title with `code`"), "Title with code");
        assert_eq!(super::plain("a [b] c"), "a [b] c");
    }

    #[test]
    fn question_heuristic() {
        assert!(super::asks("Use OAuth device flow or PKCE?"));
        assert!(super::asks("On garde la v2 ? "));
        assert!(!super::asks("All 214 checks passed."));
    }
}
