//! Agents, settings, sessions and the keypad core shared by the window, the menu bar,
//! the status receiver and the physical keypad driver. Numbers come from MECHANICS.md.

use crate::colour::{self, Rgb};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

// ---- keypad geometry (KeypadGeometry, 0x10042b24c..) ----
pub const KEY_COUNT: usize = 9;
pub const KEY_SIZE: u32 = 118;
pub const ORIGIN_X: u32 = 23;
pub const ORIGIN_Y: u32 = 6;
pub const PITCH: u32 = 158;
pub const HUB_KEY: usize = 8;
pub const SESSION_KEYS_PER_PAGE: usize = 8;
/// Idle sessions older than this stay available, but start no earlier than page two.
pub const IDLE_SECOND_PAGE_AFTER_S: f64 = 30.0 * 60.0;
/// Hold a session key to move it to the end until work resumes.
pub const ARCHIVE_HOLD_S: f64 = 0.6;
pub const HAND_BACK_HOLD_S: f64 = 1.5;
// Session timings (SessionTracker.Environment.live 0x10075703c → SessionTimings, applied by
// SessionRecord.resolve 0x100749a48).
pub const STREAMING_WINDOW_S: f64 = 1.5;
pub const DONE_DECAY_S: f64 = 180.0;
pub const STARTING_WINDOW_S: f64 = 60.0;
pub const STALE_BUSY_AFTER_S: f64 = 900.0;
pub const STALE_IDLE_AFTER_S: f64 = 7200.0;
pub const REMOVE_AFTER_S: f64 = 43200.0;
pub const SUBAGENT_FORGET_AFTER_S: f64 = 1800.0;
/// SubagentSnapshot.doneDecaySeconds (0x1004644d4).
pub const SUBAGENT_DONE_DECAY_S: f64 = 30.0;
/// Estimated: how long a key keeps showing the last message before it's only the light.
pub const RECENT_TEXT_S: f64 = 600.0;
/// KeyMemory.advanced (0x1005000d8): text on a key stays at least 1.5 s before the next
/// replaces it (respectDwell).
pub const TEXT_DWELL_S: f64 = 1.5;

/// Apps whose sessions come from their own records (local.rs), never from their windows.
/// (Claude's chats have no local record, so its window is still followed while it writes.)
pub const NATIVE_SOURCES: [&str; 1] = ["com.openai.codex"];

/// What a key shows under its name: the recent last message, else unread replies, else only
/// a waiting or error state; idle and working keys show just their light.
pub fn key_text(text: &str, unread: u32, activity: Activity) -> String {
    if !text.is_empty() && activity == Activity::Thinking {
        format!("Thinking… {text}")
    } else if !text.is_empty() && activity == Activity::WaitingQuestion {
        format!("Question · {text}")
    } else if !text.is_empty() {
        text.to_string()
    } else if activity == Activity::Thinking {
        "Thinking…".to_string()
    } else if unread > 0 {
        format!("{unread} unread")
    } else if matches!(activity.group(), Group::Waiting | Group::Error) {
        activity.label().to_string()
    } else {
        String::new()
    }
}

/// PhaseText.clockText: whole minutes since `since`: nothing under a minute, then "Nm ago",
/// "Nh ago" from 60 minutes, "Nd ago" from 1,440 minutes.
pub fn age_text(since: f64, now: f64) -> String {
    let minutes = ((now - since).max(0.0) / 60.0) as i64;
    match minutes {
        m if m < 1 => String::new(),
        m if m < 60 => format!("{m}m ago"),
        m if m < 1440 => format!("{}h ago", m / 60),
        m => format!("{}d ago", m / 1440),
    }
}

/// Compact age for a tile corner: "33m", "1h", "2d" (nothing under a minute).
pub fn age_compact(since: f64, now: f64) -> String {
    let m = ((now - since).max(0.0) / 60.0) as i64;
    match m {
        m if m < 1 => String::new(),
        m if m < 60 => format!("{m}m"),
        m if m < 1440 => format!("{}h", m / 60),
        m => format!("{}d", m / 1440),
    }
}

/// "33 min ago", "1 h ago", "2 d ago".
pub fn age_long(since: f64, now: f64) -> String {
    let m = ((now - since).max(0.0) / 60.0) as i64;
    match m {
        m if m < 1 => "just now".into(),
        m if m < 60 => format!("{m} min ago"),
        m if m < 1440 => format!("{} h ago", m / 60),
        m => format!("{} d ago", m / 1440),
    }
}

pub fn now() -> f64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0)
}

// ---------------------------------------------------------------- activity

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize, Hash)]
#[serde(rename_all = "camelCase")]
pub enum Activity {
    Empty,
    Starting,
    Idle,
    Thinking,
    Tool,
    Streaming,
    WaitingPermission,
    WaitingQuestion,
    Done,
    Error,
    Compacting,
    Offline,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Group {
    Waiting,
    Working,
    Done,
    Idle,
    Error,
    Offline,
}

impl Activity {
    pub fn group(self) -> Group {
        use Activity::*;
        match self {
            WaitingPermission | WaitingQuestion => Group::Waiting,
            Starting | Thinking | Tool | Streaming | Compacting => Group::Working,
            Done => Group::Done,
            Error => Group::Error,
            Offline => Group::Offline,
            Empty | Idle => Group::Idle,
        }
    }

    /// Keys sort by state: waiting, working, just finished, then idle.
    pub fn rank(self) -> u8 {
        match self.group() {
            Group::Waiting => 0,
            Group::Working => 1,
            Group::Error => 2,
            Group::Done => 2,
            Group::Idle => 3,
            Group::Offline => 4,
        }
    }

    /// Text shown for each session activity.
    pub fn label(self) -> &'static str {
        use Activity::*;
        match self {
            Empty => "Empty",
            Starting => "Starting",
            Idle => "Idle",
            Thinking => "Thinking",
            Tool => "Tool",
            Streaming => "Writing",
            WaitingPermission => "Waiting for approval",
            WaitingQuestion => "Question",
            Done => "Done",
            Error => "Error",
            Compacting => "Compacting",
            Offline => "Offline",
        }
    }

    pub fn parse(s: &str) -> Option<Activity> {
        let s = s.trim().to_ascii_lowercase().replace(['_', '-', ' '], "");
        Some(match s.as_str() {
            "empty" => Activity::Empty,
            "starting" | "start" => Activity::Starting,
            "idle" => Activity::Idle,
            "thinking" | "working" | "busy" | "running" => Activity::Thinking,
            "tool" => Activity::Tool,
            "streaming" | "writing" => Activity::Streaming,
            "waitingpermission" | "waiting" | "approval" | "permission" => Activity::WaitingPermission,
            "waitingquestion" | "question" => Activity::WaitingQuestion,
            "done" | "finished" | "stop" => Activity::Done,
            "error" | "failed" => Activity::Error,
            "compacting" => Activity::Compacting,
            "offline" | "ended" => Activity::Offline,
            _ => return None,
        })
    }

    pub fn animated(self) -> bool {
        matches!(
            self.group(),
            Group::Working | Group::Waiting
        ) || self == Activity::Done
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ToolKind {
    Shell,
    Edit,
    Read,
    Web,
    Delegate,
}

impl ToolKind {
    pub fn glyph(self) -> &'static str {
        match self {
            ToolKind::Shell => ">_",
            ToolKind::Edit => "+-",
            ToolKind::Read => "≡",
            ToolKind::Web => "@",
            ToolKind::Delegate => "»",
        }
    }

    pub fn from_tool_name(name: &str) -> ToolKind {
        let n = name.to_ascii_lowercase();
        if n.contains("bash") || n.contains("shell") || n.contains("exec") || n.contains("command") {
            ToolKind::Shell
        } else if n.contains("edit") || n.contains("write") || n.contains("patch") || n.contains("notebook") {
            ToolKind::Edit
        } else if n.contains("web") || n.contains("fetch") || n.contains("search") && n.contains("web") {
            ToolKind::Web
        } else if n == "task" || n.contains("agent") || n.contains("delegate") {
            ToolKind::Delegate
        } else {
            ToolKind::Read
        }
    }
}

// ---------------------------------------------------------------- agents

pub const AGENT_FORMAT: &str = "agentpad.agent";
pub const AGENTS_FORMAT: &str = "agentpad.agents";

fn default_format() -> String {
    AGENT_FORMAT.into()
}
fn one() -> u32 {
    1
}
fn orb_style() -> String {
    "orb".into()
}

/// An agent type. Built-ins ship with the app; others are imported/exported as
/// `.orbagent.json` files ("Includes its own icon and state colours" in the original).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct AgentDef {
    #[serde(default = "default_format")]
    pub format: String,
    #[serde(default = "one")]
    pub version: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub badge: String,
    /// OKLCH hue in degrees.
    pub hue: f32,
    #[serde(default)]
    pub description: String,
    /// Optional own colours per state group: working, waiting, done, idle (#rrggbb).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub state_colours: BTreeMap<String, String>,
    /// macOS app brought to the front when its key is tapped.
    #[serde(default)]
    pub app_name: String,
    /// Link opened when its key is tapped; `{session}` is replaced by the session id.
    #[serde(default)]
    pub open_url: String,
    /// Incoming event name → state (e.g. "PreToolUse": "tool").
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub events: BTreeMap<String, String>,
    /// Desktop app (bundle id) whose windows are followed through Accessibility: works for
    /// cloud and local conversations without any login in this app.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub watch_app: String,
    /// The followed app is built on Electron (needs its accessibility switched on).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub electron: bool,
    /// Key look: "orb" (a glowing light) or "dots" (three status dots, like GPT apps).
    #[serde(default = "orb_style")]
    pub style: String,
    /// Model per role (swarm, code, orchestration).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, String>,
    #[serde(skip)]
    pub builtin: bool,
}

