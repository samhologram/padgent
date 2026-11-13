//! Agents, settings, sessions and the keypad core shared by the window, the menu bar,
//! the status receiver and the physical keypad driver. Numbers come from MECHANICS.md.

use crate::colour::{self, Rgb};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use crate::observation::{Observation, ObservationEvent, ObservationKind};
#[cfg(test)] use crate::observation::Source;
use crate::tracking::SourcePolicy;
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
/// Hold a session key to show its observed steps.
pub const DETAIL_HOLD_S: f64 = 0.6;
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

pub(crate) fn hook_events() -> BTreeMap<String, String> {
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
    let mut codex = mk("codex", "Codex", "CX", 270.0, "Terminal", "Local records and Cloud task status, shown as GPT-style status dots with the last message.");
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum IslandVisibility {
    Never,
    Always,
    WhenDisconnected,
    #[serde(alias = "quotasOnly")]
    QuotasAlways,
}

impl Default for IslandVisibility {
    fn default() -> Self { Self::Always }
}

fn deserialize_island_visibility<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<IslandVisibility, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum LegacyOrCurrent { Legacy(bool), Current(IslandVisibility) }
    Ok(match LegacyOrCurrent::deserialize(deserializer)? {
        LegacyOrCurrent::Legacy(true) => IslandVisibility::Always,
        LegacyOrCurrent::Legacy(false) => IslandVisibility::Never,
        LegacyOrCurrent::Current(mode) => mode,
    })
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
    pub show_hub_clock: bool,
    pub empty_key_url: String,
    pub agent_hues: BTreeMap<String, f32>,
    pub disabled_agents: Vec<String>,
    pub state_colours: BTreeMap<String, String>,
    pub waiting_hue: f32,
    pub menu_indicator: MenuIndicator,
    #[serde(alias = "islandEnabled", deserialize_with = "deserialize_island_visibility")]
    pub island_visibility: IslandVisibility,
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
            show_hub_clock: false,
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
            island_visibility: IslandVisibility::Always,
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
    pub open_url: Option<String>,
    /// Terminal device and program the agent runs in (from the hook's X-TTY / X-Term).
    pub tty: String,
    pub term: String,
    /// The agent's process (from the hook's X-PID), used to find the app it runs in.
    pub pid: i32,
    /// Replies not yet read in the agent's own app (Codex read receipts).
    pub unread: u32,
    /// Proven scheduled origin, retained across partial updates.
    pub scheduled: bool,
    /// Opened while inactive; remains visible at reduced intensity until work resumes.
    pub acknowledged: bool,
    /// Time of the user action, independent of provider update timestamps.
    pub acknowledged_at: Option<f64>,
    /// A completion or unread reply remains visible until this conversation is opened.
    pub pending_attention: bool,
    /// Text waiting for the current one's 1.5 s dwell to pass.
    pub pending_text: Option<String>,
    pub steps: Vec<SessionStep>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SessionStep { pub kind: String, pub status: String, pub title: String, pub text: String, pub signature: String }

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
    Controlled(crate::display::Scene, u64, bool),
    Quota(crate::quota::Quota),
    QuotaHub,
    Step(SessionStep),
    Back,
}

pub const ACKNOWLEDGED_OPACITY: f32 = 0.45;

#[derive(Clone, Debug, PartialEq)]
pub struct SessionFace {
    pub key: String,
    pub opacity: f32,
    pub badge: String,
    pub status_text: Option<String>,
    pub footer_text: Option<String>,
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
    pub scheduled: bool,
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
    pub reduced_motion: bool,
    pub clock: Option<String>,
    /// Sessions per agent: (name, colour, count).
    pub agents: Vec<(String, Rgb, usize)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyPressTarget {
    Session(String),
    Hub,
    Empty,
    Disabled,
    Back,
    Controlled(u64),
}

/// Resolve an action from the face actually shown, independent of later reordering.
pub fn target_for_face(face: &KeyFace, quota_view: bool) -> KeyPressTarget {
    if let KeyFace::Controlled(_, generation, _) = face { return KeyPressTarget::Controlled(*generation); }
    if matches!(face, KeyFace::Back) { return KeyPressTarget::Back; }
    if quota_view { return KeyPressTarget::Disabled; }
    match face {
        KeyFace::Session(s) => KeyPressTarget::Session(s.key.clone()),
        KeyFace::Controlled(_, generation, _) => KeyPressTarget::Controlled(*generation),
        KeyFace::Hub(_) => KeyPressTarget::Hub,
        KeyFace::Empty => KeyPressTarget::Empty,
        KeyFace::Released | KeyFace::Quota(_) | KeyFace::QuotaHub | KeyFace::Step(_) | KeyFace::Back => KeyPressTarget::Disabled,
    }
}

#[derive(Clone, Debug)]
pub enum Action {
    Open { agent: String, session: String },
    OpenUrl(String),
    OpenApp(String),
    ShowWindow,
}

pub struct Core {
    pub settings: Settings,
    pub display: crate::display::DisplayController,
    pub agents: Vec<AgentDef>,
    pub sessions: Vec<Session>,
    order: Vec<String>,
    // Stable physical positions, reconciled lazily whenever the keypad mapping is read.
    layout: std::cell::RefCell<crate::layout::KeypadLayout>,
    pub island_layout: std::cell::RefCell<crate::layout::IslandLayout>,
    source_policy: SourcePolicy,
    aliases: std::collections::HashMap<(String, String), String>,
    suppressed_local: std::collections::HashMap<(String, String), Observation>,
    dismissed: std::collections::HashMap<String, f64>,
    hub_hold_keys: Vec<String>,
    hub_hold_page: Option<usize>,
    pub page: usize,
    pub quota_view: bool,
    pub quota_page: usize,
    pub detail_key: Option<String>,
    pub detail_page: usize,
    pub quotas: Vec<crate::quota::Quota>,
    pub in_control: bool,
    pub hardware: Hardware,
    pub keypad_connected: Option<bool>,
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
            display: crate::display::DisplayController::default(),
            agents,
            sessions: Vec::new(),
            order: Vec::new(),
            layout: std::cell::RefCell::new(crate::layout::KeypadLayout::default()),
            island_layout: std::cell::RefCell::new(crate::layout::IslandLayout::default()),
            source_policy: SourcePolicy::default(),
            aliases: std::collections::HashMap::new(),
            suppressed_local: std::collections::HashMap::new(),
            dismissed: std::collections::HashMap::new(),
            hub_hold_keys: Vec::new(),
            hub_hold_page: None,
            page: 0,
            quota_view: false,
            quota_page: 0,
            detail_key: None,
            detail_page: 0,
            quotas: Vec::new(),
            in_control,
            hardware: Hardware::Off,
            keypad_connected: None,
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

    fn reconcile_alias(&mut self, agent: &str, host: &str, session: &str) {
        if host == session { return; }
        let prior = self.aliases.get(&(agent.to_string(), host.to_string())).cloned();
        if let Some(prior) = prior {
            if prior != session && prior != host { self.reconcile_alias(agent, &prior, session); }
        }
        for ((owner, _), canonical) in &mut self.aliases {
            if owner == agent && canonical == host { *canonical = session.to_string(); }
        }
        self.aliases.insert((agent.to_string(), host.to_string()), session.to_string());
        if agent == "claude-code" { self.source_policy.remap(host, session); }
        if let Some(mut pending) = self.suppressed_local.remove(&(agent.to_string(), host.to_string())) {
            pending.session = session.to_string();
            self.suppressed_local.insert((agent.to_string(), session.to_string()), pending);
        }
        let old_key = format!("{agent}:{host}");
        let new_key = format!("{agent}:{session}");
        if let Some(at) = self.dismissed.remove(&old_key) {
            let canonical_active = self.sessions.iter().any(|s| s.agent == agent && s.id == session && matches!(s.activity.group(), Group::Working | Group::Waiting));
            if !canonical_active {
                self.dismissed.entry(new_key.clone()).and_modify(|v| *v = v.max(at)).or_insert(at);
            }
        }
        for key in &mut self.hub_hold_keys { if *key == old_key { *key = new_key.clone(); } }
        let Some(source) = self.sessions.iter().position(|s| s.agent == agent && s.id == host) else { return };
        let target = self.sessions.iter().position(|s| s.agent == agent && s.id == session);
        if let Some(target) = target {
            let old = self.sessions.remove(source);
            let target = if source < target { target - 1 } else { target };
            let existing = &mut self.sessions[target];
            existing.scheduled |= old.scheduled;
            if existing.activity.group() != Group::Working {
                existing.acknowledged |= old.acknowledged;
                existing.acknowledged_at = match (existing.acknowledged_at, old.acknowledged_at) {
                    (Some(a), Some(b)) => Some(a.max(b)),
                    (a, b) => a.or(b),
                };
            } else {
                existing.acknowledged = false;
                existing.acknowledged_at = None;
            }
            existing.pending_attention |= old.pending_attention;
            existing.unread = existing.unread.max(old.unread);
        } else {
            self.sessions[source].id = session.to_string();
        }
        let old_key = format!("{agent}:{host}");
        let new_key = format!("{agent}:{session}");
        self.layout.borrow_mut().remap(&old_key, &new_key);
        self.island_layout.borrow_mut().remap(&old_key, &new_key);
        if self.detail_key.as_deref() == Some(old_key.as_str()) {
            self.detail_key = Some(new_key.clone());
        }
        for target in &mut self.key_target {
            if let Some(KeyPressTarget::Session(key)) = target {
                if *key == old_key { *key = new_key.clone(); }
            }
        }
        if self.order.contains(&new_key) {
            self.order.retain(|key| key != &old_key);
        } else {
            for key in &mut self.order {
                if *key == old_key { *key = new_key.clone(); }
            }
        }
    }

