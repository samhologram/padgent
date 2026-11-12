//! Typed facts delivered by trackers; JSON is decoded at the compatibility boundary.
use crate::model::{hook_events, Activity, AgentDef, Incoming, SessionStep};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Hook,
    LocalSnapshot,
    CloudSnapshot,
    DesktopLive,
    DesktopInactive,
    Other,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationEvent {
    TurnStarted,
    SubagentStopped,
    Update,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationKind {
    Update,
    Heartbeat,
    Discovery,
}

#[derive(Clone, Debug)]
pub struct Observation {
    pub agent: String,
    pub session: String,
    pub aliases: Vec<String>,
    pub source: Source,
    pub observed_at: f64,
    pub kind: ObservationKind,
    pub recorded_at: Option<f64>,
    pub activity: Option<Activity>,
    pub event: Option<ObservationEvent>,
    pub tool_name: Option<String>,
    pub text: Option<String>,
    pub text_at: Option<f64>,
    pub clear_text: bool,
    pub title: Option<String>,
    pub app: Option<String>,
    pub open_url: Option<String>,
    pub tty: Option<String>,
    pub pid: Option<i32>,
    pub term: Option<String>,
    pub steps: Option<Vec<SessionStep>>,
    pub unread: Option<u32>,
    pub scheduled: bool,
    pub step_id: Option<String>,
}
impl Observation {
    pub fn new(
        agent: impl Into<String>,
        session: impl Into<String>,
        source: Source,
        observed_at: f64,
    ) -> Self {
        Self {
            agent: agent.into(),
            session: session.into(),
            aliases: Vec::new(),
            source,
            observed_at,
            kind: ObservationKind::Update,
            recorded_at: None,
            activity: None,
            event: None,
            tool_name: None,
            text: None,
            text_at: None,
            clear_text: false,
            title: None,
            app: None,
            open_url: None,
            tty: None,
            pid: None,
            term: None,
            steps: None,
            unread: None,
            scheduled: false,
            step_id: None,
        }
    }
}

fn str_at<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| v.get(*k).and_then(Value::as_str))
        .filter(|s| !s.trim().is_empty())
}

pub(crate) fn first_line(s: &str, max: usize) -> String {
    let line = s
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let clean: String = line.replace("**", "").replace('`', "");
    clean.chars().take(max).collect()
}

pub fn decode_incoming(inc: Incoming, def: Option<&AgentDef>, t: f64) -> Observation {
    let agent_id = inc.agent.trim().to_string();
    let b = &inc.body;
    let sid = str_at(
        b,
        &[
            "session_id",
            "session",
            "sessionId",
            "thread_id",
            "conversation_id",
            "id",
        ],
    )
    .unwrap_or("default")
    .to_string();
    let event = inc
        .event
        .clone()
        .or_else(|| str_at(b, &["hook_event_name", "event"]).map(str::to_string));

    // Work out the new state.
    let tool_name = str_at(b, &["tool_name", "tool"]).unwrap_or("");
    let mut activity = str_at(b, &["state", "activity", "status"]).and_then(Activity::parse);
    if activity.is_none() {
        if let Some(ev) = &event {
            let mapped = def
                .and_then(|d| d.events.get(ev))
                .cloned()
                .or_else(|| hook_events().get(ev).cloned());
            activity = mapped
                .as_deref()
                .and_then(Activity::parse)
                .or_else(|| Activity::parse(ev));
        }
    }
    if event.as_deref() == Some("Notification") {
        // SessionRecord.resolve: a notification about input, a question or an
        // elicitation waits for an answer; anything else waits for approval.
        let msg = [
            str_at(b, &["message"]),
            str_at(b, &["notification_type", "type"]),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase();
        activity = Some(
            if ["input", "question", "elicitation"]
                .iter()
                .any(|w| msg.contains(w))
            {
                Activity::WaitingQuestion
            } else {
                Activity::WaitingPermission
            },
        );
    }
    if event.as_deref() == Some("PreToolUse") && tool_name == "AskUserQuestion" {
        activity = Some(Activity::WaitingQuestion);
    }
    // Live text.
    let text = str_at(b, &["text", "message", "last_assistant_message", "summary"])
        .map(|s| first_line(s, 240))
        .or_else(|| {
            if activity == Some(Activity::Tool) {
                let input = b.get("tool_input").cloned().unwrap_or(Value::Null);
                let detail = str_at(
                    &input,
                    &[
                        "command",
                        "file_path",
                        "pattern",
                        "url",
                        "query",
                        "description",
                        "path",
                    ],
                )
                .map(|s| first_line(s, 200))
                .unwrap_or_default();
                Some(if detail.is_empty() {
                    tool_name.to_string()
                } else {
                    format!("{tool_name} {detail}")
                })
            } else {
                str_at(b, &["prompt"]).map(|s| first_line(s, 240))
            }
        });
    let title = str_at(b, &["title", "name"])
        .map(|s| first_line(s, 60))
        .or_else(|| {
            str_at(b, &["cwd", "workspace", "project"]).map(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .map(|f| f.to_string_lossy().to_string())
                    .unwrap_or_else(|| p.to_string())
            })
        });

    let mut obs = Observation::new(
        agent_id,
        sid,
        if b.get("_local_snapshot").and_then(Value::as_bool) == Some(true) {
            Source::LocalSnapshot
        } else {
            Source::Other
        },
        t,
    );
    obs.activity = activity;
    obs.event = event.as_deref().map(|event| match event {
        "UserPromptSubmit" => ObservationEvent::TurnStarted,
        "SubagentStop" => ObservationEvent::SubagentStopped,
        _ => ObservationEvent::Update,
    });
    obs.tool_name = (!tool_name.is_empty()).then(|| tool_name.to_string());
    obs.text = text;
    obs.title = title;
    obs.text_at = b.get("_text_at").and_then(Value::as_f64);
    obs.clear_text = b.get("_clear_text").and_then(Value::as_bool) == Some(true);
    obs.unread = b
        .get("_unread")
        .and_then(Value::as_u64)
        .map(|n| n.min(999) as u32);
    obs.scheduled = b.get("_scheduled").and_then(Value::as_bool) == Some(true);
    obs.app = str_at(b, &["_app"]).map(str::to_string);
    obs.tty = str_at(b, &["_tty"]).map(str::to_string);
    obs.pid = str_at(b, &["_pid"]).and_then(|v| v.trim().parse().ok());
    obs.term = str_at(b, &["_term"]).map(str::to_string);
    obs.step_id =
        str_at(b, &["tool_use_id", "item_id", "message_id", "event_id"]).map(str::to_string);
    obs.steps = b.get("_steps").and_then(Value::as_array).map(|steps| {
        steps
            .iter()
            .take(128)
            .filter_map(|step| {
                let id = str_at(step, &["id"])?;
                Some(SessionStep {
                    kind: str_at(step, &["kind"]).unwrap_or("Update").to_string(),
                    status: str_at(step, &["status"]).unwrap_or("").to_string(),
                    title: str_at(step, &["title"]).unwrap_or("").to_string(),
                    text: str_at(step, &["text"]).unwrap_or("").to_string(),
                    signature: format!("provider:{id}"),
                })
            })
            .collect()
    });
    obs
}