fn hook_events() -> BTreeMap<String, String> {
    [
        ("SessionStart", "starting"),
        ("UserPromptSubmit", "thinking"),
        ("PreToolUse", "tool"),
        ("PostToolUse", "thinking"),
        ("SubagentStop", "thinking"),
        ("Notification", "waitingPermission"),
        ("PermissionRequest", "waitingPermission"),
        ("PreCompact", "compacting"),
        ("Stop", "done"),
        ("SessionEnd", "offline"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}

pub fn default_model_roles() -> BTreeMap<String, String> {
    [
        ("swarm", "claude-haiku-4-5-20251001"),
        ("code", "claude-sonnet-5"),
        ("orchestration", "claude-opus-5"),
    ]
    .into_iter()
    .map(|(a, b)| (a.to_string(), b.to_string()))
    .collect()
}

pub fn builtin_agents() -> Vec<AgentDef> {
    let mk = |id: &str, name: &str, badge: &str, hue: f32, app: &str, desc: &str| AgentDef {
        format: AGENT_FORMAT.into(),
        version: 1,
        id: id.into(),
        name: name.into(),
        badge: badge.into(),
        hue,
        description: desc.into(),
        state_colours: BTreeMap::new(),
        app_name: app.into(),
        open_url: String::new(),
        events: hook_events(),
        watch_app: String::new(),
        electron: false,
        style: orb_style(),
        models: BTreeMap::new(),
        builtin: true,
    };
    let mut claude = mk(
        "claude-code",
        "Claude Code",
        "CC",
        79.85,
        "Terminal",
        "Live state from Claude Code hooks: thinking, tools, approvals, done.",
    );
    claude.models = default_model_roles();
    let mut codex = mk("codex", "Codex", "CX", 270.0, "Terminal", "Live state from Codex hooks, shown as GPT-style status dots with the last message.");
    codex.style = "dots".into();
    let mut chatgpt = mk("chatgpt", "ChatGPT", "GP", 150.0, "ChatGPT", "Active and recent conversations in the ChatGPT app, shown as status dots with the last message.");
    chatgpt.style = "dots".into();
    chatgpt.watch_app = "com.openai.chat".into();
    let mut claude_app = mk("claude-desktop", "Claude", "CL", 95.0, "Claude", "The open standard conversation in the Claude desktop app. Claude Code sessions are tracked separately.");
    claude_app.watch_app = "com.anthropic.claudefordesktop".into();
    claude_app.electron = true;
    vec![
        claude,
        codex,
        mk("antigravity", "Antigravity", "AG", 225.0, "Antigravity", "Conversations from the Antigravity desktop app."),
        mk("zcode", "ZCode", "ZC", 165.0, "ZCode", "Tasks from ZCode."),
        mk("kimi-code", "Kimi Code", "KC", 324.0, "Terminal", "Sessions from Kimi Code."),
        chatgpt,
        claude_app,
    ]
}

pub fn badge_from_name(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().collect();
    let b: String = if words.len() >= 2 {
        words.iter().take(2).filter_map(|w| w.chars().next()).collect()
    } else {
        name.chars().filter(|c| c.is_alphanumeric()).take(2).collect()
    };
    b.to_uppercase()
}

/// Checks an imported agent the way the original checks adapters: reverse-DNS id,
/// names not reserved by built-ins, hue kept out of the waiting band.
pub fn validate_agent(mut a: AgentDef) -> Result<(AgentDef, Vec<String>), String> {
    let mut notes = Vec::new();
    let id = a.id.trim().to_string();
    let ok_chars = id.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '.' || c == '-' || c == '_');
    if !(3..=64).contains(&id.len()) || !id.contains('.') || !ok_chars {
        return Err(format!(
            "“{id}” isn't a valid agent id: use lowercase reverse-DNS with at least one dot, 3 to 64 characters (for example com.example.myagent)."
        ));
    }
    if builtin_agents().iter().any(|b| b.id == id) {
        return Err(format!("“{id}” is reserved for a built-in agent."));
    }
    a.id = id;
    a.name = a.name.trim().chars().take(40).collect();
    if a.name.is_empty() {
        return Err("The agent needs a name.".into());
    }
    if a.badge.trim().is_empty() {
        a.badge = badge_from_name(&a.name);
    }
    a.badge = a.badge.trim().chars().take(2).collect::<String>().to_uppercase();
    if !a.hue.is_finite() {
        a.hue = colour::calm_default();
    }
    let (h, moved) = colour::keep_out_of_attention_band(a.hue);
    if moved {
        notes.push("Colour adjusted to keep states distinct: that hue is kept for sessions waiting for you.".into());
    }
    a.hue = h;
    a.state_colours.retain(|k, v| {
        let keep = ["working", "waiting", "done", "idle"].contains(&k.as_str()) && Rgb::parse(v).is_some();
        if !keep {
            notes.push(format!("Ignored state colour “{k}”: a supported state with a #rrggbb colour is required."));
        }
        keep
    });
    if a.open_url.starts_with("file:") || a.open_url.starts_with("javascript:") || a.open_url.starts_with("data:") {
        a.open_url.clear();
        notes.push("Ignored the link: Padgent does not open this kind of link.".into());
    }
    if a.style != "dots" {
        a.style = "orb".into();
    }
    a.format = AGENT_FORMAT.into();
    a.version = 1;
    a.builtin = false;
    Ok((a, notes))
}

/// A new agent that follows a desktop app through Accessibility (from the "Add app" buttons).
pub fn agent_for_app(name: &str, bundle: &str) -> AgentDef {
    let mut id: String = bundle
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '-' })
        .collect();
    if !id.contains('.') {
        id = format!("app.{id}");
    }
    id.truncate(64);
    // A stable hue per app, inside the calm arc (80..280°) so it never looks like "waiting".
    let h = bundle.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32));
    let (lo, hi) = colour::CALM_ARC;
    let hue = lo + (h % 1000) as f32 / 1000.0 * (hi - lo);
    AgentDef {
        format: AGENT_FORMAT.into(),
        version: 1,
        id,
        name: name.chars().take(40).collect(),
        badge: badge_from_name(name),
        hue,
        description: format!("Conversations followed from the {name} app."),
        state_colours: BTreeMap::new(),
        app_name: name.to_string(),
        open_url: String::new(),
        events: BTreeMap::new(),
        watch_app: bundle.to_string(),
        electron: false,
        style: orb_style(),
        models: BTreeMap::new(),
        builtin: false,
    }
}

/// Reads one agent or a bundle of agents.
pub fn parse_agent_file(text: &str) -> Result<Vec<AgentDef>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("This file isn't valid JSON ({e})."))?;
    let list: Vec<Value> = if v.get("format").and_then(Value::as_str) == Some(AGENTS_FORMAT) {
        v.get("agents").and_then(Value::as_array).cloned().unwrap_or_default()
    } else if let Some(arr) = v.as_array() {
        arr.clone()
    } else {
        vec![v]
    };
    if list.is_empty() {
        return Err("This file holds no agents.".into());
    }
    list.into_iter()
        .map(|item| {
            if let Some(f) = item.get("format").and_then(Value::as_str) {
                if f != AGENT_FORMAT {
                    return Err("This file is not a Padgent agent document.".to_string());
                }
            }
            if item.get("version").and_then(Value::as_u64).unwrap_or(1) > 1 {
                return Err("This agent file uses a newer format. Update the app to import it.".to_string());
            }
            serde_json::from_value::<AgentDef>(item).map_err(|e| format!("This agent is missing something: {e}"))
        })
        .collect()
}

// ---------------------------------------------------------------- settings

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ColourKeysBy {
    Agent,
    State,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ReorderOn {
    Turns,
    All,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MenuIndicator {
    Off,
    Dot,
    DotAndCount,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Motion {
    Full,
    Reduced,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub format: String,
    pub version: u32,
    pub hook_port: u16,
    pub launch_at_login: bool,
    pub auto_take_over_on_launch: bool,
    pub target_fps: u32,
    pub brightness: f32,
    pub text_speed: f32,
    pub idle_dim_minutes: u32,
    pub key_actions_enabled: bool,
    pub recency_minutes: u32,
    pub reorder_on: ReorderOn,
    pub colour_keys_by: ColourKeysBy,
    pub key_glyphs: bool,
    pub empty_key_url: String,
    pub agent_hues: BTreeMap<String, f32>,
    pub disabled_agents: Vec<String>,
    pub state_colours: BTreeMap<String, String>,
    pub waiting_hue: f32,
    pub menu_indicator: MenuIndicator,
    pub motion: Motion,
    pub demo_mode: bool,
    pub drive_physical_keypad: bool,
    /// Per-agent override of the followed desktop app (bundle id).
    pub watch_apps: BTreeMap<String, String>,
    /// Code signature the Accessibility entry was last reset for
    /// ("accessibilityRecoveryRequirement" in the original).
    pub accessibility_recovery_requirement: String,
    pub model_roles: BTreeMap<String, String>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            format: "agentpad.settings".into(),
            version: 1,
            hook_port: 47800,
            launch_at_login: false,
            auto_take_over_on_launch: true,
            target_fps: 30,
            brightness: 0.8,
            text_speed: 18.0,
            idle_dim_minutes: 10,
            key_actions_enabled: true,
            recency_minutes: 120,
            reorder_on: ReorderOn::Turns,
            colour_keys_by: ColourKeysBy::Agent,
            key_glyphs: true,
            empty_key_url: String::new(),
            agent_hues: BTreeMap::new(),
            disabled_agents: Vec::new(),
            state_colours: [
                ("working", "#4aa3ff"),
                ("done", "#3fd27c"),
                ("idle", "#8e8e93"),
            ]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect(),
            waiting_hue: colour::REFERENCE_WAITING_HUE,
            menu_indicator: MenuIndicator::DotAndCount,
            motion: Motion::Full,
            demo_mode: false,
            // Take the keypad from Logi Options+ when the app opens, as the original does.
            drive_physical_keypad: true,
            watch_apps: BTreeMap::new(),
            accessibility_recovery_requirement: String::new(),
            model_roles: default_model_roles(),
        }
    }
}

pub fn data_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(std::env::temp_dir);
    base.join("AgentPad")
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read_to_string(data_dir().join("settings.json"))
            .ok()
            .and_then(|t| serde_json::from_str::<Settings>(&t).ok())
            .map(|mut settings| {
                settings.format = "agentpad.settings".into();
                settings
            })
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let dir = data_dir();
        let _ = std::fs::create_dir_all(&dir);
        if let Ok(t) = serde_json::to_string_pretty(self) {
            let tmp = dir.join("settings.json.tmp");
            if std::fs::write(&tmp, t).is_ok() {
                let _ = std::fs::rename(tmp, dir.join("settings.json"));
            }
        }
    }
}

