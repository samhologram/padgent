//! Sessions read from the agents' own local records:
//! Claude Code's live session files (`~/.claude/sessions/<pid>.json`: status busy/idle and
//! the conversation's name) and Codex's thread databases (`~/.codex/state_N.sqlite` and
//! `thread_history_N.sqlite`: user threads, their latest turn, final message and unread
//! turns). Only live or recent sessions are listed; nothing is written.

use crate::model::{Activity, Core, SessionStep};
use crate::observation::{Observation, ObservationKind, Source};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
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

/// Resolve a desktop host ID only through a live process's own session record.
pub fn claude_session_for_host(host: &str) -> Option<String> {
    if crate::integrations::claude_continue_url(host).is_none() { return None; }
    let mut ids: Vec<String> = claude_sessions().into_iter().filter_map(|v| {
        (v.get("hostSessionId")?.as_str()? == host)
            .then(|| v.get("sessionId").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string))
            .flatten()
    }).collect();
    ids.sort();
    ids.dedup();
    if ids.len() == 1 { ids.pop() } else { None }
}

/// IDs linked by Claude Desktop's own session metadata to a scheduled task.
fn scheduled_claude_ids_from_record(v: &Value) -> Vec<String> {
    if v.get("scheduledTaskId").and_then(Value::as_str).is_none_or(|s| s.trim().is_empty())
        || v.get("spawnedFrom").is_some_and(|v| !v.is_null()) { return Vec::new(); }
    ["sessionId", "cliSessionId"].into_iter().filter_map(|key|
        v.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string)
    ).collect()
}

// Claude Desktop persists lastActivityAt as Unix milliseconds.
fn scheduled_claude_snapshot(v: &Value, now: f64) -> Option<(String, f64, Observation)> {
    if scheduled_claude_ids_from_record(v).is_empty() || v.get("isArchived").and_then(Value::as_bool) == Some(true) { return None; }
    let host = v.get("sessionId").and_then(Value::as_str).filter(|s| crate::integrations::claude_continue_url(s).is_some())?;
    let at = v.get("lastActivityAt").and_then(Value::as_f64)? / 1000.0;
    if at <= 0.0 || at > now || now - at >= crate::model::REMOVE_AFTER_S { return None; }
    let mut obs = Observation::new("claude-code", host, Source::LocalSnapshot, now);
    obs.kind = ObservationKind::Discovery;
    obs.activity = Some(Activity::Idle);
    obs.scheduled = true;
    obs.recorded_at = Some(at);
    obs.text_at = Some(at);
    if let Some(cli) = v.get("cliSessionId").and_then(Value::as_str).filter(|id| !id.trim().is_empty() && *id != host) {
        obs.aliases.push(cli.to_string());
    }
    obs.title = v.get("title").and_then(Value::as_str).filter(|s| !s.trim().is_empty()).map(str::to_string);
    Some((host.to_string(), at, obs))
}

#[derive(Clone, Copy, Default)]
pub struct ScheduledScanHealth { pub scans: u64, pub directories: u64, pub files: u64, pub records: u64, pub eligible: u64, pub permission_denied: u64, pub other_errors: u64 }
static SCHEDULED_SCAN_HEALTH: Mutex<ScheduledScanHealth> = Mutex::new(ScheduledScanHealth { scans: 0, directories: 0, files: 0, records: 0, eligible: 0, permission_denied: 0, other_errors: 0 });

pub fn scheduled_scan_health() -> ScheduledScanHealth {
    SCHEDULED_SCAN_HEALTH.lock().map(|s| *s).unwrap_or_default()
}