    /// Desktop apps to follow: (agent id, bundle id, electron).
    pub fn watched_apps(&self) -> Vec<(String, String, bool)> {
        let mut apps: Vec<_> = self.agents
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
            .collect();
        if self.agent_enabled("claude-code") && !apps.iter().any(|(agent, _, _)| agent == "claude-desktop") {
            apps.push(("claude-desktop".into(), "com.anthropic.claudefordesktop".into(), true));
        }
        apps
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

    pub fn decode(&self, inc: Incoming) -> Observation {
        let def = self.agent(inc.agent.trim());
        crate::observation::decode_incoming(inc, def, now())
    }

    #[cfg(test)]
    pub fn apply(&mut self, inc: Incoming) { let obs = self.decode(inc); self.observe(obs); }

    pub fn observe(&mut self, mut obs: Observation) {
        obs.text = obs.text.map(|text| crate::observation::first_line(&text, 240));
        obs.title = obs.title.map(|title| crate::observation::first_line(&title, 60));
        let agent_id = obs.agent.trim().to_string();
        if let Some(canonical) = self.aliases.get(&(agent_id.clone(), obs.session.clone())) { obs.session = canonical.clone(); }
        if obs.kind == ObservationKind::Discovery {
            // Prefer an already tracked exact alias; an untracked alias does not change
            // the host identity before its normal producer establishes the link.
            if let Some(existing) = obs.aliases.iter().find_map(|alias| {
                let canonical = self.aliases.get(&(agent_id.clone(), alias.clone())).unwrap_or(alias);
                self.sessions.iter().any(|s| s.agent == agent_id && s.id == *canonical).then(|| canonical.clone())
            }) {
                let host = std::mem::replace(&mut obs.session, existing);
                obs.aliases = vec![host];
            } else {
                obs.aliases.clear();
            }
        }
        if !self.source_policy.observe(&agent_id, &obs.session, obs.source, obs.observed_at) { return; }
        for alias in obs.aliases.clone() { self.reconcile_alias(&agent_id, &alias, &obs.session); }
        if let Some(canonical) = self.aliases.get(&(agent_id.clone(), obs.session.clone())) { obs.session = canonical.clone(); }
        let sid = obs.session.clone();
        let t = obs.observed_at;
        if obs.kind == ObservationKind::Heartbeat {
            if !self.source_policy.heartbeat_needed(&agent_id, &sid, obs.source, t) { return; }
            let Some(mut pending) = self.suppressed_local.remove(&(agent_id.clone(), sid.clone())) else { return };
            pending.observed_at = t;
            obs = pending;
        }
        let key = format!("{agent_id}:{sid}");
        let mut revived = false;
        if let Some(&dismissed_at) = self.dismissed.get(&key) {
            let dated_fresh = [obs.recorded_at, obs.text_at].into_iter().flatten().any(|at| at.is_finite() && at > dismissed_at && at <= t);
            let live_fresh = obs.recorded_at.is_none() && obs.text_at.is_none()
                && t > dismissed_at && (obs.event.is_some() || matches!(obs.source, crate::observation::Source::DesktopLive | crate::observation::Source::LocalSnapshot));
            revived = (dated_fresh || live_fresh) && obs.activity.is_some_and(|activity|
                matches!(activity.group(), Group::Working | Group::Waiting)
                || (activity == Activity::Done && dated_fresh));
            if !revived { return; }
            self.dismissed.remove(&key);
        }
        if obs.kind == ObservationKind::Discovery && !revived {
            if let Some(existing) = self.sessions.iter_mut().find(|s| s.agent == agent_id && s.id == sid) {
                if obs.scheduled && !existing.scheduled { existing.scheduled = true; self.revision += 1; }
                return;
            }
        }
        let recorded_at = if obs.kind == ObservationKind::Discovery {
            obs.recorded_at.filter(|at| at.is_finite() && *at > 0.0 && *at <= t)
        } else { None };
        let snapshot = obs.clone();
        let Some(activity) = obs.activity else {
            if obs.scheduled {
                if let Some(s) = self.sessions.iter_mut().find(|s| s.agent == agent_id && s.id == sid) {
                    if !s.scheduled { s.scheduled = true; self.revision += 1; }
                }
            }
            return;
        };
        let def = self.agent(&agent_id).cloned();
        let event = obs.event;
        let mut text = obs.text;
        let title = obs.title;
        let tool_name = obs.tool_name.as_deref().unwrap_or("");
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
                    since: recorded_at.unwrap_or(t),
                    updated: recorded_at.unwrap_or(t),
                    turn_started: recorded_at.unwrap_or(t),
                    subagents: 0,
                    subagent_at: t,
                    app_bundle: String::new(),
                    open_url: None,
                    tty: String::new(),
                    term: String::new(),
                    pid: 0,
                    unread: 0,
                    scheduled: false,
                    acknowledged: false,
                    acknowledged_at: None,
                    pending_attention: false,
                    pending_text: None,
                    steps: Vec::new(),
                });
                self.sessions.len() - 1
            }
        };
        let s = &mut self.sessions[idx];
        if revived { s.acknowledged = false; s.acknowledged_at = None; }
        s.scheduled |= obs.scheduled;
        let old = s.activity;
        let incoming_activity = activity;
        let activity = self.source_policy.activity(&agent_id, &sid, obs.source, t, old, activity);
        let suppressed = incoming_activity != activity;
        self.source_policy.record_local(&agent_id, &sid, obs.source, suppressed);
        if suppressed { text = None; }
        if agent_id == "claude-code" && obs.source == crate::observation::Source::LocalSnapshot {
            let key = (agent_id.clone(), sid.clone());
            if incoming_activity != activity { self.suppressed_local.insert(key, snapshot); }
            else { self.suppressed_local.remove(&key); }
        }
        // Read-receipt changes do not turn an already idle completed Codex thread into
        // a newly completed turn. Its local record can repeat the same final message.
        let activity = if old == Activity::Idle && activity == Activity::Done && event.is_none()
            && obs.unread.is_some() && text.as_deref() == Some(s.text.as_str())
            && obs.text_at.is_some_and(|at| at > 0.0 && at <= s.text_at)
        { old } else { activity };
        // An old local record keeps its real age when first discovered after app launch.
        let first_idle_time = if new_session && activity.group() == Group::Idle {
            obs.text_at.filter(|at| *at > 0.0 && *at <= t)
        } else { None };
        if let Some(v) = obs.app { s.app_bundle = v; }
        if let Some(v) = obs.open_url { s.open_url = Some(v); }
        if let Some(v) = obs.tty { let v = v.trim(); s.tty = if v.starts_with("/dev/") { v.to_string() } else { format!("/dev/{v}") }; }
        if let Some(v) = obs.pid { s.pid = v; }
        if let Some(v) = obs.term { s.term = v; }
        if let Some(title) = title { s.title = title; }
        if let Some(steps) = &obs.steps {
            for value in steps.iter().take(128) {
                if let Some(existing) = s.steps.iter_mut().find(|item| item.signature == value.signature) { *existing = value.clone(); }
                else { s.steps.push(value.clone()); }
            }
            if s.steps.len() > 128 { s.steps.drain(..s.steps.len() - 128); }
        }
        // Record meaningful observed updates once, not repeated local polls or streaming fragments.
        let step_text = text.clone().unwrap_or_default();
        let meaningful = event != Some(ObservationEvent::TurnStarted)
            && (activity == Activity::Tool || (!step_text.is_empty() && (activity != Activity::Streaming || old != Activity::Streaming)));
        if meaningful && !suppressed && obs.steps.is_none() {
            let kind = if activity == Activity::Tool { "Tool" } else { "Update" }.to_string();
            let title = if activity == Activity::Tool && !tool_name.is_empty() { tool_name.to_string() } else { format!("{:?}", activity) };
            let status = format!("{:?}", activity);
            let identity = obs.step_id.as_deref().unwrap_or("");
            let signature = if identity.is_empty() { format!("{kind}|{status}|{title}|{step_text}") } else { format!("event:{identity}") };
            let step = SessionStep { kind, status, title, text: step_text, signature: signature.clone() };
            if let Some(existing) = s.steps.iter_mut().find(|item| item.signature == signature) { *existing = step; }
            else {
                s.steps.push(step);
                if s.steps.len() > 128 { s.steps.remove(0); }
            }
        }
        if let Some(text) = text {
            if text != s.text && !s.text.is_empty() && t - s.text_at < TEXT_DWELL_S && obs.text_at.is_none() {
                // The current text hasn't had its 1.5 s yet: show the new one after it.
                s.pending_text = Some(text);
            } else if text != s.text {
                s.pending_text = None;
                s.text = text;
                // A message the agent's records date (Codex) keeps its own time.
                s.text_at = obs.text_at.filter(|v| *v > 0.0).unwrap_or(t);
            }
        }
        if obs.clear_text && !suppressed {
            // A new live step without words yet: drop the previous step's text (after its dwell).
            if !s.text.is_empty() && t - s.text_at < TEXT_DWELL_S {
                s.pending_text = Some(String::new());
            } else {
                s.text.clear();
                s.text_at = t;
            }
        }
        if let Some(n) = obs.unread {
            s.unread = n.min(999);
        }
        if activity == Activity::Tool && !suppressed {
            s.tool = Some(ToolKind::from_tool_name(tool_name));
            if s.tool == Some(ToolKind::Delegate) {
                s.subagents += 1;
                s.subagent_at = t;
            }
        }
        if event == Some(ObservationEvent::SubagentStopped) {
            s.subagents = s.subagents.saturating_sub(1);
            s.subagent_at = t;
        }
        if old != activity {
            s.since = first_idle_time.unwrap_or(t);
        }
        let og = old.group();
        let ng = activity.group();
        if (og != Group::Working && ng == Group::Working) || event == Some(ObservationEvent::TurnStarted) {
            s.turn_started = t;
            s.acknowledged = false;
            s.acknowledged_at = None;
            s.pending_attention = false;
        }
        if s.acknowledged { s.unread = 0; }
        if !s.acknowledged && (activity == Activity::Done || s.unread > 0) {
            s.pending_attention = true;
        }
        s.activity = activity;
        s.updated = recorded_at.unwrap_or(t);
        if let Some(recorded_at) = recorded_at { s.since = recorded_at; s.turn_started = recorded_at; }
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
        let mut changed = self.display.prune(t);
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
        let recency = (self.settings.recency_minutes as f64 * 60.0).min(REMOVE_AFTER_S);
        self.sessions.retain(|s| s.pending_attention || t - s.updated < REMOVE_AFTER_S
            || (s.acknowledged && s.acknowledged_at.is_some_and(|at| t - at < if s.scheduled { REMOVE_AFTER_S } else { recency })));
        if self.sessions.len() != before {
            changed = true;
        }
        if changed {
            self.resort();
            self.touch();
        }
        for i in 0..KEY_COUNT {
            if let (Some(d), Some(target)) = (self.key_down[i], self.key_target[i].clone()) {
                if !self.hold_fired[i] && self.in_control && t - d >= DETAIL_HOLD_S {
                    match target {
                        KeyPressTarget::Session(key) => { self.hold_fired[i] = true; self.open_session_steps(&key); }
                        KeyPressTarget::Hub => { self.hold_fired[i] = true; self.clear_hub_hold(); }
                        _ => {}
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

    pub fn island_visible(&self) -> bool {
        match self.settings.island_visibility {
            IslandVisibility::Never => false,
            IslandVisibility::Always | IslandVisibility::QuotasAlways => true,
            IslandVisibility::WhenDisconnected => self.keypad_connected == Some(false),
        }
    }

    pub fn island_quotas_only(&self) -> bool {
        self.settings.island_visibility == IslandVisibility::QuotasAlways && self.keypad_connected != Some(false)
    }

    pub fn visible(&self) -> Vec<&Session> {
        let t = now();
        let recency = self.settings.recency_minutes as f64 * 60.0;
        let recent = |s: &&Session| crate::tracking::session_visible(s, t, recency);
        let mut v: Vec<&Session> = self
            .order
            .iter()
            .filter_map(|k| self.sessions.iter().find(|s| &s.key() == k))
            .filter(|s| self.agent_enabled(&s.agent) && !self.dismissed.contains_key(&s.key()))
            .filter(recent)
            .collect();
        for s in &self.sessions {
            if !self.order.contains(&s.key()) && self.agent_enabled(&s.agent) && !self.dismissed.contains_key(&s.key()) && recent(&s) {
                v.push(s);
            }
        }
        v
    }

    /// Capture the exact target and acknowledge inactive work on open.
    pub fn acknowledge_open(&mut self, agent: &str, session: &str) -> Option<Session> {
        let s = self.sessions.iter_mut().find(|s| s.agent == agent && s.id == session)?;
        let target = s.clone();
        let dismiss = !matches!(s.activity.group(), Group::Working | Group::Waiting);
        if dismiss {
            s.acknowledged = true;
            s.acknowledged_at = Some(now());
            s.pending_attention = false;
            s.unread = 0;
            self.touch();
        }
        Some(target)
    }

    /// All keypad slots, including empty slots reserved on the first page. Keep this as the
    /// single mapping for drawing, key presses and the hub's waiting-session shortcut.
    fn keypad_slots(&self) -> Vec<Option<&Session>> {
        self.keypad_slots_at(now())
    }

    fn keypad_slots_at(&self, t: f64) -> Vec<Option<&Session>> {
        let (recent, old_idle): (Vec<_>, Vec<_>) = self.visible().into_iter().partition(|s| {
            s.acknowledged || s.pending_attention || s.unread > 0 || s.activity.group() != Group::Idle || t - s.since <= IDLE_SECOND_PAGE_AFTER_S
        });
        let positions = self.layout.borrow_mut().project(&recent, &old_idle);
        positions.iter().map(|slot| slot.as_ref().and_then(|key| self.sessions.iter().find(|s| s.key() == *key))).collect()
    }

    pub fn pages(&self) -> usize {
        self.keypad_slots().len().div_ceil(SESSION_KEYS_PER_PAGE).max(1)
    }

    pub fn waiting_count(&self) -> usize {
        self.visible().iter().filter(|s| !s.acknowledged && s.activity.group() == Group::Waiting).count()
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

    pub fn display_command(&mut self, command: crate::display::DisplayCommand) -> Result<Value, crate::display::DisplayError> {
        let result = self.display.execute(command, now());
        if result.is_ok() { self.touch(); }
        result
    }

    pub fn release_display(&mut self) { self.display.clear(); self.touch(); }

    fn overlay_display(&self, mut faces: [KeyFace; KEY_COUNT]) -> [KeyFace; KEY_COUNT] {
        let t = now();
        for (i, face) in faces.iter_mut().enumerate() {
            if let Some((scene, generation)) = self.display.scene(i, t) { *face = KeyFace::Controlled(scene, generation, self.settings.motion == Motion::Reduced); }
        }
        faces
    }

    pub fn quota_faces(&self, page: usize) -> [KeyFace; KEY_COUNT] {
        let mut out = std::array::from_fn(|_| KeyFace::Empty);
        for (row, entries) in self.quota_rows().into_iter().skip(page * 2).take(2).enumerate() {
            for (col, mut quota) in entries.into_iter().enumerate() {
                quota.remaining = crate::quota::displayed_remaining(&quota, now() as i64);
                out[row * 3 + col] = KeyFace::Quota(quota);
            }
        }
        out[HUB_KEY] = KeyFace::QuotaHub;
        self.overlay_display(out)
    }

    pub fn faces(&self) -> [KeyFace; KEY_COUNT] {
        let mut out: [KeyFace; KEY_COUNT] = std::array::from_fn(|_| KeyFace::Empty);
        if !self.in_control {
            for f in out.iter_mut() {
                *f = KeyFace::Released;
            }
            return out;
        }
        if let Some(key) = &self.detail_key {
            if let Some(session) = self.sessions.iter().find(|s| s.key() == *key) {
                for (i, step) in session.steps.iter().skip(self.detail_page * SESSION_KEYS_PER_PAGE).take(SESSION_KEYS_PER_PAGE).enumerate() { out[i] = KeyFace::Step(step.clone()); }
            }
            out[HUB_KEY] = KeyFace::Back;
            return self.overlay_display(out);
        }
        if self.quota_view {
            return self.quota_faces(self.quota_page);
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
                opacity: if s.acknowledged { ACKNOWLEDGED_OPACITY } else { 1.0 },
                badge: def.map(|d| d.badge.clone()).unwrap_or_else(|| badge_from_name(&s.agent)),
                status_text: None,
                footer_text: None,
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
                scheduled: s.scheduled,
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
        let count = |g: Group| vis.iter().filter(|s| !s.acknowledged && s.activity.group() == g).count();
        out[HUB_KEY] = KeyFace::Hub(HubFace {
            waiting: count(Group::Waiting),
            working: count(Group::Working),
            done: count(Group::Done),
            page,
            pages: self.pages(),
            dots: vis.iter().take(12).map(|s| self.colours(s).0).collect(),
            waiting_colour: self.waiting_colour(),
            reduced_motion: self.settings.motion == Motion::Reduced,
            clock: self.settings.show_hub_clock.then(|| chrono::Local::now().format("%H:%M").to_string()),
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
        self.overlay_display(out)
    }

    // ---- input (on-screen keypad, keyboard, physical keypad)

    pub fn open_session_steps(&mut self, key: &str) {
        if self.sessions.iter().any(|s| s.key() == key) {
            self.detail_key = Some(key.to_string()); self.detail_page = 0; self.touch();
        }
    }

    pub fn key_down(&mut self, i: usize) {
        if i >= KEY_COUNT { return; }
        let faces = self.faces();
        let target = target_for_face(&faces[i], self.quota_view);
        self.key_down_with_target(i, target);
    }

    fn clear_hub_hold(&mut self) {
        let keys = std::mem::take(&mut self.hub_hold_keys);
        let page = self.hub_hold_page.take();
        if self.quota_view || self.detail_key.is_some() || page != Some(self.page.min(self.pages() - 1)) { return; }
        let at = now();
        for s in &mut self.sessions {
            if keys.contains(&s.key()) && !matches!(s.activity.group(), Group::Working | Group::Waiting) {
                s.acknowledged = true;
                s.acknowledged_at = Some(at);
                s.pending_attention = false;
                s.unread = 0;
                self.dismissed.insert(s.key(), at);
            }
        }
        let acknowledged = self.sessions.iter().filter(|s| s.acknowledged).map(Session::key).collect();
        self.layout.borrow_mut().keep_positions(&acknowledged);
        self.touch();
    }

    pub fn key_down_with_target(&mut self, i: usize, target: KeyPressTarget) {
        if i < KEY_COUNT && self.key_down[i].is_none() && target != KeyPressTarget::Disabled {
            if target == KeyPressTarget::Hub && !self.quota_view && self.detail_key.is_none() {
                let page = self.page.min(self.pages() - 1);
                self.hub_hold_page = Some(page);
                self.hub_hold_keys = self.keypad_slots().iter().skip(page * SESSION_KEYS_PER_PAGE).take(SESSION_KEYS_PER_PAGE).filter_map(|s| s.map(|s| s.key())).collect();
            }
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
            if i == HUB_KEY { self.hub_hold_keys.clear(); self.hub_hold_page = None; }
        }
    }

    pub fn key_up(&mut self, i: usize) {
        if i >= KEY_COUNT { return; }
        let down = self.key_down[i].take();
        let target = self.key_target[i].take();
        if let (Some(d), Some(KeyPressTarget::Session(key))) = (down, target.as_ref()) {
            if !self.hold_fired[i] && self.in_control && now() - d >= DETAIL_HOLD_S {
                self.hold_fired[i] = true;
                self.open_session_steps(key);
            }
        }
        if let (Some(d), Some(KeyPressTarget::Hub)) = (down, target.as_ref()) {
            if !self.hold_fired[i] && self.in_control && now() - d >= DETAIL_HOLD_S {
                self.hold_fired[i] = true;
                self.clear_hub_hold();
            }
        }
        if matches!(target, Some(KeyPressTarget::Hub)) && !self.hold_fired[i] { self.hub_hold_keys.clear(); self.hub_hold_page = None; }
        if down.is_none() || self.hold_fired[i] || !self.in_control { return; }
        match target {
            Some(KeyPressTarget::Controlled(generation)) => {
                match self.display.press(i, generation, now()) {
                    Some(crate::display::SceneAction::OpenUrl { url }) => self.actions.push(Action::OpenUrl(url)),
                    Some(crate::display::SceneAction::OpenApp { bundle_id }) => self.actions.push(Action::OpenApp(bundle_id)),
                    None => {}
                }
                self.touch();
            }
            Some(KeyPressTarget::Back) => { self.detail_key = None; self.detail_page = 0; self.touch(); }
            Some(KeyPressTarget::Hub) => {
                let slots = self.keypad_slots();
                if let Some(pos) = slots.iter().position(|s| s.is_some_and(|s| !s.acknowledged && s.activity.group() == Group::Waiting)) {
                    self.page = pos / SESSION_KEYS_PER_PAGE;
                } else { self.actions.push(Action::ShowWindow); }
                self.touch();
            }
            Some(KeyPressTarget::Session(key)) if self.settings.key_actions_enabled => {
                if let Some((agent, session)) = self.sessions.iter().find(|s| s.key() == key).map(|s| (s.agent.clone(), s.id.clone())) {
                    if self.acknowledge_open(&agent, &session).is_some() {
                        self.actions.push(Action::Open { agent, session });
                    }
                }
            }
            Some(KeyPressTarget::Empty) if self.detail_key.is_none() && self.settings.key_actions_enabled && !self.settings.empty_key_url.trim().is_empty() => {
                self.actions.push(Action::OpenUrl(self.settings.empty_key_url.clone()));
            }
            _ => {}
        }
    }

    pub fn hold_progress(&self, i: usize) -> f32 {
        if self.quota_view { return 0.0; }
        match self.key_down.get(i).copied().flatten() {
            Some(d) if !self.hold_fired[i] => ((now() - d) / DETAIL_HOLD_S).clamp(0.0, 1.0) as f32,
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
            if let Some(key) = &self.detail_key {
                let pages = self.sessions.iter().find(|s| s.key() == *key).map(|s| s.steps.len().div_ceil(SESSION_KEYS_PER_PAGE).max(1)).unwrap_or(1);
                self.detail_page = if next { (self.detail_page + 1).min(pages - 1) } else { self.detail_page.saturating_sub(1) };
                self.hub_hold_keys.clear();
                self.hub_hold_page = None;
                self.key_down = [None; KEY_COUNT];
                self.key_target = std::array::from_fn(|_| None);
                self.hold_fired = [false; KEY_COUNT];
                self.touch(); return;
            }
            let pages = self.pages();
            self.hub_hold_keys.clear();
            self.hub_hold_page = None;
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
                open_url: None,
                tty: String::new(),
                term: String::new(),
                pid: 0,
                unread: 0,
                scheduled: n == 0,
                acknowledged: n == 4,
                acknowledged_at: (n == 4).then_some(t),
                pending_attention: matches!(act, Activity::Done) && n != 4,
                pending_text: None,
                steps: Vec::new(),
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
    #[test]
    fn optional_hub_clock_is_formatted_only_on_the_normal_hub_face() {
        use super::*;

        fn valid_hh_mm(value: &str) -> bool {
            let bytes = value.as_bytes();
            if bytes.len() != 5 || bytes[2] != b':' || !bytes[..2].iter().all(u8::is_ascii_digit) || !bytes[3..].iter().all(u8::is_ascii_digit) {
                return false;
            }
            let hour = value[..2].parse::<u32>().ok();
            let minute = value[3..].parse::<u32>().ok();
            matches!((hour, minute), (Some(h), Some(m)) if h < 24 && m < 60)
        }

        let mut core = Core::new(Settings::default());
        core.in_control = true;
        assert!(!core.settings.show_hub_clock);
        assert!(matches!(&core.faces()[HUB_KEY], KeyFace::Hub(hub) if hub.clock.is_none()));

        core.settings.show_hub_clock = true;
        let KeyFace::Hub(hub) = &core.faces()[HUB_KEY] else { panic!("normal hub face expected") };
        assert!(hub.clock.as_deref().is_some_and(valid_hh_mm), "hub clock uses a valid local HH:mm string");

        core.settings.motion = Motion::Reduced;
        let KeyFace::Hub(hub) = &core.faces()[HUB_KEY] else { panic!("normal hub face expected with reduced motion") };
        assert!(hub.reduced_motion);
        assert!(hub.clock.as_deref().is_some_and(valid_hh_mm), "reduced motion must not hide the clock");

        core.detail_key = Some("missing-session".into());
        assert!(matches!(core.faces()[HUB_KEY], KeyFace::Back), "detail navigation keeps the Back face without a clock");
        core.detail_key = None;
        core.quota_view = true;
        assert!(matches!(core.faces()[HUB_KEY], KeyFace::QuotaHub), "quota navigation keeps the quota hub without a clock");
        core.quota_view = false;

        let token = core.display_command(crate::display::DisplayCommand::Claim {
            owner: "clock-test".into(),
            keys: vec![HUB_KEY],
            ttl_seconds: Some(60.0),
        }).unwrap()["token"].as_str().unwrap().to_owned();
        core.display_command(crate::display::DisplayCommand::Set {
            token,
            keys: [(HUB_KEY.to_string(), Some(crate::display::Scene {
                kind: "native".into(),
                title: "Controlled hub".into(),
                ..crate::display::Scene::default()
            }))].into(),
        }).unwrap();
        assert!(matches!(&core.faces()[HUB_KEY], KeyFace::Controlled(scene, _, true) if scene.kind == "native"), "full controlled display overrides the normal hub and its clock");
    }

    #[test]
    fn desktop_code_watch_and_identity_reconciliation() {
        use super::*;
        let mut core = Core::new(Settings::default());
        core.settings.disabled_agents.push("claude-desktop".into());
        assert!(core.watched_apps().iter().any(|(a, _, _)| a == "claude-desktop"));
        core.settings.disabled_agents.push("claude-code".into());
        assert!(!core.watched_apps().iter().any(|(a, _, _)| a == "claude-desktop"));
        core.settings.disabled_agents.clear();
        core.apply(Incoming { agent: "claude-code".into(), event: None, body: serde_json::json!({"session":"local_abc", "state":"idle"}) });
        core.sessions[0].acknowledged = true;
        core.sessions[0].pending_attention = true;
        core.detail_key = Some("claude-code:local_abc".into());
        core.key_target[0] = Some(KeyPressTarget::Session("claude-code:local_abc".into()));
        core.reconcile_alias("claude-code", "local_abc", "real-id");
        assert_eq!(core.sessions.len(), 1);
        assert_eq!(core.sessions[0].id, "real-id");
        assert!(core.sessions[0].acknowledged && core.sessions[0].pending_attention);
        assert_eq!(core.detail_key.as_deref(), Some("claude-code:real-id"));
        assert_eq!(core.key_target[0], Some(KeyPressTarget::Session("claude-code:real-id".into())));
    }

    #[test]
    fn alias_collision_deduplicates_slot_shelf_detail_and_captured_key_target() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        let mut host = Observation::new("claude-code", "host", Source::Other, now());
        host.activity = Some(Activity::Idle);
        core.observe(host);
        let mut exact = Observation::new("claude-code", "exact", Source::Other, now());
        exact.activity = Some(Activity::Idle);
        core.observe(exact);
        let _ = core.keypad_slots();
        core.acknowledge_open("claude-code", "host");
        let slots = core.keypad_slots();
        let host_slot = slots.iter().position(|slot| slot.is_some_and(|session| session.id == "host")).unwrap();
        core.page = host_slot / SESSION_KEYS_PER_PAGE;
        let physical_key = host_slot % SESSION_KEYS_PER_PAGE;
        core.key_down(physical_key);
        core.detail_key = Some("claude-code:host".into());

        let mut resolved = Observation::new("claude-code", "exact", Source::Other, now());
        resolved.aliases.push("host".into());
        resolved.activity = Some(Activity::Idle);
        core.observe(resolved);

        assert_eq!(core.sessions.iter().filter(|s| s.agent == "claude-code").count(), 1);
        assert_eq!(core.sessions.iter().find(|s| s.agent == "claude-code").unwrap().id, "exact");
        assert!(core.sessions.iter().find(|s| s.agent == "claude-code").unwrap().acknowledged);
        assert_eq!(core.detail_key.as_deref(), Some("claude-code:exact"));
        let slots = core.keypad_slots();
        assert_eq!(slots.iter().filter(|slot| slot.is_some_and(|session| session.id == "exact")).count(), 1);
        assert!(!slots.iter().flatten().any(|session| session.id == "host"));

        core.key_up(physical_key);
        assert!(core.actions.iter().any(|action| matches!(action, Action::Open { agent, session } if agent == "claude-code" && session == "exact")), "the held key must resolve to the canonical session after alias reconciliation");
    }

    #[test]
    fn alias_reconciliation_keeps_a_working_canonical_target_bright() {
        let mut core = Core::new(Settings::default());
        let mut host = Observation::new("claude-code", "host-id", Source::Other, now());
        host.activity = Some(Activity::Idle);
        core.observe(host);
        core.sessions.iter_mut().find(|s| s.id == "host-id").unwrap().acknowledged = true;
        core.sessions.iter_mut().find(|s| s.id == "host-id").unwrap().acknowledged_at = Some(now());

        let mut canonical = Observation::new("claude-code", "cli-id", Source::Other, now());
        canonical.activity = Some(Activity::Thinking);
        core.observe(canonical);
        core.sessions.iter_mut().find(|s| s.id == "cli-id").unwrap().acknowledged = true;
        core.sessions.iter_mut().find(|s| s.id == "cli-id").unwrap().acknowledged_at = Some(now());

        let mut resolved = Observation::new("claude-code", "cli-id", Source::Other, now());
        resolved.aliases.push("host-id".into());
        resolved.activity = Some(Activity::Thinking);
        core.observe(resolved);

        assert_eq!(core.sessions.len(), 1);
        let session = &core.sessions[0];
        assert_eq!(session.id, "cli-id");
        assert_eq!(session.activity, Activity::Thinking);
        assert!(!session.acknowledged);
        assert_eq!(session.acknowledged_at, None);
        assert!(matches!(&core.faces()[0], KeyFace::Session(face) if face.key == "claude-code:cli-id" && face.opacity == 1.0));
    }

    #[test]
    fn desktop_live_lease_protects_state_and_expires() {
        use super::*;
        let mut core = Core::new(Settings::default());
        let apply = |core: &mut Core, state: &str, local: bool| core.apply(Incoming {
            agent: "claude-code".into(), event: None,
            body: serde_json::json!({"session":"s", "state":state, "_local_snapshot":local}),
        });
        apply(&mut core, "streaming", false);
        core.source_policy.observe("claude-code", "s", Source::DesktopLive, now());
        core.sessions[0].acknowledged = true;
        apply(&mut core, "idle", true);
        assert_eq!(core.sessions[0].activity, Activity::Streaming);
        assert!(core.sessions[0].acknowledged);
        apply(&mut core, "waitingQuestion", false);
        apply(&mut core, "thinking", true);
        assert_eq!(core.sessions[0].activity, Activity::WaitingQuestion);
        core.source_policy.observe("claude-code", "s", Source::DesktopInactive, now());
        apply(&mut core, "idle", true);
        assert_eq!(core.sessions[0].activity, Activity::Idle);
    }
    use super::*;

    fn add_test_session(core: &mut Core, id: &str, state: &str) {
        core.apply(Incoming {
            agent: "codex".into(),
            event: None,
            body: serde_json::json!({"session": id, "state": state, "title": id}),
        });
    }

    fn finish_hub_hold(core: &mut Core) {
        assert_eq!(core.key_target[HUB_KEY], Some(KeyPressTarget::Hub));
        core.key_down[HUB_KEY] = Some(now() - DETAIL_HOLD_S - 0.01);
        core.tick();
        core.key_up(HUB_KEY);
    }

    fn test_quota(provider: &str, window: &str, remaining: f64) -> crate::quota::Quota {
        crate::quota::Quota {
            provider: provider.into(), window: window.into(), remaining: Some(remaining), reset: None,
            reset_text: Some("Reset unknown".into()), updated: Some("2026-11-11T15:30:00Z".into()), source: "test".into(),
        }
    }

    #[test]
    fn island_visibility_setting_migrates_legacy_boolean_and_round_trips_enum() {
        assert_eq!(Settings::default().island_visibility, IslandVisibility::Always);

        let legacy_enabled: Settings = serde_json::from_value(serde_json::json!({"islandEnabled": true})).unwrap();
        let legacy_disabled: Settings = serde_json::from_value(serde_json::json!({"islandEnabled": false})).unwrap();
        let absent: Settings = serde_json::from_value(serde_json::json!({})).unwrap();
        assert_eq!(legacy_enabled.island_visibility, IslandVisibility::Always);
        assert_eq!(legacy_disabled.island_visibility, IslandVisibility::Never);
        assert_eq!(absent.island_visibility, IslandVisibility::Always);

        let mut settings = Settings::default();
        settings.island_visibility = IslandVisibility::WhenDisconnected;
        let stored = serde_json::to_value(&settings).unwrap();
        assert_eq!(stored["islandVisibility"], "whenDisconnected");
        assert!(stored.get("islandEnabled").is_none());
        let restored: Settings = serde_json::from_value(stored).unwrap();
        assert_eq!(restored.island_visibility, IslandVisibility::WhenDisconnected);
        settings.island_visibility = IslandVisibility::QuotasAlways;
        let stored = serde_json::to_value(&settings).unwrap();
        assert_eq!(stored["islandVisibility"], "quotasAlways");
        let legacy: Settings = serde_json::from_value(serde_json::json!({"islandVisibility": "quotasOnly"})).unwrap();
        assert_eq!(legacy.island_visibility, IslandVisibility::QuotasAlways);
        assert_eq!(serde_json::from_value::<Settings>(stored).unwrap().island_visibility, IslandVisibility::QuotasAlways);
    }

    #[test]
    fn island_when_disconnected_tracks_physical_presence_not_keypad_control_state() {
        let cases = [
            (IslandVisibility::Never, None, Hardware::NoKeypad, false, false),
            (IslandVisibility::Always, Some(false), Hardware::NoKeypad, false, true),
            (IslandVisibility::QuotasAlways, None, Hardware::Off, false, true),
            (IslandVisibility::QuotasAlways, Some(true), Hardware::Connected, false, true),
            (IslandVisibility::QuotasAlways, Some(false), Hardware::NoKeypad, false, true),
            (IslandVisibility::WhenDisconnected, None, Hardware::Off, false, false),
            // Connected means physically present even while Options+ still has control.
            (IslandVisibility::WhenDisconnected, Some(true), Hardware::Connected, false, false),
            (IslandVisibility::WhenDisconnected, Some(true), Hardware::Driving, true, false),
            (IslandVisibility::WhenDisconnected, Some(true), Hardware::Problem("busy".into()), false, false),
            (IslandVisibility::WhenDisconnected, Some(false), Hardware::NoKeypad, false, true),
            (IslandVisibility::WhenDisconnected, Some(false), Hardware::Off, true, true),
        ];
        for (visibility, connected, hardware, in_control, expected) in cases {
            let mut settings = Settings::default();
            settings.island_visibility = visibility;
            let mut core = Core::new(settings);
            core.keypad_connected = connected;
            core.hardware = hardware;
            core.in_control = in_control;
            assert_eq!(core.island_visible(), expected, "visibility={visibility:?}, connected={connected:?}, in_control={in_control}");
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

        // Acknowledgement keeps the captured inactive tile in place; a new session uses a free key.
        let first = core.sessions.iter_mut().find(|s| s.id == "first").unwrap();
        first.activity = Activity::Idle;
        first.acknowledged = true;
        let second = core.sessions.iter_mut().find(|s| s.id == "second").unwrap();
        second.activity = Activity::WaitingQuestion;
        core.resort();
        add_test_session(&mut core, "third", "thinking");
        assert!(matches!(&core.faces()[0], KeyFace::Session(face) if face.key == "codex:first" && face.opacity == ACKNOWLEDGED_OPACITY));
        assert!(matches!(&core.faces()[2], KeyFace::Session(face) if face.key == "codex:third"));

        core.key_up(0);
        assert!(matches!(core.actions.as_slice(), [Action::Open { agent, session }] if agent == "codex" && session == "first"));
    }

    #[test]
    fn acknowledged_idle_session_stays_dimmed_in_place_through_stale_updates() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "archived", "idle");
        let original_slot = core.keypad_slots().iter().position(|slot| slot.is_some_and(|s| s.id == "archived")).unwrap();
        core.sessions[0].acknowledged = true;

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"idle", "title":"Renamed", "_unread":3}),
        });
        assert!(core.sessions[0].acknowledged);
        assert_eq!(core.sessions[0].title, "Renamed");
        assert_eq!(core.sessions[0].unread, 0, "an acknowledged session does not become unread again from a stale poll");
        assert_eq!(core.visible()[0].id, "archived");
        let slots = core.keypad_slots_at(now());
        assert_eq!(slots[original_slot].unwrap().id, "archived", "acknowledged sessions keep their physical slot");
        assert!(matches!(&core.faces()[original_slot], KeyFace::Session(face) if face.opacity == ACKNOWLEDGED_OPACITY));

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"thinking"}),
        });
        assert!(!core.sessions[0].acknowledged);
        assert_eq!(core.visible()[0].id, "archived");
        assert!(matches!(&core.faces()[original_slot], KeyFace::Session(face) if face.opacity == 1.0));
    }

    #[test]
    fn acknowledged_work_stays_dimmed_until_new_turn_or_user_prompt_event() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "archived", "thinking");
        core.sessions[0].acknowledged = true;
        let key = core.keypad_slots().iter().position(|slot| slot.is_some_and(|s| s.id == "archived")).unwrap();

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"streaming", "title":"Still running"}),
        });
        assert!(core.sessions[0].acknowledged);
        assert_eq!(core.visible()[0].id, "archived");
        assert!(matches!(&core.faces()[key], KeyFace::Session(face) if face.opacity == ACKNOWLEDGED_OPACITY));

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived", "state":"idle"}),
        });
        assert!(core.sessions[0].acknowledged);
        assert_eq!(core.visible()[0].id, "archived");

        core.apply(Incoming {
            agent: "codex".into(), event: Some("UserPromptSubmit".into()),
            body: serde_json::json!({"session":"archived"}),
        });
        assert!(!core.sessions[0].acknowledged);
        assert_eq!(core.visible()[0].id, "archived");
        assert!(matches!(&core.faces()[key], KeyFace::Session(face) if face.opacity == 1.0));
    }

    #[test]
    fn scheduled_metadata_is_latched_without_reactivating_or_moving_session() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "scheduled", "idle");
        core.sessions[0].acknowledged = true;
        let old_updated = core.sessions[0].updated;
        core.apply(Incoming { agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"scheduled", "_scheduled":true}) });
        assert!(core.sessions[0].scheduled);
        assert!(core.sessions[0].acknowledged);
        assert_eq!(core.sessions[0].updated, old_updated);
        core.apply(Incoming { agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"scheduled", "state":"idle", "_scheduled":false}) });
        assert!(core.sessions[0].scheduled);
    }

    #[test]
    fn acknowledged_sessions_keep_their_page_one_slot_and_resume_at_full_brightness() {
        let mut settings = Settings::default();
        settings.recency_minutes = 240;
        let mut core = Core::new(settings);
        core.in_control = true;
        add_test_session(&mut core, "active", "thinking");
        add_test_session(&mut core, "old-idle", "idle");
        add_test_session(&mut core, "archived-waiting", "waiting_question");
        core.sessions.iter_mut().find(|s| s.id == "old-idle").unwrap().since = now() - IDLE_SECOND_PAGE_AFTER_S - 60.0;
        core.sessions.iter_mut().find(|s| s.id == "archived-waiting").unwrap().acknowledged = true;
        core.resort();

        let slots = core.keypad_slots_at(now());
        assert_eq!(slots[0].unwrap().id, "active");
        assert_eq!(slots[1].unwrap().id, "archived-waiting", "acknowledgement keeps the existing first-page slot");
        assert_eq!(slots[SESSION_KEYS_PER_PAGE].unwrap().id, "old-idle");
        assert_eq!(core.pages(), 2);
        assert_eq!(core.waiting_count(), 0, "archived waiting sessions are absent from normal counts");

        core.page = 1;
        assert!(matches!(&core.faces()[0], KeyFace::Session(face) if face.key == "codex:old-idle"));

        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"archived-waiting", "state":"thinking"}),
        });
        assert!(!core.sessions.iter().find(|s| s.id == "archived-waiting").unwrap().acknowledged);
        assert_eq!(core.visible()[0].id, "archived-waiting", "resumed work returns to the ordinary recent ordering");
        assert_eq!(core.keypad_slots_at(now())[0].unwrap().id, "active");
        assert_eq!(core.keypad_slots_at(now())[1].unwrap().id, "archived-waiting");
        core.page = 0;
        assert!(matches!(&core.faces()[1], KeyFace::Session(face) if face.key == "codex:archived-waiting" && face.opacity == 1.0));
    }

    #[test]
    fn acknowledging_inactive_sessions_keeps_slots_stable_across_polls_and_reordering() {
        let mut settings = Settings::default();
        settings.recency_minutes = 240;
        let mut core = Core::new(settings);
        core.in_control = true;
        add_test_session(&mut core, "normal-a", "thinking");
        add_test_session(&mut core, "normal-b", "waiting_question");
        add_test_session(&mut core, "shelved-a", "done");
        add_test_session(&mut core, "shelved-b", "idle");
        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"shelved-b", "state":"idle", "_unread":2}),
        });

        let before: Vec<_> = core.keypad_slots_at(now()).iter().map(|s| s.map(|s| s.key())).collect();

        for id in ["shelved-a", "shelved-b"] {
            let index = core.faces().iter().take(HUB_KEY).position(|face| {
                matches!(face, KeyFace::Session(session) if session.key == format!("codex:{id}"))
            }).expect("inactive conversation is available to click");
            let target = target_for_face(&core.faces()[index], core.quota_view);
            core.key_down_with_target(index, target);
            core.key_up(index);
        }
        assert!(matches!(core.actions.as_slice(), [
            Action::Open { agent: first_agent, session: first },
            Action::Open { agent: second_agent, session: second },
        ] if first_agent == "codex" && first == "shelved-a" && second_agent == "codex" && second == "shelved-b"));

        // Activity and metadata updates preserve these physical positions.
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"normal-a", "state":"idle", "title":"now idle"}) });
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"normal-b", "state":"waiting_permission", "title":"now waiting"}) });
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"shelved-a", "state":"idle", "title":"refreshed A"}) });
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"shelved-b", "state":"idle", "title":"refreshed B", "_unread":3}) });

        let slots = core.keypad_slots_at(now());
        for i in 0..before.len() {
            assert_eq!(slots[i].map(|s| s.key()), before[i], "slot {i} moved");
        }
        for id in ["shelved-a", "shelved-b"] {
            let slot = slots.iter().position(|s| s.is_some_and(|s| s.id == id)).unwrap();
            assert!(matches!(&core.faces()[slot], KeyFace::Session(face) if face.opacity == ACKNOWLEDGED_OPACITY));
        }
    }

    #[test]
    fn shelved_waiting_session_does_not_drive_hub_navigation() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "active", "thinking");
        add_test_session(&mut core, "archived-waiting", "waiting_question");
        core.sessions.iter_mut().find(|s| s.id == "archived-waiting").unwrap().acknowledged = true;
        core.resort();

        assert_eq!(core.waiting_count(), 0);
        assert!(matches!(&core.faces()[HUB_KEY], KeyFace::Hub(hub) if hub.waiting == 0));
        core.key_down(HUB_KEY);
        core.key_up(HUB_KEY);
        assert_eq!(core.page, 0, "the archived waiting tile must not redirect to its later page");
        assert!(matches!(core.actions.as_slice(), [Action::ShowWindow]));
    }

    #[test]
    fn hub_hold_clears_only_inactive_sessions_on_the_captured_page_without_moving_live_keys() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        for i in 0..10 { add_test_session(&mut core, &format!("session-{i}"), "idle"); }
        let before: Vec<_> = core.keypad_slots().iter().map(|s| s.map(|s| s.key())).collect();
        assert_eq!(before.len(), 10);

        let status = [
            (0, Activity::Thinking), (1, Activity::WaitingQuestion), (2, Activity::Done),
            (3, Activity::Error), (4, Activity::Idle), (5, Activity::Offline),
            (6, Activity::Done), (7, Activity::Idle), (8, Activity::Error), (9, Activity::Thinking),
        ];
        for (slot, activity) in status {
            let key = before[slot].as_ref().unwrap();
            let session = core.sessions.iter_mut().find(|s| s.key() == *key).unwrap();
            session.activity = activity;
            session.pending_attention = activity == Activity::Done;
            session.unread = u32::from(activity == Activity::Done) * 2;
        }
        let ids_before: Vec<_> = core.sessions.iter().map(|s| s.key()).collect();
        let title_before = core.sessions.iter().find(|s| s.id == "session-4").unwrap().title.clone();
        let updated_before = core.sessions.iter().find(|s| s.id == "session-4").unwrap().updated;

        core.key_down(HUB_KEY);
        assert_eq!(core.hub_hold_keys.len(), SESSION_KEYS_PER_PAGE);
        finish_hub_hold(&mut core);

        assert_eq!(core.sessions.iter().map(|s| s.key()).collect::<Vec<_>>(), ids_before, "clearing does not delete tracked sessions");
        assert_eq!(core.page, 0);
        assert!(core.actions.is_empty());
        assert_eq!(core.waiting_count(), 1);
        for (slot, key) in before.iter().enumerate() {
            let Some(key) = key else { continue };
            let session = core.sessions.iter().find(|s| s.key() == *key).unwrap();
            if matches!(session.activity.group(), Group::Working | Group::Waiting) {
                assert_eq!(core.keypad_slots()[slot].unwrap().key(), *key, "live slot {slot} moves");
                assert!(!session.acknowledged);
            } else if slot < SESSION_KEYS_PER_PAGE {
                assert!(matches!(&core.faces()[slot], KeyFace::Empty), "inactive current-page slot {slot} should be empty");
                assert!(session.acknowledged);
            } else {
                assert_eq!(core.keypad_slots()[slot].unwrap().key(), *key, "a session on another page must not be cleared");
                assert!(!session.acknowledged);
            }
        }
        let retained = core.sessions.iter().find(|s| s.id == "session-4").unwrap();
        assert_eq!(retained.title, title_before);
        assert_eq!(retained.updated, updated_before, "hub clear must not rewrite provider freshness");

        core.page = 1;
        let page_two = core.faces();
        assert!(matches!(&page_two[0], KeyFace::Session(face) if face.key == *before[8].as_ref().unwrap()));
        assert!(matches!(&page_two[1], KeyFace::Session(face) if face.key == *before[9].as_ref().unwrap()));
        core.key_down(HUB_KEY);
        finish_hub_hold(&mut core);
        assert!(matches!(&core.faces()[0], KeyFace::Empty), "inactive key on the selected later page clears");
        assert!(matches!(&core.faces()[1], KeyFace::Session(face) if face.key == *before[9].as_ref().unwrap()), "working key on the selected later page stays put");
        assert!(matches!(&core.keypad_slots()[0], Some(s) if s.key() == *before[0].as_ref().unwrap()), "clearing page two leaves page one intact");
    }

    #[test]
    fn hub_hold_uses_captured_id_not_replacement_slot_and_rechecks_activity() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        for i in 0..SESSION_KEYS_PER_PAGE { add_test_session(&mut core, &format!("session-{i}"), "idle"); }
        let before: Vec<_> = core.keypad_slots().iter().map(|s| s.map(|s| s.key())).collect();
        let replaced = before[0].as_ref().unwrap().clone();
        let displaced = core.sessions.iter_mut().find(|s| s.key() == replaced).unwrap();
        displaced.acknowledged = true;
        displaced.acknowledged_at = Some(now());
        core.key_down(HUB_KEY);

        add_test_session(&mut core, "new-live", "thinking");
        let after_insert = core.keypad_slots();
        assert_eq!(after_insert[0].unwrap().id, "new-live", "the captured old key is replaced while the press is held");
        assert!(after_insert.iter().enumerate().any(|(i, s)| i >= SESSION_KEYS_PER_PAGE && s.is_some_and(|s| s.key() == replaced)));
        finish_hub_hold(&mut core);
        assert!(core.visible().iter().any(|s| s.id == "new-live"), "a newly inserted replacement was not captured for clear");
        assert!(core.sessions.iter().any(|s| s.key() == replaced), "the captured inactive identity remains tracked");
        assert!(!core.visible().iter().any(|s| s.key() == replaced), "the captured inactive identity itself was cleared");

        let mut resumed = Core::new(Settings::default());
        resumed.in_control = true;
        add_test_session(&mut resumed, "becomes-live", "idle");
        add_test_session(&mut resumed, "other-live", "thinking");
        resumed.key_down(HUB_KEY);
        resumed.apply(Incoming { agent: "codex".into(), event: Some("UserPromptSubmit".into()), body: serde_json::json!({"session":"becomes-live", "state":"thinking"}) });
        finish_hub_hold(&mut resumed);
        assert!(resumed.visible().iter().any(|s| s.id == "becomes-live" && s.activity.group() == Group::Working));
        assert!(!resumed.sessions.iter().find(|s| s.id == "becomes-live").unwrap().acknowledged, "work that becomes active during the hold is not dismissed");
    }

    #[test]
    fn hub_hold_aborts_on_page_change_and_short_back_quota_or_cancel_never_clear() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        for i in 0..10 { add_test_session(&mut core, &format!("session-{i}"), "idle"); }
        core.keypad_slots();
        let before = core.visible().len();
        core.key_down(HUB_KEY);
        core.page_button(true, true);
        assert_eq!(core.page, 1);
        core.tick();
        core.key_up(HUB_KEY);
        core.page_button(true, false);
        assert_eq!(core.visible().len(), before, "navigation cancels a pending clear");
        assert!(core.actions.is_empty());

        core.key_down(HUB_KEY);
        core.key_up(HUB_KEY);
        assert_eq!(core.visible().len(), before, "a short hub press only follows its existing click action");
        assert!(matches!(core.actions.as_slice(), [Action::ShowWindow]));

        core.detail_key = Some(core.sessions[0].key());
        core.actions.clear();
        core.key_down(HUB_KEY);
        assert_eq!(core.key_target[HUB_KEY], Some(KeyPressTarget::Back));
        core.key_down[HUB_KEY] = Some(now() - DETAIL_HOLD_S - 0.01);
        core.tick();
        core.key_up(HUB_KEY);
        assert_eq!(core.visible().len(), before, "a Back target cannot clear the grid");

        core.quota_view = true;
        core.key_down(HUB_KEY);
        assert_eq!(core.key_target[HUB_KEY], None, "the quota hub is not a clear target");
        core.key_up(HUB_KEY);
        core.quota_view = false;
        core.detail_key = None;
        core.key_down(HUB_KEY);
        core.cancel_key(HUB_KEY);
        core.tick();
        assert_eq!(core.visible().len(), before, "a cancelled pointer hold cannot clear the grid");
    }

    #[test]
    fn dismissed_sessions_expire_but_stale_discovery_is_suppressed_and_fresh_work_returns() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        for id in ["stale", "active-again", "done-again"] { add_test_session(&mut core, id, "idle"); }
        core.key_down(HUB_KEY);
        finish_hub_hold(&mut core);
        let cleared_at = now();

        for session in &mut core.sessions {
            session.updated = cleared_at - REMOVE_AFTER_S - 1.0;
            session.acknowledged_at = Some(cleared_at - REMOVE_AFTER_S - 1.0);
        }
        core.tick();
        assert!(core.sessions.is_empty(), "full session state follows the normal retention horizon");

        let mut stale = Observation::new("codex", "stale", Source::LocalSnapshot, cleared_at + 1.0);
        stale.kind = ObservationKind::Discovery;
        stale.activity = Some(Activity::Idle);
        stale.recorded_at = Some(cleared_at - 1.0);
        stale.text_at = Some(cleared_at - 1.0);
        stale.unread = Some(4);
        core.observe(stale);
        assert!(core.sessions.is_empty(), "a stale snapshot/receipt does not recreate a cleared session after pruning");

        let mut active = Observation::new("codex", "active-again", Source::Hook, cleared_at + 2.0);
        active.activity = Some(Activity::Thinking);
        active.event = Some(ObservationEvent::TurnStarted);
        core.observe(active);
        assert!(core.visible().iter().any(|s| s.id == "active-again" && s.activity.group() == Group::Working));

        let mut completed = Observation::new("codex", "done-again", Source::LocalSnapshot, cleared_at + 3.0);
        completed.activity = Some(Activity::Done);
        completed.recorded_at = Some(cleared_at + 1.0);
        completed.text_at = Some(cleared_at + 1.0);
        core.observe(completed);
        let done = core.visible().into_iter().find(|s| s.id == "done-again").unwrap();
        assert_eq!(done.activity, Activity::Done, "a newly dated completion revives after clear and expiry");
        assert!(done.pending_attention);
        assert!(core.visible().iter().all(|s| !s.acknowledged));
    }

    #[test]
    fn hub_hold_tracks_alias_reconciliation_and_displaced_ack_resumes_after_an_unrelated_clear() {
        let mut alias = Core::new(Settings::default());
        alias.in_control = true;
        add_test_session(&mut alias, "host-id", "idle");
        alias.key_down(HUB_KEY);
        let mut observation = Observation::new("codex", "cli-id", Source::Other, now());
        observation.activity = Some(Activity::Idle);
        observation.aliases.push("host-id".into());
        alias.observe(observation);
        assert_eq!(alias.sessions.len(), 1);
        assert_eq!(alias.sessions[0].id, "cli-id");
        finish_hub_hold(&mut alias);
        assert!(alias.sessions.iter().any(|s| s.id == "cli-id"), "canonical tracked identity survives clear");
        assert!(alias.visible().is_empty(), "the remapped captured identity is cleared");

        let mut core = Core::new(Settings::default());
        core.in_control = true;
        for i in 0..SESSION_KEYS_PER_PAGE { add_test_session(&mut core, &format!("slot-{i}"), "idle"); }
        let original = core.keypad_slots();
        let target_key = original[0].unwrap().key();
        let target_id = original[0].unwrap().id.clone();
        let target_slot = 0;
        let target = core.sessions.iter_mut().find(|s| s.key() == target_key).unwrap();
        target.acknowledged = true;
        target.acknowledged_at = Some(now());
        core.keypad_slots();
        add_test_session(&mut core, "replacement", "thinking");
        assert_eq!(core.keypad_slots()[target_slot].unwrap().id, "replacement");
        assert!(core.keypad_slots().iter().enumerate().any(|(i, s)| i >= SESSION_KEYS_PER_PAGE && s.is_some_and(|s| s.id == target_id)));

        core.key_down(HUB_KEY);
        finish_hub_hold(&mut core);
        assert!(core.sessions.iter().any(|s| s.id == target_id), "the displaced acknowledged record stays tracked on a later page");
        core.apply(Incoming { agent: "codex".into(), event: Some("UserPromptSubmit".into()), body: serde_json::json!({"session":target_id, "state":"thinking"}) });
        let slots = core.keypad_slots();
        assert!(slots.iter().take(SESSION_KEYS_PER_PAGE).any(|s| s.is_some_and(|s| s.id == target_id)), "resumed work returns to page one after unrelated clear");
        assert_eq!(slots[target_slot].unwrap().id, "replacement", "resuming does not move the surviving bright neighbor");
    }

    #[test]
    fn long_hold_opens_captured_session_steps_without_opening_or_shelving_it() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "first", "thinking");
        add_test_session(&mut core, "second", "idle");
        let shown = core.faces();
        let target = target_for_face(&shown[0], core.quota_view);
        assert_eq!(target, KeyPressTarget::Session("codex:first".into()));
        core.key_down_with_target(0, target);

        // Activity updates do not displace the pressed session.
        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"second", "state":"waiting_question"}),
        });
        assert!(matches!(&core.faces()[0], KeyFace::Session(face) if face.key == "codex:first"));

        core.key_down[0] = Some(now() - DETAIL_HOLD_S - 0.1);
        core.tick();
        assert_eq!(core.detail_key.as_deref(), Some("codex:first"));
        assert!(!core.sessions.iter().find(|s| s.id == "first").unwrap().acknowledged);
        assert!(!core.sessions.iter().find(|s| s.id == "second").unwrap().acknowledged);
        core.key_up(0);
        assert!(core.actions.is_empty());
    }

    #[test]
    fn step_stream_deduplicates_updates_and_bounds_history() {
        let mut core = Core::new(Settings::default());
        let payload = || Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"steps", "state":"thinking", "_steps":[
                {"id":"turn-1", "kind":"Assistant", "status":"done", "title":"Answer", "text":"A stable answer"}
            ]}),
        };
        core.apply(payload());
        core.apply(payload());
        let session = core.sessions.iter().find(|s| s.id == "steps").unwrap();
        assert_eq!(session.steps.len(), 1, "repeated polling of the same provider step is a single tile");
        assert_eq!(session.steps[0].text, "A stable answer");

        let steps: Vec<_> = (0..128).map(|i| serde_json::json!({"id":format!("step-{i}"), "kind":"Tool", "title":format!("Tool {i}")})).collect();
        core.apply(Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"steps", "state":"thinking", "_steps":steps}),
        });
        let session = core.sessions.iter().find(|s| s.id == "steps").unwrap();
        assert_eq!(session.steps.len(), 128, "step history has a hard cap");
        assert_eq!(session.steps.first().unwrap().signature, "provider:step-0", "the bounded history retains its oldest item when exactly at the cap");
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"steps", "state":"thinking", "_steps":[{"id":"step-128", "kind":"Tool"}]}) });
        let session = core.sessions.iter().find(|s| s.id == "steps").unwrap();
        assert_eq!(session.steps.len(), 128);
        assert_eq!(session.steps.first().unwrap().signature, "provider:step-1", "the oldest step is evicted when a new step exceeds the cap");
    }

    #[test]
    fn detail_paginates_eight_steps_and_back_returns_to_the_opening_page() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_test_session(&mut core, "many", "thinking");
        let steps: Vec<_> = (0..10).map(|i| serde_json::json!({"id":format!("s{i}"), "kind":"Update", "title":format!("Step {i}")})).collect();
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"many", "state":"thinking", "_steps":steps}) });
        core.page = 1;
        core.open_session_steps("codex:many");
        let faces = core.faces();
        assert!(matches!(&faces[0], KeyFace::Step(step) if step.title == "Step 0"));
        assert!(matches!(&faces[7], KeyFace::Step(step) if step.title == "Step 7"));
        assert!(matches!(&faces[HUB_KEY], KeyFace::Back));
        core.page_button(true, true);
        core.page_button(true, false);
        let faces = core.faces();
        assert!(matches!(&faces[0], KeyFace::Step(step) if step.title == "Step 8"));
        assert!(matches!(&faces[1], KeyFace::Step(step) if step.title == "Step 9"));
        core.key_down(HUB_KEY);
        core.key_up(HUB_KEY);
        assert_eq!(core.detail_key, None);
        assert_eq!(core.page, 1, "Back restores the prior session page");
    }

    #[test]
    fn keypad_slots_keep_holes_and_reuse_them_without_moving_neighbours() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        for i in 0..(SESSION_KEYS_PER_PAGE + 3) {
            add_test_session(&mut core, &format!("session-{i}"), "idle");
        }
        let before: Vec<_> = core.keypad_slots().iter().map(|s| s.map(|s| s.key())).collect();
        let removed = before[1].clone().unwrap();
        let removed_id = removed.strip_prefix("codex:").unwrap();
        core.key_down(1);
        core.acknowledge_open("codex", removed_id);
        let after: Vec<_> = core.keypad_slots().iter().map(|s| s.map(|s| s.key())).collect();
        assert_eq!(after[1].as_deref(), Some(removed.as_str()), "acknowledgement keeps the key in place until another active session needs it");
        let face = &core.faces()[1];
        assert!(matches!(face, KeyFace::Session(face) if face.key == removed && face.opacity == ACKNOWLEDGED_OPACITY));
        for i in 0..before.len() {
            if i != 1 { assert_eq!(after[i], before[i], "slot {i} moved"); }
        }
        add_test_session(&mut core, "new", "thinking");
        assert_eq!(core.keypad_slots()[1].unwrap().id, "new");
        assert!(core.keypad_slots().iter().enumerate().any(|(i, s)| i >= SESSION_KEYS_PER_PAGE && s.is_some_and(|session| session.key() == removed)), "the displaced acknowledged tile remains retrievable later");
        core.key_up(1);
        assert!(matches!(core.actions.as_slice(), [Action::Open { agent, session }] if agent == "codex" && session == removed_id), "a key held across acknowledged-slot replacement still opens its captured target");

        // A resumed session takes the next ordinary hole, leaving the others fixed.
        let another = before[3].clone().unwrap();
        core.acknowledge_open("codex", another.strip_prefix("codex:").unwrap());
        let _ = core.keypad_slots();
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session": removed_id, "state": "thinking"}) });
        let resumed = core.keypad_slots();
        assert_eq!(resumed[3].unwrap().key(), removed);
        assert!(matches!(&core.faces()[3], KeyFace::Session(face) if face.key == removed && face.opacity == 1.0));
        assert_eq!(resumed[SESSION_KEYS_PER_PAGE].unwrap().key(), before[SESSION_KEYS_PER_PAGE].clone().unwrap());
    }

    #[test]
    fn acknowledgements_stay_put_and_resume_restores_full_brightness() {
        let mut core = Core::new(Settings::default());
        for id in ["a", "b", "c"] { add_test_session(&mut core, id, "idle"); }
        let initial: Vec<_> = core.keypad_slots().iter().map(|s| s.unwrap().id.clone()).collect();
        let a = &initial[0];
        let b = &initial[1];
        let c = &initial[2];
        core.acknowledge_open("codex", a);
        assert_eq!(core.keypad_slots()[0].unwrap().id, *a);
        core.acknowledge_open("codex", b);
        assert_eq!(core.keypad_slots()[1].unwrap().id, *b);
        core.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session": a, "state": "thinking"}) });
        assert_eq!(core.keypad_slots()[0].unwrap().id, *a);
        assert!(matches!(&core.faces()[0], KeyFace::Session(face) if face.key == format!("codex:{a}") && face.opacity == 1.0));
        core.acknowledge_open("codex", c);
        let slots = core.keypad_slots();
        let b_pos = slots.iter().position(|s| s.is_some_and(|s| s.id == *b)).unwrap();
        let c_pos = slots.iter().position(|s| s.is_some_and(|s| s.id == *c)).unwrap();
        assert_eq!((b_pos, c_pos), (1, 2));
        assert!(matches!(&core.faces()[1], KeyFace::Session(face) if face.opacity == ACKNOWLEDGED_OPACITY));
        assert!(matches!(&core.faces()[2], KeyFace::Session(face) if face.opacity == ACKNOWLEDGED_OPACITY));
    }

    #[test]
    fn old_idle_moves_out_of_first_page_without_compacting_others() {
        let mut settings = Settings::default();
        settings.recency_minutes = 240;
        let mut core = Core::new(settings);
        for id in ["one", "two", "three"] { add_test_session(&mut core, id, "idle"); }
        let before: Vec<_> = core.keypad_slots().iter().map(|s| s.map(|s| s.key())).collect();
        let key = before[1].as_ref().unwrap();
        core.sessions.iter_mut().find(|s| s.key() == *key).unwrap().since = now() - IDLE_SECOND_PAGE_AFTER_S - 1.0;
        let after = core.keypad_slots();
        assert!(after[1].is_none());
        assert_eq!(after[0].unwrap().key(), before[0].clone().unwrap());
        assert_eq!(after[2].unwrap().key(), before[2].clone().unwrap());
        assert_eq!(after[SESSION_KEYS_PER_PAGE].unwrap().key(), *key);
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
        assert_eq!(slots[0].unwrap().id, "local-old", "unread inactive work stays in the recent first-page slots");
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
    fn acknowledged_session_stays_visible_dimmed_and_brightens_when_work_resumes() {
        let mut c = Core::new(Settings::default());
        c.in_control = true;
        c.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"a","state":"idle","title":"A"}) });
        assert_eq!(c.visible().len(), 1);
        let key = c.visible()[0].key();
        c.sessions[0].acknowledged = true;
        assert_eq!(c.visible().len(), 1);
        assert!(matches!(&c.faces()[0], KeyFace::Session(face) if face.key == key && face.opacity == ACKNOWLEDGED_OPACITY));
        std::thread::sleep(std::time::Duration::from_millis(5));
        c.apply(Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session":"a","state":"thinking"}) });
        assert_eq!(c.visible()[0].key(), key);
        assert!(matches!(&c.faces()[0], KeyFace::Session(face) if face.key == key && face.opacity == 1.0));
    }
}
