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
pub const COLUMNS: usize = 3;
pub const KEY_SIZE: u32 = 118;
pub const ORIGIN_X: u32 = 23;
pub const ORIGIN_Y: u32 = 6;
pub const PITCH: u32 = 158;
pub const HUB_KEY: usize = 8;
pub const SESSION_KEYS_PER_PAGE: usize = 8;
pub const FOCUS_HOLD_S: f64 = 0.6;
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
            WaitingQuestion => "Waiting for your answer",
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

pub const AGENT_FORMAT: &str = "legacy-rs.agent";
pub const AGENTS_FORMAT: &str = "legacy-rs.agents";

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
    let mut claude_app = mk("claude-desktop", "Claude", "CL", 95.0, "Claude", "Active and recent conversations (and Claude Code sessions) in the Claude desktop app.");
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
        notes.push("Ignored the link: AgentPad does not open this kind of link.".into());
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
                    return Err("This file is not an AgentPad agent document.".to_string());
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
            format: "legacy-rs.settings".into(),
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
    let dir = base.join("AgentPad");
    // Keep settings and imported agents from before the rename.
    let old = base.join("legacy RS");
    if !dir.exists() && old.exists() {
        let _ = std::fs::rename(&old, &dir);
    }
    dir
}

impl Settings {
    pub fn load() -> Settings {
        std::fs::read_to_string(data_dir().join("settings.json"))
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
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

pub fn load_custom_agents() -> Vec<AgentDef> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(data_dir().join("agents")) {
        for e in rd.flatten() {
            if let Ok(t) = std::fs::read_to_string(e.path()) {
                if let Ok(list) = parse_agent_file(&t) {
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
    FocusText { text: String, colour: Rgb },
    Back,
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
    pub focus_header: bool,
    pub dots: bool,
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
    pub focus: Option<String>,
    pub in_control: bool,
    pub hardware: Hardware,
    key_down: [Option<f64>; KEY_COUNT],
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
            focus: None,
            in_control,
            hardware: Hardware::Off,
            key_down: [None; KEY_COUNT],
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
                });
                self.sessions.len() - 1
            }
        };
        let s = &mut self.sessions[idx];
        let old = s.activity;
        if let Some(v) = str_at(b, &["_app"]) {
            s.app_bundle = v.to_string();
        }
        if let Some(v) = str_at(b, &["_tty"]) {
            let v = v.trim();
            s.tty = if v.starts_with("/dev/") { v.to_string() } else { format!("/dev/{v}") };
        }
        if let Some(v) = str_at(b, &["_term"]) {
            s.term = v.to_string();
        }
        if let Some(title) = title {
            s.title = title;
        }
        if let Some(text) = text {
            if text != s.text {
                s.text = text;
                s.text_at = t;
            }
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
            s.since = t;
        }
        let og = old.group();
        let ng = activity.group();
        if og != Group::Working && og != Group::Waiting && ng == Group::Working {
            s.turn_started = t;
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
        self.sessions.retain(|s| t - s.updated < REMOVE_AFTER_S);
        if self.sessions.len() != before {
            changed = true;
        }
        if changed {
            self.resort();
            self.touch();
        }
        // Hold a session key for 0.6 s → focus view.
        for i in 0..KEY_COUNT {
            if let Some(d) = self.key_down[i] {
                if !self.hold_fired[i] && t - d >= FOCUS_HOLD_S && i != HUB_KEY && self.focus.is_none() {
                    self.hold_fired[i] = true;
                    if let Some(s) = self.session_at(i) {
                        self.focus = Some(s.key());
                        self.touch();
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
        let recent = |s: &&Session| matches!(s.activity.group(), Group::Working | Group::Waiting) || t - s.updated < recency;
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

    pub fn pages(&self) -> usize {
        self.visible().len().div_ceil(SESSION_KEYS_PER_PAGE).max(1)
    }

    fn session_at(&self, key: usize) -> Option<&Session> {
        if key >= SESSION_KEYS_PER_PAGE {
            return None;
        }
        let page = self.page.min(self.pages() - 1);
        self.visible().get(page * SESSION_KEYS_PER_PAGE + key).copied()
    }

    pub fn waiting_count(&self) -> usize {
        self.visible().iter().filter(|s| s.activity.group() == Group::Waiting).count()
    }

    /// After this long with every session idle, the keypad dims and redraws at 10 fps.
    pub fn dimmed(&self) -> bool {
        let quiet = self.visible().iter().all(|s| !matches!(s.activity.group(), Group::Working | Group::Waiting));
        quiet && now() - self.last_change > self.settings.idle_dim_minutes as f64 * 60.0
    }

    pub fn fps(&self) -> u32 {
        if self.dimmed() { 10 } else { self.settings.target_fps.clamp(10, 60) }
    }

    // ---- colours

    pub fn waiting_colour(&self) -> Rgb {
        let orb = colour::hex_u32(colour::reference_ramp("waitingPermission")[2]);
        colour::rotate(orb, self.settings.waiting_hue - colour::REFERENCE_WAITING_HUE)
    }

    pub fn error_colour() -> Rgb {
        colour::oklch(0.467, 0.154, colour::ERROR_HUE)
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

    // ---- keypad faces

    pub fn faces(&self) -> [KeyFace; KEY_COUNT] {
        let mut out: [KeyFace; KEY_COUNT] = std::array::from_fn(|_| KeyFace::Empty);
        if !self.in_control {
            for f in out.iter_mut() {
                *f = KeyFace::Released;
            }
            return out;
        }
        let t = now();
        let speed = self.settings.text_speed.max(1.0) as f64;
        let face_of = |s: &Session, header: bool| {
            let (orb, glow) = self.colours(s);
            let shown = ((t - s.text_at) * speed).max(0.0) as usize;
            let text: String = s.text.chars().take(shown).collect();
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
                focus_header: header,
                dots: def.map(|d| d.style == "dots").unwrap_or(false),
            }
        };
        if let Some(fk) = &self.focus {
            if let Some(s) = self.sessions.iter().find(|s| &s.key() == fk) {
                out[0] = KeyFace::Session(face_of(s, true));
                let (orb, _) = self.colours(s);
                let shown = ((t - s.text_at) * speed).max(0.0) as usize;
                let typed: String = s.text.chars().take(shown).collect();
                let chunks = wrap_chunks(&typed, 7, 34);
                for (i, chunk) in chunks.into_iter().enumerate() {
                    out[1 + i] = KeyFace::FocusText { text: chunk, colour: orb };
                }
                out[HUB_KEY] = KeyFace::Back;
                return out;
            }
        }
        let vis = self.visible();
        let page = self.page.min(self.pages() - 1);
        for (i, s) in vis.iter().skip(page * SESSION_KEYS_PER_PAGE).take(SESSION_KEYS_PER_PAGE).enumerate() {
            out[i] = KeyFace::Session(face_of(s, false));
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
        });
        out
    }

    // ---- input (on-screen keypad, keyboard, physical keypad)

    pub fn key_down(&mut self, i: usize) {
        if i < KEY_COUNT && self.key_down[i].is_none() {
            self.key_down[i] = Some(now());
            self.hold_fired[i] = false;
        }
    }

    pub fn key_up(&mut self, i: usize) {
        if i >= KEY_COUNT {
            return;
        }
        let was_down = self.key_down[i].take().is_some();
        if !was_down || self.hold_fired[i] {
            return;
        }
        if !self.in_control {
            return;
        }
        if self.focus.is_some() {
            if i == HUB_KEY {
                self.focus = None;
                self.touch();
            } else if let Some(fk) = self.focus.clone() {
                if let Some(s) = self.sessions.iter().find(|s| s.key() == fk) {
                    self.actions.push(Action::Open { agent: s.agent.clone(), session: s.id.clone() });
                }
            }
            return;
        }
        if i == HUB_KEY {
            // Jump to the first page with a waiting session.
            let vis = self.visible();
            if let Some(pos) = vis.iter().position(|s| s.activity.group() == Group::Waiting) {
                self.page = pos / SESSION_KEYS_PER_PAGE;
            } else {
                self.actions.push(Action::ShowWindow);
            }
            self.touch();
            return;
        }
        if !self.settings.key_actions_enabled {
            return;
        }
        match self.session_at(i) {
            Some(s) => {
                let a = Action::Open { agent: s.agent.clone(), session: s.id.clone() };
                self.actions.push(a);
            }
            None => {
                if !self.settings.empty_key_url.trim().is_empty() {
                    self.actions.push(Action::OpenUrl(self.settings.empty_key_url.clone()));
                }
            }
        }
    }

    pub fn is_key_down(&self, i: usize) -> bool {
        self.key_down.get(i).map(|d| d.is_some()).unwrap_or(false)
    }

    pub fn hold_progress(&self, i: usize) -> f32 {
        match self.key_down.get(i).copied().flatten() {
            Some(d) if !self.hold_fired[i] => ((now() - d) / FOCUS_HOLD_S).clamp(0.0, 1.0) as f32,
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
        if let Some(fk) = self.focus.clone() {
            // In focus view the page buttons move to the next session.
            let keys: Vec<String> = self.visible().iter().map(|s| s.key()).collect();
            if let Some(pos) = keys.iter().position(|k| *k == fk) {
                let n = keys.len();
                let np = if next { (pos + 1) % n } else { (pos + n - 1) % n };
                self.focus = Some(keys[np].clone());
            }
        } else {
            let pages = self.pages();
            self.page = if next { (self.page + 1) % pages } else { (self.page + pages - 1) % pages };
        }
        self.touch();
    }

    pub fn take_over(&mut self) {
        if !self.in_control {
            self.in_control = true;
            self.notify("AgentPad has the keypad. Each key is one session in its agent's colour; the hub key counts what is waiting.");
        }
    }

    pub fn release(&mut self) {
        if self.in_control {
            self.in_control = false;
            self.focus = None;
            self.notify("The keypad is back with Logi Options+. The menu bar keeps working.");
        }
    }

    pub fn control_label(&self) -> &'static str {
        match (&self.hardware, self.in_control) {
            (Hardware::Driving, true) => "AgentPad has the keypad",
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

/// Splits text into up to `keys` chunks of about `per_key` characters on word boundaries.
pub fn wrap_chunks(text: &str, keys: usize, per_key: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in text.split_whitespace() {
        if !cur.is_empty() && cur.chars().count() + 1 + word.chars().count() > per_key {
            out.push(std::mem::take(&mut cur));
            if out.len() == keys {
                break;
            }
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(word);
    }
    if !cur.is_empty() && out.len() < keys {
        out.push(cur);
    }
    out
}
