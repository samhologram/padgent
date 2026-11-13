//! Read-only Codex Cloud task inventory via the user's installed, authenticated CLI.
//! This is the first page (up to 20 tasks), not the account's full conversation history.
//! The CLI may omit tasks or environments it cannot list. We neither log in nor install it.
use crate::model::{Activity, Core, REMOVE_AFTER_S};
use crate::observation::{Observation, ObservationKind, Source};
use serde::Deserialize;
use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Deserialize)]
struct Page {
    tasks: Vec<Task>,
}
#[derive(Deserialize)]
struct Task {
    id: String,
    url: String,
    title: String,
    status: String,
    updated_at: String,
}

// Exact URL and identifier match also reject userinfo, ports, redirects and URL escapes.
pub fn trusted_task_url(id: &str, url: &str) -> bool {
    id.starts_with("task_")
        && id.len() <= 200
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && url == format!("https://chatgpt.com/codex/tasks/{id}")
}

fn activity(status: &str) -> Option<Activity> {
    // Codex's cloud-tasks-client maps completed to Ready and in-progress to Pending:
    // https://github.com/openai/codex/blob/main/codex-rs/cloud-tasks-client/src/http.rs
    match status {
        "pending" => Some(Activity::Thinking),
        "ready" | "applied" => Some(Activity::Done),
        "error" => Some(Activity::Error),
        _ => None,
    }
}

#[derive(Default)]
struct Tracker {
    last: HashMap<String, (String, String)>,
}
impl Tracker {
    fn parse(&mut self, json: &[u8], now: f64, recency: f64) -> Option<Vec<Observation>> {
        let page: Page = serde_json::from_slice(json).ok()?;
        let mut observations = Vec::new();
        for task in page.tasks {
            let Some(state) = activity(&task.status) else {
                continue;
            };
            if !trusted_task_url(&task.id, &task.url) {
                continue;
            }
            let Ok(updated) = chrono::DateTime::parse_from_rfc3339(&task.updated_at) else {
                continue;
            };
            let updated = updated.timestamp_millis() as f64 / 1000.0;
            if updated <= 0.0 || updated > now + 300.0 {
                continue;
            }
            let fingerprint = (task.status.clone(), task.updated_at.clone());
            let previous = self.last.get(&task.id);
            if previous == Some(&fingerprint) {
                continue;
            }
            if previous.is_none()
                && state != Activity::Thinking
                && now - updated > recency.min(REMOVE_AFTER_S)
            {
                continue;
            }
            let mut obs = Observation::new("codex", &task.id, Source::CloudSnapshot, now);
            obs.recorded_at = Some(updated.min(now));
            if previous.is_none() {
                obs.kind = ObservationKind::Discovery;
            }
            obs.activity = Some(state);
            obs.title = Some(task.title);
            obs.open_url = Some(task.url);
            self.last.insert(task.id, fingerprint);
            observations.push(obs);
        }
        Some(observations)
    }
}

fn list() -> Option<Vec<u8>> {
    // GUI launch environments often lack Homebrew in PATH.
    let executable = [
        std::path::PathBuf::from("/opt/homebrew/bin/codex"),
        std::path::PathBuf::from("/usr/local/bin/codex"),
        dirs::home_dir()
            .unwrap_or_default()
            .join(".local/bin/codex"),
    ]
    .into_iter()
    .find(|p| p.is_file())
    .unwrap_or_else(|| "codex".into());
    list_with_executable(&executable)
}

fn list_with_executable(executable: &std::path::Path) -> Option<Vec<u8>> {
    list_with_timeout(executable, Duration::from_secs(10))
}

fn list_with_timeout(executable: &std::path::Path, timeout: Duration) -> Option<Vec<u8>> {
    use std::os::unix::process::CommandExt;
    let deadline = Instant::now() + timeout;
    let mut child = Command::new(executable)
        .args(["cloud", "list", "--json", "--limit", "20"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let group = child.id() as i32;
    let stdout = child.stdout.take()?;
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let result = stdout.take(2_000_001).read_to_end(&mut out).map(|_| out);
        let _ = sender.send(result);
    });
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(
                    Duration::from_millis(20)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            _ => break None,
        }
    };
    let output = status.filter(|s| s.success()).and_then(|_| {
        receiver
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()?
            .ok()
    });
    if output.is_none() {
        // The child has its own group, so this cannot signal existing Codex sessions.
        // It also closes stdout held open by any wrapper descendants.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
        if status.is_none() {
            // Reap off-thread so even an unresponsive process cannot block the poller.
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        }
    }
    output.filter(|out| out.len() <= 2_000_000)
}