fn scheduled_claude_records() -> Vec<Value> {
    let mut health = ScheduledScanHealth { scans: scheduled_scan_health().scans + 1, ..Default::default() };
    let mut records = Vec::new();
    if let Some(home) = dirs::home_dir() {
        let base = home.join("Library/Application Support/Claude");
        fn directory(path: &Path, health: &mut ScheduledScanHealth) -> Option<std::fs::ReadDir> {
            match std::fs::read_dir(path) {
                Ok(dir) => { health.directories += 1; Some(dir) }
                Err(err) => {
                    if err.kind() == std::io::ErrorKind::PermissionDenied { health.permission_denied += 1; }
                    else if err.kind() != std::io::ErrorKind::NotFound { health.other_errors += 1; }
                    None
                }
            }
        }
        for root in ["claude-code-sessions", "local-agent-mode-sessions"] {
            let Some(workspaces) = directory(&base.join(root), &mut health) else { continue };
            for workspace in workspaces.flatten() {
                let Some(projects) = directory(&workspace.path(), &mut health) else { continue };
                for project in projects.flatten() {
                    let Some(files) = directory(&project.path(), &mut health) else { continue };
                    for file in files.flatten() {
                        let p = file.path();
                        if p.extension().and_then(|s| s.to_str()) != Some("json") || p.file_name().and_then(|s| s.to_str()) == Some("scheduled-tasks.json") { continue; }
                        health.files += 1;
                        match std::fs::metadata(&p) {
                            Ok(m) if m.len() <= 2 * 1024 * 1024 => {}
                            Ok(_) => continue,
                            Err(err) => { if err.kind() == std::io::ErrorKind::PermissionDenied { health.permission_denied += 1; } else { health.other_errors += 1; } continue; }
                        }
                        let bytes = match std::fs::read(p) {
                            Ok(bytes) => bytes,
                            Err(err) => { if err.kind() == std::io::ErrorKind::PermissionDenied { health.permission_denied += 1; } else { health.other_errors += 1; } continue; }
                        };
                        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
                            if !scheduled_claude_ids_from_record(&v).is_empty() {
                                if scheduled_claude_snapshot(&v, now_s()).is_some() { health.eligible += 1; }
                                records.push(v);
                            }
                        }
                    }
                }
            }
        }
    } else { health.other_errors += 1; }
    health.records = records.len() as u64;
    if let Ok(mut s) = SCHEDULED_SCAN_HEALTH.lock() { *s = health; }
    records
}