// Stored definitions may have an earlier marker; external imports still require the current one.
fn parse_stored_agents(text: &str) -> Result<Vec<AgentDef>, String> {
    fn normalize_agent(value: &mut Value) {
        if let Some(object) = value.as_object_mut() {
            object.insert("format".into(), Value::String(AGENT_FORMAT.into()));
        }
    }

    let mut value: Value = serde_json::from_str(text).map_err(|e| format!("This file isn't valid JSON ({e})."))?;
    if let Some(agents) = value.as_array_mut() {
        for agent in agents { normalize_agent(agent); }
    } else if let Some(object) = value.as_object_mut() {
        if object.get("agents").is_some_and(Value::is_array) {
            object.insert("format".into(), Value::String(AGENTS_FORMAT.into()));
            if let Some(agents) = object.get_mut("agents").and_then(Value::as_array_mut) {
                for agent in agents { normalize_agent(agent); }
            }
        } else {
            normalize_agent(&mut value);
        }
    }
    parse_agent_file(&value.to_string())
}

pub fn load_custom_agents() -> Vec<AgentDef> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(data_dir().join("agents")) {
        for e in rd.flatten() {
            if let Ok(t) = std::fs::read_to_string(e.path()) {
                if let Ok(list) = parse_stored_agents(&t) {
                    for a in list {
                        if let Ok((a, _)) = validate_agent(a) {
                            out.push(a);
                        }
                    }
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

pub fn save_custom_agent(a: &AgentDef) -> std::io::Result<()> {
    let dir = data_dir().join("agents");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join(format!("{}.orbagent.json", a.id)), serde_json::to_string_pretty(a).unwrap_or_default())
}

pub fn delete_custom_agent(id: &str) {
    let _ = std::fs::remove_file(data_dir().join("agents").join(format!("{id}.orbagent.json")));
}

// ---------------------------------------------------------------- sessions

#[derive(Clone, Debug)]
pub struct Session {
    pub agent: String,
    pub id: String,
    pub title: String,
    pub activity: Activity,
    pub tool: Option<ToolKind>,
    pub text: String,
    pub text_at: f64,
    pub since: f64,
    pub updated: f64,
    pub turn_started: f64,
    pub subagents: u32,
    pub subagent_at: f64,
    /// Desktop app (bundle id) showing this conversation, for opening it exactly.
    pub app_bundle: String,
    /// Terminal device and program the agent runs in (from the hook's X-TTY / X-Term).
    pub tty: String,
    pub term: String,
    /// The agent's process (from the hook's X-PID), used to find the app it runs in.
    pub pid: i32,
    /// Replies not yet read in the agent's own app (Codex read receipts).
    pub unread: u32,
    /// Placed after normal sessions until a new turn or resumed work.
    pub archived: bool,
    /// Text waiting for the current one's 1.5 s dwell to pass.
    pub pending_text: Option<String>,
}

impl Session {
    pub fn key(&self) -> String {
        format!("{}:{}", self.agent, self.id)
    }
}

/// One update from an agent (hook, generic event, or demo).
#[derive(Clone, Debug)]
pub struct Incoming {
    pub agent: String,
    pub event: Option<String>,
    pub body: Value,
}

fn str_at<'a>(v: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| v.get(*k).and_then(Value::as_str)).filter(|s| !s.trim().is_empty())
}

fn first_line(s: &str, max: usize) -> String {
    let line = s.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let clean: String = line.replace("**", "").replace('`', "");
    clean.chars().take(max).collect()
}

// ---------------------------------------------------------------- takeover

#[derive(Clone, Debug, PartialEq)]
pub enum Hardware {
    /// The physical keypad driver is switched off: the keypad is shown on screen only.
    Off,
    NoKeypad,
    Connected,
    Driving,
    Problem(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum KeyFace {
    Empty,
    Released,
    Session(SessionFace),
    Hub(HubFace),
    Quota(crate::quota::Quota),
    QuotaHub,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SessionFace {
    pub key: String,
    pub badge: String,
    pub agent_name: String,
    pub title: String,
    pub text: String,
    pub activity: Activity,
    pub tool: Option<ToolKind>,
    pub orb: Rgb,
    pub glow: Rgb,
    pub since: f64,
    pub subagents: u32,
    pub dots: bool,
    pub unread: u32,
    /// The last message whatever its age (shown dim on the tile).
    pub last_message: String,
    /// When the session last did something (for "last turn 33 min ago").
    pub last_turn: f64,
    /// The agent's own colour (badge).
    pub agent_colour: Rgb,
}

#[derive(Clone, Debug, PartialEq)]
pub struct HubFace {
    pub waiting: usize,
    pub working: usize,
    pub done: usize,
    pub page: usize,
    pub pages: usize,
    pub dots: Vec<Rgb>,
    pub waiting_colour: Rgb,
    pub dimmed: bool,
    /// Sessions per agent: (name, colour, count).
    pub agents: Vec<(String, Rgb, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyPressTarget {
    Session(String),
    Hub,
    Empty,
    Disabled,
}

/// Resolve an action from the face actually shown, independent of later reordering.
pub fn target_for_face(face: &KeyFace, quota_view: bool) -> KeyPressTarget {
    if quota_view { return KeyPressTarget::Disabled; }
    match face {
        KeyFace::Session(s) => KeyPressTarget::Session(s.key.clone()),
        KeyFace::Hub(_) => KeyPressTarget::Hub,
        KeyFace::Empty => KeyPressTarget::Empty,
        KeyFace::Released | KeyFace::Quota(_) | KeyFace::QuotaHub => KeyPressTarget::Disabled,
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Open { agent: String, session: String },
    OpenUrl(String),
    ShowWindow,
}

pub struct Core {
    pub settings: Settings,
    pub agents: Vec<AgentDef>,
    pub sessions: Vec<Session>,
    order: Vec<String>,
    pub page: usize,
    pub quota_view: bool,
    pub quota_page: usize,
    pub quotas: Vec<crate::quota::Quota>,
    pub in_control: bool,
    pub hardware: Hardware,
    key_down: [Option<f64>; KEY_COUNT],
    key_target: [Option<KeyPressTarget>; KEY_COUNT],
    hold_fired: [bool; KEY_COUNT],
    page_down: [Option<f64>; 2],
    pub actions: Vec<Action>,
    pub revision: u64,
    pub notice: Option<(String, f64)>,
    pub server_status: String,
    /// The Accessibility reset failed: show the remove-and-add steps.
    pub accessibility_reset_failed: bool,
    last_change: f64,
}

impl Core {
    pub fn new(settings: Settings) -> Core {
        let mut agents = builtin_agents();
        agents.extend(load_custom_agents());
        let in_control = settings.auto_take_over_on_launch;
        Core {
            settings,
            agents,
            sessions: Vec::new(),
            order: Vec::new(),
            page: 0,
            quota_view: false,
            quota_page: 0,
            quotas: Vec::new(),
            in_control,
            hardware: Hardware::Off,
            key_down: [None; KEY_COUNT],
            key_target: std::array::from_fn(|_| None),
            hold_fired: [false; KEY_COUNT],
            page_down: [None; 2],
            actions: Vec::new(),
            revision: 0,
            notice: None,
            server_status: String::new(),
            accessibility_reset_failed: false,
            last_change: now(),
        }
    }

    pub fn agent(&self, id: &str) -> Option<&AgentDef> {
        self.agents.iter().find(|a| a.id == id)
    }

    /// Desktop apps to follow: (agent id, bundle id, electron).
    pub fn watched_apps(&self) -> Vec<(String, String, bool)> {
        self.agents
            .iter()
            .filter(|a| self.agent_enabled(&a.id))
            .filter_map(|a| {
                let b = self.settings.watch_apps.get(&a.id).cloned().unwrap_or_else(|| a.watch_app.clone());
                let b = b.trim().to_string();
                if NATIVE_SOURCES.contains(&b.as_str()) {
                    return None;
                }
                let electron = a.electron || b.contains("claude") || b.contains("antigravity");
                (!b.is_empty()).then(|| (a.id.clone(), b, electron))
            })
            .collect()
    }

    pub fn agent_enabled(&self, id: &str) -> bool {
        !self.settings.disabled_agents.iter().any(|d| d == id)
    }

    pub fn notify(&mut self, text: impl Into<String>) {
        self.notice = Some((text.into(), now()));
        self.revision += 1;
    }

    fn touch(&mut self) {
        self.revision += 1;
        self.last_change = now();
    }

    // ---- intake

    pub fn apply(&mut self, inc: Incoming) {
        let t = now();
        let agent_id = inc.agent.trim().to_string();
        let def = self.agent(&agent_id).cloned();
        let b = &inc.body;
        let sid = str_at(b, &["session_id", "session", "sessionId", "thread_id", "conversation_id", "id"])
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
                    .as_ref()
                    .and_then(|d| d.events.get(ev))
                    .cloned()
                    .or_else(|| hook_events().get(ev).cloned());
                activity = mapped.as_deref().and_then(Activity::parse).or_else(|| Activity::parse(ev));
            }
        }
        if event.as_deref() == Some("Notification") {
            // SessionRecord.resolve: a notification about input, a question or an
            // elicitation waits for an answer; anything else waits for approval.
            let msg = [str_at(b, &["message"]), str_at(b, &["notification_type", "type"])]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" ")
                .to_lowercase();
            activity = Some(if ["input", "question", "elicitation"].iter().any(|w| msg.contains(w)) {
                Activity::WaitingQuestion
            } else {
                Activity::WaitingPermission
            });
        }
        if event.as_deref() == Some("PreToolUse") && tool_name == "AskUserQuestion" {
            activity = Some(Activity::WaitingQuestion);
        }
        let Some(activity) = activity else { return };

        // Live text.
        let text = str_at(b, &["text", "message", "last_assistant_message", "summary"])
            .map(|s| first_line(s, 240))
            .or_else(|| {
                if activity == Activity::Tool {
                    let input = b.get("tool_input").cloned().unwrap_or(Value::Null);
                    let detail = str_at(&input, &["command", "file_path", "pattern", "url", "query", "description", "path"])
                        .map(|s| first_line(s, 200))
                        .unwrap_or_default();
                    Some(if detail.is_empty() { tool_name.to_string() } else { format!("{tool_name} {detail}") })
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

        let idx = self.sessions.iter().position(|s| s.agent == agent_id && s.id == sid);
        let new_session = idx.is_none();
        let idx = match idx {
            Some(i) => i,
            None => {
                if activity == Activity::Offline {
                    return;
                }
                let name = def.as_ref().map(|d| d.name.clone()).unwrap_or_else(|| agent_id.clone());
                self.sessions.push(Session {
                    agent: agent_id.clone(),
                    id: sid.clone(),
                    title: name,
                    activity: Activity::Starting,
                    tool: None,
                    text: String::new(),
                    text_at: t,
                    since: t,
                    updated: t,
                    turn_started: t,
                    subagents: 0,
                    subagent_at: t,
                    app_bundle: String::new(),
                    tty: String::new(),
                    term: String::new(),
                    pid: 0,
                    unread: 0,
                    archived: false,
                    pending_text: None,
                });
                self.sessions.len() - 1
            }
        };
        let s = &mut self.sessions[idx];
        let old = s.activity;
        // Read-receipt changes do not turn an already idle completed Codex thread into
        // a newly completed turn. Its local record can repeat the same final message.
        let activity = if old == Activity::Idle && activity == Activity::Done && event.is_none()
            && b.get("_unread").is_some() && text.as_deref() == Some(s.text.as_str())
            && b.get("_text_at").and_then(Value::as_f64).is_some_and(|at| at > 0.0 && at <= s.text_at)
        { old } else { activity };
        // An old local record keeps its real age when first discovered after app launch.
        let first_idle_time = if new_session && activity.group() == Group::Idle {
            b.get("_text_at").and_then(Value::as_f64).filter(|at| *at > 0.0 && *at <= t)
        } else { None };
        if let Some(v) = str_at(b, &["_app"]) {
            s.app_bundle = v.to_string();
        }
        if let Some(v) = str_at(b, &["_tty"]) {
            let v = v.trim();
            s.tty = if v.starts_with("/dev/") { v.to_string() } else { format!("/dev/{v}") };
        }
        if let Some(v) = str_at(b, &["_pid"]).and_then(|v| v.trim().parse::<i32>().ok()) {
            s.pid = v;
        }
        if let Some(v) = str_at(b, &["_term"]) {
            s.term = v.to_string();
        }
        if let Some(title) = title {
            s.title = title;
        }
        if let Some(text) = text {
            if text != s.text && !s.text.is_empty() && t - s.text_at < TEXT_DWELL_S && b.get("_text_at").is_none() {
                // The current text hasn't had its 1.5 s yet: show the new one after it.
                s.pending_text = Some(text);
            } else if text != s.text {
                s.pending_text = None;
                s.text = text;
                // A message the agent's records date (Codex) keeps its own time.
                s.text_at = b.get("_text_at").and_then(Value::as_f64).filter(|v| *v > 0.0).unwrap_or(t);
            }
        }
        if b.get("_clear_text").and_then(Value::as_bool) == Some(true) {
            // A new live step without words yet: drop the previous step's text (after its dwell).
            if !s.text.is_empty() && t - s.text_at < TEXT_DWELL_S {
                s.pending_text = Some(String::new());
            } else {
                s.text.clear();
                s.text_at = t;
            }
        }
        if let Some(n) = b.get("_unread").and_then(Value::as_u64) {
            s.unread = n.min(999) as u32;
        }
        if activity == Activity::Tool {
            s.tool = Some(ToolKind::from_tool_name(tool_name));
            if s.tool == Some(ToolKind::Delegate) {
                s.subagents += 1;
                s.subagent_at = t;
            }
        }
        if event.as_deref() == Some("SubagentStop") {
            s.subagents = s.subagents.saturating_sub(1);
            s.subagent_at = t;
        }
        if old != activity {
            s.since = first_idle_time.unwrap_or(t);
        }
        let og = old.group();
        let ng = activity.group();
        if (og != Group::Working && ng == Group::Working) || event.as_deref() == Some("UserPromptSubmit") {
            s.turn_started = t;
            s.archived = false;
        }
        s.activity = activity;
        s.updated = t;
        let turn_change = og != ng && (ng == Group::Waiting || og == Group::Working || ng == Group::Working || ng == Group::Done);
        if self.settings.reorder_on == ReorderOn::All || turn_change || !self.order.contains(&self.sessions[idx].key()) {
            self.resort();
        }
        self.touch();
    }

    fn resort(&mut self) {
        let mut v: Vec<&Session> = self.sessions.iter().collect();
        v.sort_by(|a, b| {
            a.activity
                .rank()
                .cmp(&b.activity.rank())
                .then(b.turn_started.partial_cmp(&a.turn_started).unwrap_or(std::cmp::Ordering::Equal))
        });
        self.order = v.iter().map(|s| s.key()).collect();
    }

    /// Time-based changes from SessionRecord.resolve: writing falls back to thinking after the
    /// streaming window, done and starting fade to idle, silent sessions lose their signal,
    /// old ones are removed; sub-agents are forgotten.
    pub fn tick(&mut self) {
        let t = now();
        let mut changed = false;
        for s in &mut self.sessions {
            if s.pending_text.is_some() && t - s.text_at >= TEXT_DWELL_S {
                s.text = s.pending_text.take().unwrap_or_default();
                s.text_at = t;
                changed = true;
            }
            let quiet = t - s.updated;
            let next = match s.activity {
                // The sample sessions don't stream real text, so they keep their state.
                Activity::Streaming if quiet > STREAMING_WINDOW_S && !s.id.starts_with("demo-") => Some(Activity::Thinking),
                Activity::Done if t - s.since > DONE_DECAY_S => Some(Activity::Idle),
                Activity::Starting if t - s.since > STARTING_WINDOW_S => Some(Activity::Idle),
                a if matches!(a.group(), Group::Working | Group::Waiting) && quiet > STALE_BUSY_AFTER_S => Some(Activity::Offline),
                a if matches!(a.group(), Group::Idle | Group::Done | Group::Error) && quiet > STALE_IDLE_AFTER_S => Some(Activity::Offline),
                _ => None,
            };
            if let Some(a) = next {
                s.activity = a;
                s.since = t;
                changed = true;
            }
            let sub_done = s.activity.group() != Group::Working && t - s.since > SUBAGENT_DONE_DECAY_S;
            if s.subagents > 0 && (sub_done || t - s.subagent_at > SUBAGENT_FORGET_AFTER_S) {
                s.subagents = 0;
                changed = true;
            }
        }
        let before = self.sessions.len();
        self.sessions.retain(|s| s.archived || t - s.updated < REMOVE_AFTER_S);
        if self.sessions.len() != before {
            changed = true;
        }
        if changed {
            self.resort();
            self.touch();
        }
        for i in 0..KEY_COUNT {
            if let (Some(d), Some(KeyPressTarget::Session(key))) = (self.key_down[i], self.key_target[i].clone()) {
                if !self.hold_fired[i] && t - d >= ARCHIVE_HOLD_S {
                    self.hold_fired[i] = true;
                    if let Some(s) = self.sessions.iter_mut().find(|s| s.key() == key) {
                        s.archived = true;
                        self.touch();
                        self.notify("Moved to the end until the conversation resumes work.");
                    }
                }
            }
        }
        // Hold both page buttons for 1.5 s → hand the keypad back to Options+.
        if let (Some(a), Some(b)) = (self.page_down[0], self.page_down[1]) {
            if t - a.max(b) >= HAND_BACK_HOLD_S && self.in_control {
                self.page_down = [None; 2];
                self.release();
                self.notify("Keypad handed back to Options+ (both page buttons held).");
            }
        }
        if let Some((_, at)) = &self.notice {
            if t - at > 6.0 {
                self.notice = None;
            }
        }
    }

    pub fn visible(&self) -> Vec<&Session> {
        let t = now();
        let recency = self.settings.recency_minutes as f64 * 60.0;
        // Working and waiting sessions always keep a key; others only while recent.
        let recent = |s: &&Session| {
            !s.archived && (matches!(s.activity.group(), Group::Working | Group::Waiting) || t - s.updated < recency)
        };
        let mut v: Vec<&Session> = self
            .order
            .iter()
            .filter_map(|k| self.sessions.iter().find(|s| &s.key() == k))
            .filter(|s| self.agent_enabled(&s.agent))
            .filter(recent)
            .collect();
        for s in &self.sessions {
            if !self.order.contains(&s.key()) && self.agent_enabled(&s.agent) && recent(&s) {
                v.push(s);
            }
        }
        v
    }

    /// All keypad slots, including empty slots reserved on the first page. Keep this as the
    /// single mapping for drawing, key presses and the hub's waiting-session shortcut.
    fn keypad_slots(&self) -> Vec<Option<&Session>> {
        self.keypad_slots_at(now())
    }

    fn keypad_slots_at(&self, t: f64) -> Vec<Option<&Session>> {
        let (recent, old_idle): (Vec<_>, Vec<_>) = self.visible().into_iter().partition(|s| {
            s.activity.group() != Group::Idle || t - s.since <= IDLE_SECOND_PAGE_AFTER_S
        });
        let mut slots: Vec<Option<&Session>> = recent.into_iter().map(Some).collect();
        // Archived sessions remain reachable after all normal sessions, including those
        // outside the recency window. Never fill the first page's reserved holes with them.
        let mut archived: Vec<&Session> = self.order.iter()
            .filter_map(|key| self.sessions.iter().find(|s| &s.key() == key))
            .filter(|s| s.archived && self.agent_enabled(&s.agent))
            .collect();
        archived.extend(self.sessions.iter().filter(|s| s.archived && self.agent_enabled(&s.agent) && !self.order.contains(&s.key())));
        if (!old_idle.is_empty() || !archived.is_empty()) && slots.len() < SESSION_KEYS_PER_PAGE {
            slots.resize(SESSION_KEYS_PER_PAGE, None);
        }
        slots.extend(old_idle.into_iter().map(Some));
        slots.extend(archived.into_iter().map(Some));
        slots
    }

    pub fn pages(&self) -> usize {
        self.keypad_slots().len().div_ceil(SESSION_KEYS_PER_PAGE).max(1)
    }

    pub fn waiting_count(&self) -> usize {
        self.visible().iter().filter(|s| s.activity.group() == Group::Waiting).count()
    }

    /// After this long with every session idle, the keypad dims and redraws at 10 fps.
    pub fn dimmed(&self) -> bool {
        let quiet = self.visible().iter().all(|s| !matches!(s.activity.group(), Group::Working | Group::Waiting));
        quiet && now() - self.last_change > self.settings.idle_dim_minutes as f64 * 60.0
    }

    /// FrameLoop.reschedule (0x10015d4dc): min(30, max(targetFPS, 1)), and at most 10 when
    /// the keypad is quiet (dimmed here); the timer repeats at 1/fps.
    pub fn fps(&self) -> u32 {
        let fps = self.settings.target_fps.max(1).min(30);
        if self.dimmed() { fps.min(10) } else { fps }
    }

    // ---- colours

    pub fn waiting_colour(&self) -> Rgb {
        let orb = colour::hex_u32(colour::reference_ramp("waitingPermission")[2]);
        colour::rotate(orb, self.settings.waiting_hue - colour::REFERENCE_WAITING_HUE)
    }

    pub fn hue_of(&self, agent: &str) -> f32 {
        self.settings
            .agent_hues
            .get(agent)
            .copied()
            .or_else(|| self.agent(agent).map(|a| a.hue))
            .unwrap_or(250.0)
    }

    /// Orb and glow colours from the original's reference ramps: calm states turn to the
    /// agent's hue; waiting keeps the shared terracotta (turned by the Waiting setting);
    /// error stays red; offline stays grey.
    pub fn colours(&self, s: &Session) -> (Rgb, Rgb) {
        let g = s.activity.group();
        let key = match s.activity {
            Activity::Empty => "idle",
            Activity::Starting => "starting",
            Activity::Idle => "idle",
            Activity::Thinking => "thinking",
            Activity::Tool => "tool",
            Activity::Streaming => "streaming",
            Activity::WaitingPermission => "waitingPermission",
            Activity::WaitingQuestion => "waitingQuestion",
            Activity::Done => "done",
            Activity::Error => "error",
            Activity::Compacting => "compacting",
            Activity::Offline => "offline",
        };
        let ramp = colour::reference_ramp(key);
        let (orb, glow) = (colour::hex_u32(ramp[2]), colour::hex_u32(ramp[3]));
        match g {
            Group::Waiting => {
                let d = self.settings.waiting_hue - colour::REFERENCE_WAITING_HUE;
                return (colour::rotate(orb, d), colour::rotate(glow, d));
            }
            Group::Error | Group::Offline => return (orb, glow),
            _ => {}
        }
        let state_key = match g {
            Group::Working => "working",
            Group::Done => "done",
            _ => "idle",
        };
        let own = self.agent(&s.agent).and_then(|a| a.state_colours.get(state_key)).and_then(|h| Rgb::parse(h));
        if let Some(c) = own {
            return (c, c);
        }
        if self.settings.colour_keys_by == ColourKeysBy::State {
            let c = self.settings.state_colours.get(state_key).and_then(|h| Rgb::parse(h)).unwrap_or(orb);
            return (c, c);
        }
        let d = self.hue_of(&s.agent) - colour::REFERENCE_HUE;
        (colour::rotate(orb, d), colour::rotate(glow, d))
    }

    /// Each row is one provider and up to three quota windows. Overflow has more pages.
    pub fn quota_rows(&self) -> Vec<Vec<crate::quota::Quota>> {
        let mut providers = vec!["Codex".to_string(), "Claude".to_string()];
        for q in &self.quotas {
            if !providers.iter().any(|p| p.eq_ignore_ascii_case(&q.provider)) { providers.push(q.provider.clone()); }
        }
        providers[2..].sort();
        let mut rows = Vec::new();
        let mut overflow = Vec::new();
        for provider in providers {
            let mut values: Vec<crate::quota::Quota> = self.quotas.iter()
                .filter(|q| q.provider.eq_ignore_ascii_case(&provider)).cloned().collect();
            values.sort_by(|a, b| {
                let rank = |window: &str| match window { "Session" => 0, "Weekly" => 1, _ => 2 };
                rank(&a.window).cmp(&rank(&b.window)).then_with(|| a.window.cmp(&b.window))
                    .then_with(|| b.updated.cmp(&a.updated))
            });
            values.dedup_by(|a, b| a.window == b.window);
            let mut chunks = values.chunks(3);
            if let Some(first) = chunks.next() { rows.push(first.to_vec()); }
            overflow.extend(chunks.map(|chunk| chunk.to_vec()));
        }
        rows.extend(overflow);
        rows
    }

    pub fn quota_pages(&self) -> usize { self.quota_rows().len().div_ceil(2).max(1) }

    // ---- keypad faces

    pub fn faces(&self) -> [KeyFace; KEY_COUNT] {
        let mut out: [KeyFace; KEY_COUNT] = std::array::from_fn(|_| KeyFace::Empty);
        if !self.in_control {
            for f in out.iter_mut() {
                *f = KeyFace::Released;
            }
            return out;
        }
        if self.quota_view {
            for (row, entries) in self.quota_rows().into_iter().skip(self.quota_page * 2).take(2).enumerate() {
                for (col, mut quota) in entries.into_iter().enumerate() {
                    quota.remaining = crate::quota::displayed_remaining(&quota, now() as i64);
                    out[row * 3 + col] = KeyFace::Quota(quota);
                }
            }
            out[HUB_KEY] = KeyFace::QuotaHub;
            return out;
        }
        let t = now();
        let speed = self.settings.text_speed.max(1.0) as f64;
        let face_of = |s: &Session| {
            let (orb, glow) = self.colours(s);
            let shown = ((t - s.text_at) * speed).max(0.0) as usize;
            // Only a recent last message is shown; otherwise the unread count, otherwise nothing.
            let text: String = if t - s.text_at <= RECENT_TEXT_S { s.text.chars().take(shown).collect() } else { String::new() };
            let def = self.agent(&s.agent);
            SessionFace {
                key: s.key(),
                badge: def.map(|d| d.badge.clone()).unwrap_or_else(|| badge_from_name(&s.agent)),
                agent_name: def.map(|d| d.name.clone()).unwrap_or_else(|| s.agent.clone()),
                title: s.title.clone(),
                text,
                activity: s.activity,
                tool: s.tool,
                orb,
                glow,
                since: s.since,
                subagents: s.subagents,
                dots: def.map(|d| d.style == "dots").unwrap_or(false),
                unread: s.unread,
                last_message: s.text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("").to_string(),
                last_turn: s.updated,
                agent_colour: colour::rotate(colour::hex_u32(0xd9a441), self.hue_of(&s.agent) - colour::REFERENCE_HUE),
            }
        };
        let vis = self.visible();
        let page = self.page.min(self.pages() - 1);
        for (i, slot) in self.keypad_slots().iter().skip(page * SESSION_KEYS_PER_PAGE).take(SESSION_KEYS_PER_PAGE).enumerate() {
            if let Some(s) = slot { out[i] = KeyFace::Session(face_of(s)); }
        }
        let count = |g: Group| vis.iter().filter(|s| s.activity.group() == g).count();
        out[HUB_KEY] = KeyFace::Hub(HubFace {
            waiting: count(Group::Waiting),
            working: count(Group::Working),
            done: count(Group::Done),
            page,
            pages: self.pages(),
            dots: vis.iter().take(12).map(|s| self.colours(s).0).collect(),
            waiting_colour: self.waiting_colour(),
            dimmed: self.dimmed(),
            agents: {
                let mut list: Vec<(String, Rgb, usize)> = Vec::new();
                for s in &vis {
                    let name = self.agent(&s.agent).map(|a| a.name.clone()).unwrap_or_else(|| s.agent.clone());
                    match list.iter_mut().find(|(n, _, _)| *n == name) {
                        Some(e) => e.2 += 1,
                        None => {
                            let hue = self.hue_of(&s.agent);
                            list.push((name, colour::rotate(colour::hex_u32(0xd9a441), hue - colour::REFERENCE_HUE), 1));
                        }
                    }
                }
                list
            },
        });
        out
    }

    // ---- input (on-screen keypad, keyboard, physical keypad)

    pub fn key_down(&mut self, i: usize) {
        if i >= KEY_COUNT { return; }
        let faces = self.faces();
        let target = target_for_face(&faces[i], self.quota_view);
        self.key_down_with_target(i, target);
    }

    pub fn key_down_with_target(&mut self, i: usize, target: KeyPressTarget) {
        if i < KEY_COUNT && self.key_down[i].is_none() && target != KeyPressTarget::Disabled {
            self.key_down[i] = Some(now());
            self.key_target[i] = Some(target);
            self.hold_fired[i] = false;
        }
    }

    /// Discard a pointer press when its window disappears, without invoking its action.
    pub fn cancel_key(&mut self, i: usize) {
        if i < KEY_COUNT {
            self.key_down[i] = None;
            self.key_target[i] = None;
            self.hold_fired[i] = false;
        }
    }

    pub fn key_up(&mut self, i: usize) {
        if i >= KEY_COUNT { return; }
        let down = self.key_down[i].take();
        let target = self.key_target[i].take();
        if let (Some(d), Some(KeyPressTarget::Session(key))) = (down, target.as_ref()) {
            if !self.hold_fired[i] && self.in_control && now() - d >= ARCHIVE_HOLD_S {
                self.hold_fired[i] = true;
                if let Some(s) = self.sessions.iter_mut().find(|s| s.key() == *key) {
                    s.archived = true;
                    self.touch();
                    self.notify("Moved to the end until the conversation resumes work.");
                }
            }
        }
        if down.is_none() || self.hold_fired[i] || !self.in_control { return; }
        match target {
            Some(KeyPressTarget::Hub) => {
                let slots = self.keypad_slots();
                if let Some(pos) = slots.iter().position(|s| s.is_some_and(|s| !s.archived && s.activity.group() == Group::Waiting)) {
                    self.page = pos / SESSION_KEYS_PER_PAGE;
                } else { self.actions.push(Action::ShowWindow); }
                self.touch();
            }
            Some(KeyPressTarget::Session(key)) if self.settings.key_actions_enabled => {
                if let Some(s) = self.sessions.iter().find(|s| s.key() == key) {
                    self.actions.push(Action::Open { agent: s.agent.clone(), session: s.id.clone() });
                }
            }
            Some(KeyPressTarget::Empty) if self.settings.key_actions_enabled && !self.settings.empty_key_url.trim().is_empty() => {
                self.actions.push(Action::OpenUrl(self.settings.empty_key_url.clone()));
            }
            _ => {}
        }
    }

    pub fn hold_progress(&self, i: usize) -> f32 {
        if self.quota_view { return 0.0; }
        match self.key_down.get(i).copied().flatten() {
            Some(d) if !self.hold_fired[i] => ((now() - d) / ARCHIVE_HOLD_S).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        }
    }

    /// `next = false` is the previous-page button (control 0x01A1), `true` the next (0x01A2).
    pub fn page_button(&mut self, next: bool, down: bool) {
        let i = next as usize;
        if down {
            if self.page_down[i].is_none() {
                self.page_down[i] = Some(now());
                if self.in_control {
                    self.step(next);
                }
            }
        } else {
            self.page_down[i] = None;
        }
    }

    pub fn page_held(&self, next: bool) -> bool {
        self.page_down[next as usize].is_some()
    }

    pub fn hand_back_progress(&self) -> f32 {
        match (self.page_down[0], self.page_down[1]) {
            (Some(a), Some(b)) => ((now() - a.max(b)) / HAND_BACK_HOLD_S).clamp(0.0, 1.0) as f32,
            _ => 0.0,
        }
    }

    fn step(&mut self, next: bool) {
            let pages = self.pages();
            // A press started on another page must not act on the new page.
            self.key_down = [None; KEY_COUNT];
            self.key_target = std::array::from_fn(|_| None);
            self.hold_fired = [false; KEY_COUNT];
            if self.quota_view {
                if next && self.quota_page + 1 < self.quota_pages() { self.quota_page += 1; }
                else if !next && self.quota_page > 0 { self.quota_page -= 1; }
                else { self.quota_view = false; self.page = if next { 0 } else { pages - 1 }; }
            } else if !next && self.page == 0 {
                self.quota_view = true;
                self.quota_page = self.quota_pages() - 1;
            } else if next && self.page + 1 >= pages {
                self.quota_view = true;
                self.quota_page = 0;
            } else if next { self.page += 1; } else { self.page -= 1; }
        self.touch();
    }

    pub fn take_over(&mut self) {
        if !self.in_control {
            self.in_control = true;
            self.notify("Padgent has the keypad. Each key is one session in its agent's colour; the hub key counts what is waiting.");
        }
    }

    pub fn release(&mut self) {
        if self.in_control {
            self.in_control = false;
            self.notify("The keypad is back with Logi Options+. The menu bar keeps working.");
        }
    }

    pub fn control_label(&self) -> &'static str {
        match (&self.hardware, self.in_control) {
            (Hardware::Driving, true) => "Padgent has the keypad",
            (Hardware::Problem(_), true) => "Keypad needs attention",
            (_, false) => "Keypad with Logi Options+",
            (Hardware::Off, true) | (Hardware::NoKeypad, true) => "In control · on screen",
            (Hardware::Connected, true) => "Taking over the keypad…",
        }
    }

    // ---- demo scene (also used for --screenshot)

    pub fn load_demo(&mut self) {
        let t = now();
        let demo = [
            ("claude-code", "api-server", Activity::Thinking, None, "Tracing the 502s back to the pool timeout in db/pool.rs"),
            ("codex", "billing-ui", Activity::Tool, Some(ToolKind::Shell), "npm test -- --watch=false"),
            ("claude-code", "orb-docs", Activity::WaitingPermission, None, "Allow edit to README.md?"),
            ("antigravity", "landing-page", Activity::Streaming, None, "Rebuilding the hero with the new glass cards and a softer gradient"),
            ("zcode", "data-pipeline", Activity::Done, None, "All 214 checks passed."),
            ("kimi-code", "infra", Activity::Compacting, None, "Compacting the conversation"),
            ("codex", "auth-service", Activity::WaitingQuestion, None, "Use OAuth device flow or PKCE?"),
            ("claude-code", "mobile-app", Activity::Idle, None, "Shipped the onboarding fix."),
            ("claude-code", "scripts", Activity::Error, None, "Build failed: OPENAI_KEY is not set"),
        ];
        self.sessions.clear();
        for (n, (agent, title, act, tool, text)) in demo.iter().enumerate() {
            self.sessions.push(Session {
                agent: agent.to_string(),
                id: format!("demo-{n}"),
                title: title.to_string(),
                activity: *act,
                tool: *tool,
                text: text.to_string(),
                text_at: t - 30.0 + n as f64 * 0.0,
                since: t - 2.0 - n as f64 * 0.37,
                updated: t,
                turn_started: t - n as f64,
                subagents: if n == 0 { 2 } else { 0 },
                subagent_at: t,
                app_bundle: String::new(),
                tty: String::new(),
                term: String::new(),
                pid: 0,
                unread: if *title == "data-pipeline" { 2 } else { 0 },
                archived: false,
                pending_text: None,
            });
        }
        self.resort();
        self.touch();
    }

    /// Keeps the demo alive: retypes the live text every few seconds.
    pub fn demo_tick(&mut self) {
        let t = now();
        for s in &mut self.sessions {
            if s.id.starts_with("demo-") {
                if s.activity == Activity::Done || s.activity == Activity::Idle {
                    s.since = s.since.max(t - 20.0);
                }
                if matches!(s.activity, Activity::Streaming | Activity::Thinking) && t - s.text_at > 9.0 {
                    s.text_at = t;
                }
                s.updated = t;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn add_test_session(core: &mut Core, id: &str, state: &str) {
        core.apply(Incoming {
            agent: "codex".into(),
            event: None,
            body: serde_json::json!({"session": id, "state": state, "title": id}),
        });
    }

    fn test_quota(provider: &str, window: &str, remaining: f64) -> crate::quota::Quota {
        crate::quota::Quota {
            provider: provider.into(), window: window.into(), remaining: Some(remaining), reset: None,
            reset_text: Some("Reset unknown".into()), updated: Some("2026-11-11T15:30:00Z".into()), source: "test".into(),
        }
    }

    #[test]
    fn agentpad_agent_export_import_round_trip_preserves_definition() {
        let mut agent = agent_for_app("Example Editor", "com.example.editor");
        agent.description = "An imported editor integration".into();
        agent.open_url = "example://session/{session}".into();
        agent.style = "dots".into();
        agent.events.insert("PromptStart".into(), "thinking".into());
        agent.models.insert("code".into(), "example-model".into());

        let exported = serde_json::to_string_pretty(&agent).unwrap();
        let imported = parse_agent_file(&exported).unwrap();
        assert_eq!(imported, vec![agent]);
        assert_eq!(imported[0].format, "agentpad.agent");
    }

    #[test]
    fn agentpad_bundle_export_import_round_trip_preserves_agents() {
        let mut first = agent_for_app("First Editor", "com.example.first");
        first.events.insert("PromptStart".into(), "thinking".into());
        let second = agent_for_app("Second Editor", "com.example.second");
        let bundle = serde_json::json!({
            "format": AGENTS_FORMAT,
            "version": 1,
            "agents": [first, second],
        });

        let imported = parse_agent_file(&bundle.to_string()).unwrap();
        assert_eq!(imported.len(), 2);
        assert_eq!(imported[0].id, "com.example.first");
        assert_eq!(imported[0].events.get("PromptStart").map(String::as_str), Some("thinking"));
        assert_eq!(imported[1].id, "com.example.second");
        assert!(imported.iter().all(|agent| agent.format == AGENT_FORMAT));
    }

    #[test]
    fn stored_agent_objects_arrays_and_bundles_normalize_their_markers() {
        let agent = agent_for_app("Saved Editor", "com.example.saved");
        let mut object = serde_json::to_value(&agent).unwrap();
        object["format"] = serde_json::json!("previous.vendor.agent");

        let from_object = parse_stored_agents(&object.to_string()).unwrap();
        assert_eq!(from_object[0].format, AGENT_FORMAT);

        let from_array = parse_stored_agents(&serde_json::json!([object.clone()]).to_string()).unwrap();
        assert_eq!(from_array[0].format, AGENT_FORMAT);

        let bundle = serde_json::json!({
            "format": "previous.vendor.agents",
            "version": 1,
            "agents": [object],
        });
        let from_bundle = parse_stored_agents(&bundle.to_string()).unwrap();
        assert_eq!(from_bundle[0].format, AGENT_FORMAT);
        assert_eq!(from_bundle[0].id, agent.id);
    }

    #[test]
    fn external_import_rejects_a_non_agentpad_marker() {
        let mut object = serde_json::to_value(agent_for_app("Saved Editor", "com.example.saved")).unwrap();
        object["format"] = serde_json::json!("previous.vendor.agent");
        assert!(parse_agent_file(&object.to_string()).is_err());
    }

    #[test]
    fn settings_round_trip_uses_agentpad_marker_and_preserves_values() {
        let mut settings = Settings::default();
        settings.brightness = 0.42;
        settings.menu_indicator = MenuIndicator::Dot;
        settings.disabled_agents.push("com.example.editor".into());

        let exported = serde_json::to_string(&settings).unwrap();
        let value: Value = serde_json::from_str(&exported).unwrap();
        assert_eq!(value["format"], "agentpad.settings");
        let imported: Settings = serde_json::from_str(&exported).unwrap();
        assert_eq!(imported.format, "agentpad.settings");
        assert_eq!(imported.brightness, 0.42);
        assert_eq!(imported.menu_indicator, MenuIndicator::Dot);
        assert_eq!(imported.disabled_agents, vec!["com.example.editor"]);
    }

    #[test]
    fn cancelling_pointer_press_clears_hold_and_suppresses_click_action() {
        let mut settings = Settings::default();
        settings.empty_key_url = "https://example.com/empty".into();
        let mut core = Core::new(settings);
        core.in_control = true;

        core.key_down(0);
        core.cancel_key(0);
        assert_eq!(core.hold_progress(0), 0.0);
        core.key_up(0);
        assert!(core.actions.is_empty());

        // A later normal press still dispatches through the same Core input path.
        core.key_down(0);
        core.key_up(0);
        assert!(matches!(core.actions.as_slice(), [Action::OpenUrl(url)] if url == "https://example.com/empty"));
    }

    #[test]
    fn press_opens_the_session_that_was_on_the_key_at_keydown() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "first", "thinking");
        add_test_session(&mut core, "second", "idle");

        let shown = core.faces();
        let target = target_for_face(&shown[0], core.quota_view);
        assert_eq!(target, KeyPressTarget::Session("codex:first".into()));
        core.key_down_with_target(0, target);

        // Reordering moves the waiting session into the first position before key-up.
        let first = core.sessions.iter_mut().find(|s| s.id == "first").unwrap();
        first.activity = Activity::Idle;
        let second = core.sessions.iter_mut().find(|s| s.id == "second").unwrap();
        second.activity = Activity::WaitingQuestion;
        core.resort();
        assert!(matches!(&core.faces()[0], KeyFace::Session(face) if face.key == "codex:second"));

        core.key_up(0);
        assert!(matches!(core.actions.as_slice(), [Action::Open { agent, session }] if agent == "codex" && session == "first"));
    }

    #[test]
    fn archived_idle_session_stays_hidden_for_read_receipts_and_title_updates() {
        let mut core = Core::new(Settings::default());
        add_test_session(&mut core, "archived", "idle");
        core.sessions[0].archived = true;

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"idle", "title":"Renamed", "_unread":3}),
        });
        assert!(core.sessions[0].archived);
        assert_eq!(core.sessions[0].title, "Renamed");
        assert_eq!(core.sessions[0].unread, 3);
        assert!(core.visible().is_empty());

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"thinking"}),
        });
        assert!(!core.sessions[0].archived);
        assert_eq!(core.visible()[0].id, "archived");
    }

    #[test]
    fn archived_work_stays_hidden_until_new_turn_or_user_prompt_event() {
        let mut core = Core::new(Settings::default());
        add_test_session(&mut core, "archived", "thinking");
        core.sessions[0].archived = true;

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"streaming", "title":"Still running"}),
        });
        assert!(core.sessions[0].archived);
        assert!(core.visible().is_empty());

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"idle"}),
        });
        assert!(core.sessions[0].archived);
        assert!(core.visible().is_empty());

        core.apply(Incoming {
            agent: "codex".into(), event: Some("UserPromptSubmit".into()),
            body: serde_json::json!({"session":"archived"}),
        });
        assert!(!core.sessions[0].archived);
        assert_eq!(core.visible()[0].id, "archived");
    }

    #[test]
    fn archived_sessions_follow_old_idle_tiles_and_return_to_normal_order_on_resume() {
        let mut settings = Settings::default();
        settings.recency_minutes = 240;
        let mut core = Core::new(settings);
        core.in_control = true;
        add_test_session(&mut core, "active", "thinking");
        add_test_session(&mut core, "old-idle", "idle");
        add_test_session(&mut core, "archived-waiting", "waiting_question");
        core.sessions.iter_mut().find(|s| s.id == "old-idle").unwrap().since = now() - IDLE_SECOND_PAGE_AFTER_S - 60.0;
        core.sessions.iter_mut().find(|s| s.id == "archived-waiting").unwrap().archived = true;
        core.resort();

        let slots = core.keypad_slots_at(now());
        assert_eq!(slots[0].unwrap().id, "active");
        assert!(slots[1..SESSION_KEYS_PER_PAGE].iter().all(Option::is_none), "archives must not consume reserved page-one holes");
        assert_eq!(slots[SESSION_KEYS_PER_PAGE].unwrap().id, "old-idle");
        assert_eq!(slots[SESSION_KEYS_PER_PAGE + 1].unwrap().id, "archived-waiting");
        assert_eq!(core.waiting_count(), 0, "archived waiting sessions are absent from normal counts");

        core.page = 1;
        assert!(matches!(&core.faces()[1], KeyFace::Session(face) if face.key == "codex:archived-waiting"));
        core.key_down(1);
        core.key_up(1);
        assert!(matches!(core.actions.as_slice(), [Action::Open { agent, session }] if agent == "codex" && session == "archived-waiting"));

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived-waiting", "state":"thinking"}),
        });
        assert!(!core.sessions.iter().find(|s| s.id == "archived-waiting").unwrap().archived);
        assert_eq!(core.visible()[0].id, "archived-waiting", "resumed work returns to the ordinary recent ordering");
        assert_eq!(core.keypad_slots_at(now())[0].unwrap().id, "archived-waiting");
    }

    #[test]
    fn archived_waiting_session_does_not_drive_hub_navigation() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "active", "thinking");
        add_test_session(&mut core, "archived-waiting", "waiting_question");
        core.sessions.iter_mut().find(|s| s.id == "archived-waiting").unwrap().archived = true;
        core.resort();

        assert_eq!(core.waiting_count(), 0);
        assert!(matches!(&core.faces()[HUB_KEY], KeyFace::Hub(hub) if hub.waiting == 0));
        core.key_down(HUB_KEY);
        core.key_up(HUB_KEY);
        assert_eq!(core.page, 0, "the archived waiting tile must not redirect to its later page");
        assert!(matches!(core.actions.as_slice(), [Action::ShowWindow]));
    }

    #[test]
    fn delayed_release_archives_captured_session_without_opening_it() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "first", "thinking");
        add_test_session(&mut core, "second", "idle");
        let shown = core.faces();
        let target = target_for_face(&shown[0], core.quota_view);
        assert_eq!(target, KeyPressTarget::Session("codex:first".into()));
        core.key_down_with_target(0, target);

        // A later turn reorders the other session into this physical key's current slot.
        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"second", "state":"waiting_question"}),
        });
        assert!(matches!(&core.faces()[0], KeyFace::Session(face) if face.key == "codex:second"));

        core.key_down[0] = Some(now() - ARCHIVE_HOLD_S - 0.1);
        core.tick();
        assert!(core.sessions.iter().find(|s| s.id == "first").unwrap().archived);
        assert!(!core.sessions.iter().find(|s| s.id == "second").unwrap().archived);
        core.key_up(0);
        assert!(core.actions.is_empty());
    }

    #[test]
    fn old_idle_sessions_leave_reserved_page_one_slots_and_remain_on_page_two() {
        let mut settings = Settings::default();
        settings.recency_minutes = 240;
        settings.reorder_on = ReorderOn::Turns;
        let mut core = Core::new(settings);
        core.in_control = true;
        add_test_session(&mut core, "old-idle", "idle");
        add_test_session(&mut core, "recent-idle", "idle");
        add_test_session(&mut core, "active", "thinking");

        let t = now();
        for s in &mut core.sessions {
            match s.id.as_str() {
                "old-idle" => { s.since = t - IDLE_SECOND_PAGE_AFTER_S - 60.0; s.updated = t - 60.0; s.turn_started = s.since; }
                "recent-idle" => { s.since = t - 5.0 * 60.0; s.updated = s.since; s.turn_started = s.since; }
                "active" => { s.since = t - 90.0 * 60.0; s.updated = s.since; s.turn_started = s.since; }
                _ => unreachable!(),
            }
        }
        core.resort();

        let slots = core.keypad_slots_at(t);
        assert_eq!(slots.len(), SESSION_KEYS_PER_PAGE + 1);
        assert_eq!(slots[0].unwrap().id, "active");
        assert_eq!(slots[1].unwrap().id, "recent-idle");
        assert!(slots[2..SESSION_KEYS_PER_PAGE].iter().all(Option::is_none));
        assert_eq!(slots[SESSION_KEYS_PER_PAGE].unwrap().id, "old-idle");
        assert_eq!(core.pages(), 2);

        core.page = 0;
        assert!(matches!(&core.faces()[0], KeyFace::Session(s) if s.key == "codex:active"));
        assert!(matches!(&core.faces()[2], KeyFace::Empty));
        core.page = 1;
        assert!(matches!(&core.faces()[0], KeyFace::Session(s) if s.key == "codex:old-idle"));
        core.key_down(0);
        core.key_up(0);
        assert!(matches!(core.actions.as_slice(), [Action::Open { agent, session }] if agent == "codex" && session == "old-idle"));
    }

    #[test]
    fn idle_recency_boundary_is_inclusive_and_waiting_sessions_stay_on_page_one() {
        let mut settings = Settings::default();
        settings.recency_minutes = 240;
        let mut core = Core::new(settings);
        core.in_control = true;
        add_test_session(&mut core, "exact", "idle");
        add_test_session(&mut core, "over", "idle");
        add_test_session(&mut core, "waiting", "waiting_question");

        let t = now();
        for s in &mut core.sessions {
            match s.id.as_str() {
                "exact" => { s.since = t - IDLE_SECOND_PAGE_AFTER_S; s.updated = t - 10.0; s.turn_started = s.since; }
                "over" => { s.since = t - IDLE_SECOND_PAGE_AFTER_S - 0.01; s.updated = t - 10.0; s.turn_started = s.since; }
                "waiting" => { s.since = t - 2.0 * 60.0 * 60.0; s.updated = s.since; s.turn_started = s.since; }
                _ => unreachable!(),
            }
        }
        core.resort();

        let slots = core.keypad_slots_at(t);
        assert_eq!(slots[0].unwrap().id, "waiting");
        assert_eq!(slots[1].unwrap().id, "exact");
        assert!(slots[2..SESSION_KEYS_PER_PAGE].iter().all(Option::is_none));
        assert_eq!(slots[SESSION_KEYS_PER_PAGE].unwrap().id, "over");
        assert_eq!(core.waiting_count(), 1);
        assert!(matches!(&core.faces()[HUB_KEY], KeyFace::Hub(hub) if hub.waiting == 1));
    }

    #[test]
    fn old_local_idle_age_survives_first_discovery_and_unread_refresh() {
        let mut settings = Settings::default();
        settings.recency_minutes = 240;
        let mut core = Core::new(settings);
        let message_at = now() - 60.0 * 60.0;
        core.apply(Incoming {
            agent: "codex".into(),
            event: None,
            body: serde_json::json!({
                "session": "local-old", "state": "idle", "title": "Local", "text": "last answer",
                "_text_at": message_at, "_unread": 1
            }),
        });
        assert_eq!(core.sessions[0].since, message_at);

        core.apply(Incoming {
            agent: "codex".into(),
            event: None,
            body: serde_json::json!({
                "session": "local-old", "state": "done", "title": "Local", "text": "last answer",
                "_text_at": message_at, "_unread": 2
            }),
        });
        assert_eq!(core.sessions[0].activity, Activity::Idle);
        assert_eq!(core.sessions[0].since, message_at);
        assert_eq!(core.sessions[0].unread, 2);

        let slots = core.keypad_slots_at(now());
        assert!(slots[..SESSION_KEYS_PER_PAGE].iter().all(Option::is_none));
        assert_eq!(slots[SESSION_KEYS_PER_PAGE].unwrap().id, "local-old");
    }

    #[test]
    fn old_idle_pagination_is_consistent_across_reorder_modes() {
        for mode in [ReorderOn::Turns, ReorderOn::All] {
            let mut settings = Settings::default();
            settings.recency_minutes = 240;
            settings.reorder_on = mode;
            let mut core = Core::new(settings);
            add_test_session(&mut core, "old", "idle");
            add_test_session(&mut core, "recent", "idle");
            let t = now();
            for s in &mut core.sessions {
                if s.id == "old" {
                    s.since = t - IDLE_SECOND_PAGE_AFTER_S - 5.0;
                    s.updated = t - 30.0;
                    s.turn_started = s.since;
                } else {
                    s.since = t - 30.0;
                    s.updated = s.since;
                    s.turn_started = s.since;
                }
            }
            core.resort();
            let slots = core.keypad_slots_at(t);
            assert_eq!(slots[0].unwrap().id, "recent", "mode: {mode:?}");
            assert_eq!(slots[SESSION_KEYS_PER_PAGE].unwrap().id, "old", "mode: {mode:?}");
        }
    }

    #[test]
    fn quota_page_does_not_invent_unobserved_provider_tiles() {
        let mut core = Core::new(Settings::default());
        core.quota_view = true;
        let faces = core.faces();
        assert!(core.quota_rows().is_empty());
        assert_eq!(core.quota_pages(), 1);
        assert!(faces[..HUB_KEY].iter().all(|face| matches!(face, KeyFace::Empty)));
        assert!(matches!(&faces[HUB_KEY], KeyFace::QuotaHub));
    }

    #[test]
    fn expired_quota_is_unknown_in_faces_but_retained_in_raw_observations() {
        let mut core = Core::new(Settings::default());
        core.quota_view = true;
        let mut old = test_quota("Codex", "Weekly", 42.0);
        old.updated = Some("2020-01-01T00:00:00Z".into());
        core.quotas.push(old.clone());

        let faces = core.faces();
        assert!(matches!(&faces[0], KeyFace::Quota(q)
            if q.window == "Weekly" && q.remaining.is_none() && crate::quota::detail(q) == "Reading expired"));
        assert_eq!(core.quotas[0], old);
        assert_eq!(core.quotas[0].remaining, Some(42.0));
    }

    #[test]
    fn quota_rows_keep_provider_window_identity_when_values_change() {
        let mut core = Core::new(Settings::default());
        core.quotas = vec![
            test_quota("Codex", "Weekly", 42.0),
            test_quota("Codex", "Session", 63.0),
            test_quota("Codex", "Weekly · Fable", 58.0),
            test_quota("Claude", "Weekly", 42.0),
            test_quota("Claude", "Session", 63.0),
        ];
        let identities = |rows: Vec<Vec<crate::quota::Quota>>| rows.into_iter().map(|row| {
            row.into_iter().map(|q| (q.provider, q.window)).collect::<Vec<_>>()
        }).collect::<Vec<_>>();
        let before = identities(core.quota_rows());
        assert_eq!(before[0], vec![
            ("Codex".into(), "Session".into()),
            ("Codex".into(), "Weekly".into()),
            ("Codex".into(), "Weekly · Fable".into()),
        ]);
        assert_eq!(before[1], vec![
            ("Claude".into(), "Session".into()),
            ("Claude".into(), "Weekly".into()),
        ]);
        for q in &mut core.quotas { q.remaining = q.remaining.map(|n| 100.0 - n); }
        assert_eq!(identities(core.quota_rows()), before);
    }

    #[test]
    fn quota_navigation_returns_to_the_expected_session_page() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        for i in 0..SESSION_KEYS_PER_PAGE + 1 {
            add_test_session(&mut core, &format!("page-{i}"), "thinking");
        }
        assert_eq!(core.pages(), 2);

        core.page = 1;
        core.page_button(true, true); // next from last page opens quotas
        assert!(core.quota_view);
        core.page_button(true, false);
        core.page_button(false, true); // previous from quotas returns to the page that opened it
        assert!(!core.quota_view);
        assert_eq!(core.page, 1);
        core.page_button(false, false);

        core.page_button(true, true); // next from last page opens quotas again
        assert!(core.quota_view);
        core.page_button(true, false);
        core.page_button(true, true); // next from quotas returns to page one
        assert!(!core.quota_view);
        assert_eq!(core.page, 0);
    }

    #[test]
    fn quota_keys_cannot_open_or_archive_session_tiles() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "session", "thinking");
        core.key_down(0);
        core.page_button(false, true); // switch to quotas while a tile is held
        core.page_button(false, false);
        assert!(core.quota_view);
        core.tick();
        core.key_down(0);
        core.tick();
        core.key_up(0);
        assert!(core.actions.is_empty());
    }

    #[test]
    fn new_text_waits_for_the_dwell() {
        let mut c = Core::new(Settings::default());
        c.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"a","state":"streaming","text":"first"}) });
        c.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"a","state":"streaming","text":"second"}) });
        assert_eq!(c.sessions[0].text, "first");
        assert_eq!(c.sessions[0].pending_text.as_deref(), Some("second"));
        c.sessions[0].text_at -= TEXT_DWELL_S;
        c.tick();
        assert_eq!(c.sessions[0].text, "second");
    }

    #[test]
    fn age_clock_compact() {
        assert_eq!(age_text(0.0, 59.0), "");
        assert_eq!(age_text(0.0, 5.0 * 60.0), "5m ago");
        assert_eq!(age_text(0.0, 61.0 * 60.0), "1h ago");
        assert_eq!(age_text(0.0, 1441.0 * 60.0), "1d ago");
    }

    #[test]
    fn archived_session_returns_when_work_resumes() {
        let mut c = Core::new(Settings::default());
        c.in_control = true;
        c.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"a","state":"idle","title":"A"}) });
        assert_eq!(c.visible().len(), 1);
        let key = c.visible()[0].key();
        c.sessions[0].archived = true;
        assert_eq!(c.visible().len(), 0);
        std::thread::sleep(std::time::Duration::from_millis(5));
        c.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"a","state":"thinking"}) });
        assert_eq!(c.visible()[0].key(), key);
    }
}