pub fn start(core: Arc<Mutex<Core>>, wake: Arc<dyn Fn() + Send + Sync>) {
    let _ = std::thread::Builder::new()
        .name("codex-cloud".into())
        .spawn(move || {
            let mut tracker = Tracker::default();
            loop {
                let (enabled, recency) = match core.lock() {
                    Ok(c) => (
                        c.agent_enabled("codex"),
                        c.settings.recency_minutes as f64 * 60.0,
                    ),
                    Err(_) => return,
                };
                if enabled {
                    if let Some(bytes) = list() {
                        if let Some(events) = tracker.parse(&bytes, crate::model::now(), recency) {
                            if !events.is_empty() {
                                if let Ok(mut c) = core.lock() {
                                    for event in events {
                                        c.observe(event);
                                    }
                                }
                                wake();
                            }
                        }
                    }
                }
                std::thread::sleep(Duration::from_secs(30));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Settings;

    fn page(status: &str, updated: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({"tasks":[{"id":"task_e_fake123", "url":"https://chatgpt.com/codex/tasks/task_e_fake123", "title":"Example", "status":status, "summary":{"files_changed":0,"lines_added":0,"lines_removed":0}, "updated_at":updated,"attempt_total":1,"environment_id":null,"environment_label":null,"is_review":false}],"cursor":null})).unwrap()
    }
    #[test]
    fn cli_invocation_respects_list_limit_contract() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("padgent-cloud-fake-{}", std::process::id()));
        std::fs::write(&path, "#!/bin/sh\n[ \"$1\" = cloud ] && [ \"$2\" = list ] && [ \"$3\" = --json ] && [ \"$4\" = --limit ] && [ \"$5\" -le 20 ] || exit 2\nprintf '{\"tasks\":[],\"cursor\":null}'\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = list_with_executable(&path);
        let _ = std::fs::remove_file(&path);
        assert!(
            Tracker::default()
                .parse(&result.expect("valid invocation"), 1_800_000_000.0, 7200.0)
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn child_holding_stdout_cannot_block_polling() {
        use std::os::unix::fs::PermissionsExt;
        let path = std::env::temp_dir().join(format!("padgent-cloud-hold-{}", std::process::id()));
        std::fs::write(&path, "#!/bin/sh\nsleep 30 &\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        let start = Instant::now();
        assert!(list_with_timeout(&path, Duration::from_millis(100)).is_none());
        assert!(start.elapsed() < Duration::from_secs(2));
        let _ = std::fs::remove_file(&path);
    }
    #[test]
    fn exact_url_and_unknown_status_are_rejected() {
        assert!(trusted_task_url(
            "task_e_fake123",
            "https://chatgpt.com/codex/tasks/task_e_fake123"
        ));
        assert!(!trusted_task_url(
            "task_e_fake123",
            "https://chatgpt.com.evil.test/codex/tasks/task_e_fake123"
        ));
        assert!(!trusted_task_url(
            "task_e_fake123",
            "https://chatgpt.com/codex/tasks/task_e_fake123?x=1"
        ));
        assert!(
            Tracker::default()
                .parse(
                    &page("mystery", "2026-11-12T00:00:00Z"),
                    1_800_000_000.0,
                    7200.0
                )
                .unwrap()
                .is_empty()
        );
    }
    #[test]
    fn changes_only_and_open_target_survive_acknowledgement() {
        let mut tracker = Tracker::default();
        let mut core = Core::new(Settings::default());
        let at = chrono::DateTime::parse_from_rfc3339("2026-11-12T00:00:00Z")
            .unwrap()
            .timestamp() as f64;
        for o in tracker
            .parse(&page("pending", "2026-11-12T00:00:00Z"), at, 7200.0)
            .unwrap()
        {
            core.observe(o);
        }
        assert_eq!(core.sessions[0].activity, Activity::Thinking);
        assert!(!core.sessions[0].scheduled, "cloud task provenance alone is not proof of a scheduled run");
        for o in tracker
            .parse(&page("ready", "2026-11-12T00:00:01Z"), at + 1.0, 7200.0)
            .unwrap()
        {
            core.observe(o);
        }
        assert_eq!(core.sessions[0].activity, Activity::Done);
        assert!(core.acknowledge_open("codex", "task_e_fake123").is_some());
        assert!(
            tracker
                .parse(&page("ready", "2026-11-12T00:00:01Z"), at + 30.0, 7200.0)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            core.sessions[0].open_url.as_deref(),
            Some("https://chatgpt.com/codex/tasks/task_e_fake123")
        );
        assert!(!core.sessions[0].pending_attention);
        for o in tracker
            .parse(&page("ready", "2026-11-12T00:00:02Z"), at + 2.0, 7200.0)
            .unwrap()
        {
            core.observe(o);
        }
        assert!(core.sessions[0].acknowledged, "a metadata refresh for the same completed task must not reset its shelf state");
        assert!(!core.sessions[0].pending_attention);
        assert!(
            Tracker::default()
                .parse(&page("ready", "2026-11-12T00:00:00Z"), at + 8000.0, 7200.0)
                .unwrap()
                .is_empty()
        );
    }
}