/// Exact thread IDs from the desktop automation run records; never read prompt content.
fn scheduled_codex_ids() -> HashSet<String> {
    let Some(home) = dirs::home_dir() else { return HashSet::new() };
    let mut ids = HashSet::new();
    for name in ["codex.db", "codex-dev.db"] {
        let path = home.join(".codex/sqlite").join(name);
        if !path.is_file() { continue; }
        let uri = format!("file:{}?mode=ro", path.to_string_lossy());
        let output = std::process::Command::new("/usr/bin/sqlite3").args(["-json", "-cmd", ".timeout 500", &uri,
            "select distinct thread_id from automation_runs where thread_id is not null and thread_id <> ''"]).output();
        if let Ok(o) = output {
            if o.status.success() {
                if let Ok(rows) = serde_json::from_slice::<Vec<Value>>(&o.stdout) {
                    ids.extend(rows.iter().filter_map(|v| v.get("thread_id").and_then(Value::as_str).map(str::to_string)));
                }
            }
        }
    }
    ids
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

/// Select ordinary user threads and exact automation-run threads, excluding internal work.
fn codex_source_predicate(scheduled_ids: &HashSet<String>) -> String {
    let mut ids: Vec<_> = scheduled_ids.iter().filter(|id| crate::integrations::codex_thread_url(id).is_some()).collect();
    ids.sort();
    let in_ids = if ids.is_empty() { "0".to_string() } else {
        format!("t.id in ({})", ids.iter().map(|id| format!("'{id}'")).collect::<Vec<_>>().join(","))
    };
    format!("coalesce(t.thread_source,'') not in ('subagent','guardian_review') and (t.thread_source='user' or {in_ids})")
}

/// Recent user and explicitly mapped scheduled Codex threads.
fn codex_threads(recent_ms: i64, scheduled_ids: &HashSet<String>) -> Vec<Value> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let dir = home.join(".codex");
    let (Some(state), Some(hist)) = (newest_db(&dir, "state_"), newest_db(&dir, "thread_history_")) else { return Vec::new() };
    let hist = hist.to_string_lossy().replace('\'', "''");
    let source_predicate = codex_source_predicate(scheduled_ids);
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
 (select json_group_array(json_object('id', item_id, 'turn', turn_id, 'type', item_type, 'item', item_json)) from
   (select i.item_id, i.turn_id, i.item_type, i.item_json from h.thread_items i where i.thread_id=t.id and i.turn_id=(select x.turn_id from h.thread_turns x where x.thread_id=t.id order by x.rollout_ordinal desc limit 1)
    order by i.rollout_ordinal desc limit 12)) as steps_json,
 (select count(*) from h.thread_turns x where x.thread_id=t.id and r.first_unread_turn is not null and
   x.rollout_ordinal >= (select y.rollout_ordinal from h.thread_turns y where y.thread_id=t.id and y.turn_id=r.first_unread_turn)) as unread
from threads t left join thread_read_receipts r on r.thread_id=t.id
where {source_predicate} and t.archived=0 and (coalesce(t.name,'')<>'' or coalesce(t.title,'')<>'' or trim(coalesce(t.first_user_message,''))<>'') and t.recency_at_ms > (strftime('%s','now')*1000 - {recent_ms})
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

/// A reply that ends by asking the person something (heuristic, based on the
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

/// Bounded assistant-only transcript summaries, in chronological order.
fn claude_steps(path: &Path) -> Vec<SessionStep> {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else { return Vec::new() };
    let Ok(meta) = file.metadata() else { return Vec::new() };
    let start = meta.len().saturating_sub(192 * 1024);
    if file.seek(SeekFrom::Start(start)).is_err() { return Vec::new(); }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() { return Vec::new(); }
    let text = String::from_utf8_lossy(&bytes);
    let mut out = Vec::new();
    for (line_index, raw) in text.lines().enumerate() {
        if start > 0 && line_index == 0 { continue; }
        let Ok(row) = serde_json::from_str::<Value>(raw) else { continue };
        if row.get("type").and_then(Value::as_str) != Some("assistant") { continue; }
        let Some(uuid) = row.get("uuid").and_then(Value::as_str) else { continue };
        let Some(blocks) = row.pointer("/message/content").and_then(Value::as_array) else { continue };
        for (index, block) in blocks.iter().enumerate() {
            let (kind, title) = match block.get("type").and_then(Value::as_str).unwrap_or("") {
                "thinking" => ("Thinking", "Thinking".to_string()),
                "text" => ("Reply", "Reply".to_string()),
                "tool_use" => ("Tool", block.get("name").and_then(Value::as_str).unwrap_or("Tool").to_string()),
                _ => continue,
            };
            let detail = match block.get("type").and_then(Value::as_str).unwrap_or("") {
                "thinking" => line(block.get("thinking").and_then(Value::as_str).unwrap_or(""), 160),
                "text" => line(block.get("text").and_then(Value::as_str).unwrap_or(""), 160),
                "tool_use" => tool_detail(&title, &block.get("input").cloned().unwrap_or(Value::Null)),
                _ => String::new(),
            };
            out.push(SessionStep { signature: format!("provider:{uuid}:{index}"), kind: kind.into(), title, status: "Observed".into(), text: detail });
        }
    }
    if out.len() > 12 { out.drain(..out.len() - 12); }
    out
}

fn codex_steps(value: &Value) -> Vec<SessionStep> {
    let mut rows: Vec<Value> = value.as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or_default();
    rows.reverse();
    rows.into_iter().filter_map(|row| {
        let id = row.get("id")?.as_str()?;
        let turn = row.get("turn").and_then(Value::as_str).unwrap_or("");
        let kind = row.get("type").and_then(Value::as_str).unwrap_or("");
        let item: Value = row.get("item").and_then(Value::as_str).and_then(|s| serde_json::from_str(s).ok()).unwrap_or(Value::Null);
        let (category, title) = match kind {
            "reasoning" => ("Thinking", "Thinking".to_string()),
            "agentMessage" => ("Reply", "Reply".to_string()),
            "commandExecution" => ("Tool", "Shell".to_string()),
            "mcpToolCall" | "dynamicToolCall" | "collabAgentToolCall" => ("Tool", item.get("tool").and_then(Value::as_str).unwrap_or("Tool").to_string()),
            "fileChange" => ("Tool", "Editing".to_string()),
            "webSearch" => ("Tool", "Search".to_string()),
            "contextCompaction" => ("Update", "Compacting".to_string()),
            _ => return None,
        };
        let status = item.get("status").and_then(Value::as_str).unwrap_or("Observed");
        let (_, detail, _) = codex_live(kind, &item);
        Some(SessionStep { signature: format!("provider:{turn}:{id}"), kind: category.into(), title, status: status.into(), text: detail })
    }).collect()
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
            let is_question_tool = ["request_user_input", "request_user_input_async"].iter().any(|suffix| {
                tool == *suffix || tool.strip_suffix(suffix).is_some_and(|prefix| prefix.ends_with('.') || prefix.ends_with('/') || prefix.ends_with(':') || prefix.ends_with("__"))
            });
            let status = item.get("status").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase();
            let pending = !matches!(status.as_str(), "completed" | "complete" | "failed" | "cancelled" | "canceled");
            if is_question_tool && pending {
                let args = item.get("arguments").cloned().unwrap_or(Value::Null);
                let args = args.as_str().and_then(|s| serde_json::from_str(s).ok()).unwrap_or(args);
                let question = args.get("questions").and_then(Value::as_array).and_then(|questions| {
                    questions.iter().find_map(|q| q.get("question").and_then(Value::as_str).filter(|s| !s.trim().is_empty())
                        .or_else(|| q.get("title").and_then(Value::as_str).filter(|s| !s.trim().is_empty())))
                }).unwrap_or("");
                ("waitingQuestion", line(question, 240), String::new())
            } else { ("tool", name.clone(), name) }
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
            let mut scheduled_records = Vec::new();
            let mut codex_scheduled = HashSet::new();
            let mut schedule_scan: Option<std::time::Instant> = None;
            loop {
                let (claude_on, codex_on, recency) = match core.lock() {
                    Ok(c) => (c.agent_enabled("claude-code"), c.agent_enabled("codex"), c.settings.recency_minutes as i64),
                    Err(_) => return,
                };
                if schedule_scan.is_none_or(|t| t.elapsed() >= Duration::from_secs(5)) {
                    if claude_on { scheduled_records = scheduled_claude_records(); }
                    if codex_on { codex_scheduled = scheduled_codex_ids(); }
                    schedule_scan = Some(std::time::Instant::now());
                }
                let mut events: Vec<Observation> = Vec::new();
                let mut busy_any = false;
                if claude_on {
                    let live_sessions = claude_sessions();
                    let records = &scheduled_records;
                    let scheduled: HashSet<String> = records.iter().flat_map(scheduled_claude_ids_from_record).collect();
                    // Discovery creates missing recent sessions and only annotates existing ones.
                    for v in records {
                        if let Some((_, _, obs)) = scheduled_claude_snapshot(v, now_s()) { events.push(obs); }
                    }
                    // Exact scheduled IDs annotate existing sessions without creating them.
                    for id in &scheduled {
                        let mut obs = Observation::new("claude-code", id, Source::LocalSnapshot, now_s());
                        obs.scheduled = true;
                        events.push(obs);
                    }
                    for v in live_sessions {
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
                        let steps = transcripts.get(id).map(|p| claude_steps(p)).unwrap_or_default();
                        let sig = format!("{status}|{}|{steps:?}|{}|{}", live.as_ref().map(|l| l.3.as_str()).unwrap_or(""), scheduled.contains(id), v.get("hostSessionId").and_then(Value::as_str).unwrap_or(""));
                        if prev.as_deref() == Some(sig.as_str()) {
                            let mut obs = Observation::new("claude-code", id, Source::LocalSnapshot, now_s());
                            obs.kind = ObservationKind::Heartbeat;
                            events.push(obs);
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
                        let mut obs = Observation::new("claude-code", id, Source::LocalSnapshot, now_s());
                        obs.activity = Activity::parse(state);
                        obs.pid = i32::try_from(pid).ok();
                        obs.scheduled = scheduled.contains(id);
                        obs.steps = Some(steps);
                        if let Some(host) = v.get("hostSessionId").and_then(Value::as_str) { obs.aliases.push(host.to_string()); }
                        obs.title = v.get("name").and_then(Value::as_str).filter(|n| !n.trim().is_empty()).map(str::to_string)
                            .or_else(|| v.get("cwd").and_then(Value::as_str).map(|cwd| std::path::Path::new(cwd).file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_else(|| cwd.to_string())));
                        obs.clear_text = text.is_empty() && busy;
                        if !text.is_empty() {
                            obs.text = Some(text);
                            if prev.is_none() && !busy {
                                obs.text_at = transcripts.get(id).and_then(|p| std::fs::metadata(p).ok()).and_then(|m| m.modified().ok()).map(|m| m.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0));
                            }
                        }
                        if !tool.is_empty() { obs.tool_name = Some(tool); }
                        events.push(obs);
                    }
                }
                if codex_on {
                    let scheduled = &codex_scheduled;
                    for v in codex_threads(recency * 60_000, scheduled) {
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
                        let steps = codex_steps(&v["steps_json"]);
                        let sig = format!("{state}|{unread}|{message}|{live_sig}|{}|{}", format!("{steps:?}"), scheduled.contains(id));
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
                        let mut obs = Observation::new("codex", id, Source::LocalSnapshot, now_s());
                        obs.activity = Activity::parse(state);
                        obs.app = Some("com.openai.codex".into());
                        obs.unread = Some(unread.max(0) as u32);
                        obs.steps = Some(steps);
                        obs.scheduled = scheduled.contains(id);
                        obs.title = v.get("name").and_then(Value::as_str).filter(|n| !n.is_empty()).map(str::to_string);
                        obs.clear_text = message.is_empty() && running;
                        if !message.is_empty() {
                            obs.text = Some(message);
                            if !running { obs.text_at = Some(message_at); }
                        }
                        if !tool.is_empty() { obs.tool_name = Some(tool); }
                        events.push(obs);
                    }
                }
                if !events.is_empty() {
                    if let Ok(mut c) = core.lock() {
                        for e in events {
                            c.observe(e);
                        }
                    }
                    wake();
                }
                // Every second while something works, every three seconds otherwise.
                std::thread::sleep(Duration::from_millis(if busy_any { 1000 } else { 3000 }));
            }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    #[test]
    fn codex_source_query_includes_only_user_or_explicit_runs_and_excludes_internal() {
        use std::collections::HashSet;
        let ids: Vec<String> = (1..=6).map(|n| format!("00000000-0000-0000-0000-{n:012}" )).collect();
        let scheduled: HashSet<String> = [ids[1].clone(), ids[2].clone(), ids[4].clone(), ids[5].clone(), "invalid' OR 1=1 --".into()].into_iter().collect();
        let predicate = super::codex_source_predicate(&scheduled);
        let sql = format!("create table threads(id text, thread_source text);             insert into threads values ('{}','user'),('{}',null),('{}','automation'),('{}',null),('{}','subagent'),('{}','guardian_review');             select t.id from threads t where {predicate} order by t.id;", ids[0], ids[1], ids[2], ids[3], ids[4], ids[5]);
        let out = std::process::Command::new("/usr/bin/sqlite3").args(["-json", ":memory:", &sql]).output().unwrap();
        assert!(out.status.success());
        let rows: Vec<serde_json::Value> = serde_json::from_slice(&out.stdout).unwrap();
        let found: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(found, vec![ids[0].as_str(), ids[1].as_str(), ids[2].as_str()]);
        assert!(!scheduled.contains(&ids[0]), "manual user thread stays unmarked");
    }

    #[test]
    fn scheduled_claude_requires_explicit_task_and_excludes_subagents() {
        use serde_json::json;
        assert_eq!(super::scheduled_claude_ids_from_record(&json!({"scheduledTaskId":"task", "sessionId":"host", "cliSessionId":"cli"})), vec!["host", "cli"]);
        assert!(super::scheduled_claude_ids_from_record(&json!({"sessionId":"host"})).is_empty());
        assert!(super::scheduled_claude_ids_from_record(&json!({"scheduledTaskId":"task", "sessionId":"host", "spawnedFrom":{"taskId":"child"}})).is_empty());
    }
    #[test]
    fn codex_steps_preserve_provider_identity_and_details() {
        let rows = serde_json::json!([{ "id": "item-1", "turn": "turn-1", "type": "agentMessage", "item": r#"{"text":"Hello"}"# }]);
        let steps = super::codex_steps(&serde_json::Value::String(rows.to_string()));
        assert_eq!(steps.len(), 1);
        assert_eq!(steps[0].signature, "provider:turn-1:item-1");
        assert_eq!(steps[0].kind, "Reply");
        assert_eq!(steps[0].text, "Hello");
    }

    #[test]
    fn scheduled_claude_snapshot_requires_recent_unarchived_explicit_record() {
        use serde_json::json;
        let v = json!({"sessionId":"local_fake-123", "cliSessionId":"cli", "scheduledTaskId":"task", "lastActivityAt":1_794_400_000_000.0, "title":"Synthetic"});
        let (_, at, body) = super::scheduled_claude_snapshot(&v, 1_794_400_060.0).unwrap();
        assert_eq!(at, 1_794_400_000.0);
        assert!(body.scheduled);
        assert_eq!(body.kind, crate::observation::ObservationKind::Discovery);
        assert_eq!(body.recorded_at, Some(at));
        assert_eq!(body.aliases, vec!["cli"]);
        assert_eq!(body.activity, Some(crate::model::Activity::Idle));
        assert!(super::scheduled_claude_snapshot(&v, 1_794_414_400.0).is_some());
        assert!(super::scheduled_claude_snapshot(&v, 1_794_443_200.0).is_none());
        let mut archived = v.clone(); archived["isArchived"] = json!(true);
        assert!(super::scheduled_claude_snapshot(&archived, 1_794_400_060.0).is_none());
        let mut child = v; child["spawnedFrom"] = json!({"taskId":"child"});
        assert!(super::scheduled_claude_snapshot(&child, 1_794_400_060.0).is_none());
    }
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

    #[test]
    fn pending_codex_user_input_tools_show_the_first_question() {
        for (tool, status, arguments, expected) in [
            ("request_user_input", "inProgress", serde_json::json!({"questions":[{"question":"Which region?"}]}), "Which region?"),
            ("mcp__codex__request_user_input_async", "running", serde_json::json!({"questions":[{"title":"Choose a format"}]}), "Choose a format"),
        ] {
            let item = serde_json::json!({"tool":tool, "status":status, "arguments":arguments});
            let (state, text, tool_name) = super::codex_live("mcpToolCall", &item);
            assert_eq!(state, "waitingQuestion", "tool={tool}");
            assert_eq!(text, expected, "tool={tool}");
            assert!(tool_name.is_empty(), "the waiting state should not be rendered as an ordinary tool: {tool}");
        }
    }

    #[test]
    fn completed_failed_and_cancelled_user_input_calls_remain_tool_updates() {
        for status in ["completed", "complete", "failed", "cancelled", "canceled"] {
            let item = serde_json::json!({
                "tool":"request_user_input",
                "status":status,
                "arguments":{"questions":[{"question":"Already answered?"}]}
            });
            let (state, _, tool_name) = super::codex_live("mcpToolCall", &item);
            assert_eq!(state, "tool", "status={status}");
            assert_eq!(tool_name, "request_user_input", "status={status}");
        }
    }

    #[test]
    fn similarly_named_unrelated_tools_do_not_become_waiting_questions() {
        for tool in ["my_request_user_input", "request_user_input_helper", "shell"] {
            let item = serde_json::json!({"tool":tool, "status":"running", "arguments":{"questions":[{"question":"Not a prompt"}]}});
            let (state, _, tool_name) = super::codex_live("mcpToolCall", &item);
            assert_eq!(state, "tool", "tool={tool}");
            assert_eq!(tool_name, tool, "tool={tool}");
        }
    }
}
