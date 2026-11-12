//! Padgent — an open-source keypad and menu bar for agent sessions.

mod colour;
mod field;
mod hardware;
mod local;
mod icon;
mod integrations;
mod mac;
mod model;
mod paint;
mod quota;
mod server;
mod tray;
mod watch;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Rect, RichText, Sense, Stroke, Ui, UiBuilder, pos2,
    vec2,
};
use model::{
    Action, AgentDef, ColourKeysBy, Core, Group, Hardware, KEY_COUNT, MenuIndicator, Motion, ReorderOn, Settings,
};
use paint::{c32, white, with_alpha};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Keypad,
    Agents,
    Look,
    Keys,
    MenuBar,
    About,
}

const PAGES: [(Page, &str, [u8; 3]); 6] = [
    (Page::Keypad, "Keypad", [255, 159, 10]),
    (Page::Agents, "Agents", [94, 92, 230]),
    (Page::Look, "Look", [255, 55, 95]),
    (Page::Keys, "Keys and sessions", [48, 209, 88]),
    (Page::MenuBar, "Menu bar and hand-over", [10, 132, 255]),
    (Page::About, "About", [142, 142, 147]),
];

/// Key travel when sessions reorder: 0.8 s, ease-in-out cubic (Travel.position 0x1005058e4).
const TRAVEL_S: f64 = 0.8;

fn travel_pos(tr: ((f32, f32), (f32, f32), f64), now: f64) -> (f32, f32) {
    let ((fx, fy), (tx, ty), start) = tr;
    let p = ((now - start) / TRAVEL_S).clamp(0.0, 1.0) as f32;
    let e = if p < 0.5 { 4.0 * p * p * p } else { 1.0 - (2.0 - 2.0 * p).powi(3) / 2.0 };
    (fx + (tx - fx) * e, fy + (ty - fy) * e)
}

struct IslandAgent {
    agent: String,
    session: String,
    name: String,
    title: String,
    activity: model::Activity,
    tool: Option<model::ToolKind>,
    since: f64,
    orb: colour::Rgb,
    glow: colour::Rgb,
}

struct IslandQuota { provider: String, session: Option<f64>, weekly: Option<f64>, colour: colour::Rgb }

enum IslandAction { None, Expand, Quotas, Open(String, String) }

fn island_quotas(c: &Core) -> Vec<IslandQuota> {
    c.quota_rows().into_iter().filter_map(|row| {
        let provider = row.first()?.provider.clone();
        let value = |window: &str| row.iter().find(|q| q.window == window)
            .and_then(|q| quota::displayed_remaining(q, model::now() as i64))
            .filter(|v| v.is_finite() && (0.0..=100.0).contains(v));
        let (session, weekly) = (value("Session"), value("Weekly"));
        let agent = if provider.eq_ignore_ascii_case("Claude") { "claude-code" } else if provider.eq_ignore_ascii_case("Codex") { "codex" } else { provider.as_str() };
        let colour = colour::rotate(colour::hex_u32(0xd9a441), c.hue_of(agent) - colour::REFERENCE_HUE);
        (session.is_some() || weekly.is_some()).then_some(IslandQuota { provider, session, weekly, colour })
    }).collect()
}

fn island_agents(c: &Core) -> Vec<IslandAgent> {
    let mut sessions: Vec<_> = c.visible().into_iter()
        .filter(|s| matches!(s.activity.group(), Group::Working | Group::Waiting | Group::Done) || s.pending_attention || s.unread > 0).collect();
    sessions.sort_by_key(|s| if s.activity.group() == Group::Waiting { 0 } else if s.pending_attention || s.unread > 0 || s.activity.group() == Group::Done { 1 } else { 2 });
    sessions.into_iter().map(|s| {
        let name = c.agents.iter().find(|a| a.id == s.agent).map(|a| a.name.clone()).unwrap_or_else(|| s.agent.clone());
        let (orb, glow) = c.colours(s);
        IslandAgent { agent: s.agent.clone(), session: s.id.clone(), name, title: s.title.clone(),
            activity: s.activity, tool: s.tool, since: s.since, orb, glow }
    }).collect()
}

struct App {
    travels: std::collections::HashMap<String, ((f32, f32), (f32, f32), f64)>,
    core: Arc<Mutex<Core>>,
    page: Page,
    tray: Option<tray::Tray>,
    menu_events: Arc<Mutex<Vec<String>>>,
    tray_clicks: Arc<Mutex<Vec<tray_icon::Rect>>>,
    popup_open: bool,
    popup_from_island: bool,
    island_geometry: Option<(f32, f32, f32, f32)>,
    island_progress: f32,
    popup_position: egui::Pos2,
    popup_focused: bool,
    popup_keys: [bool; KEY_COUNT],
    popup_pages: [bool; 2],
    screenshot: Option<String>,
    screenshot_menu_bar: bool,
    screenshot_island: bool,
    screenshot_island_expanded: bool,
    frames: u32,
    shot_done: Arc<AtomicBool>,
    shot_failed: Arc<AtomicBool>,
    shot_requested: bool,
    keys_prev: [bool; KEY_COUNT],
    pages_prev: [bool; 2],
    selected_agent: String,
    agent_notes: Vec<String>,
    new_agent: Option<AgentDef>,
    settings_rev: u64,
    saved_rev: u64,
}

fn load_system_font() -> Option<Vec<u8>> {
    let bytes = std::fs::read("/System/Library/Fonts/SFNS.ttf").ok()?;
    ab_glyph::FontRef::try_from_slice(&bytes).ok()?;
    Some(bytes)
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    if let Some(bytes) = load_system_font() {
        fonts.font_data.insert("sf".into(), Arc::new(egui::FontData::from_owned(bytes)));
        if let Some(f) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            f.insert(0, "sf".into());
        }
    }
    let mut mono = vec![];
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/SFNSMono.ttf") {
        fonts.font_data.insert("sfmono".into(), Arc::new(egui::FontData::from_owned(bytes)));
        mono.push("sfmono".to_string());
    }
    let mut bold = vec![];
    if let Ok(bytes) = std::fs::read("/System/Library/Fonts/Menlo.ttc") {
        // Menlo.ttc holds Regular, Bold, Italic, Bold Italic: face 1 is Bold.
        let mut fd = egui::FontData::from_owned(bytes);
        fd.index = 1;
        fonts.font_data.insert("menlobold".into(), Arc::new(fd));
        bold.push("menlobold".to_string());
    }
    let fallback: Vec<String> = fonts.families.get(&egui::FontFamily::Monospace).cloned().unwrap_or_default();
    mono.extend(fallback.iter().cloned());
    bold.extend(mono.iter().cloned());
    fonts.families.insert(egui::FontFamily::Name("mono".into()), mono);
    fonts.families.insert(egui::FontFamily::Name("monobold".into()), bold);
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::Theme::Dark);
    ctx.set_visuals(egui::Visuals::dark());
    ctx.all_styles_mut(|s| {
        let v = &mut s.visuals;
        v.panel_fill = Color32::TRANSPARENT;
        v.window_fill = Color32::from_rgba_unmultiplied(30, 32, 40, 235);
        v.extreme_bg_color = Color32::from_rgba_unmultiplied(0, 0, 0, 70);
        v.selection.bg_fill = Color32::from_rgb(10, 132, 255);
        v.hyperlink_color = Color32::from_rgb(100, 170, 255);
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.open] {
            w.corner_radius = CornerRadius::same(9);
        }
        v.widgets.inactive.weak_bg_fill = white(0.08);
        v.widgets.inactive.bg_fill = white(0.08);
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, white(0.1));
        v.widgets.hovered.weak_bg_fill = white(0.15);
        v.widgets.hovered.bg_fill = white(0.15);
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, white(0.22));
        v.widgets.active.weak_bg_fill = white(0.22);
        v.widgets.active.bg_fill = white(0.22);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, white(0.08));
        s.spacing.item_spacing = vec2(10.0, 9.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 26.0;
        s.text_styles.insert(egui::TextStyle::Body, FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Button, FontId::proportional(14.0));
        s.text_styles.insert(egui::TextStyle::Heading, FontId::proportional(24.0));
        s.text_styles.insert(egui::TextStyle::Small, FontId::proportional(12.0));
    });
}

fn card<R>(ui: &mut Ui, title: &str, body: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(white(0.055))
        .stroke(Stroke::new(1.0, white(0.11)))
        .corner_radius(CornerRadius::same(18))
        .inner_margin(egui::Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if !title.is_empty() {
                ui.label(RichText::new(title).size(15.0).strong().color(white(0.95)));
                ui.add_space(4.0);
            }
            body(ui)
        })
        .inner
}

fn hint(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).size(12.5).color(white(0.55)));
}

impl App {
    fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<String>, screenshot_menu_bar: bool, screenshot_island: bool, screenshot_island_expanded: bool, shot_failed: Arc<AtomicBool>) -> App {
        setup_style(&cc.egui_ctx);
        let mut settings = Settings::load();
        let demo = screenshot.is_some();
        if demo {
            settings.demo_mode = true;
            settings.drive_physical_keypad = false;
        }
        let mut core = Core::new(settings);
        if core.settings.demo_mode {
            core.load_demo();
        }
        if demo {
            core.in_control = true;
        }
        if screenshot_island {
            for (provider, window, remaining) in [("Codex", "Weekly", 72.0), ("Claude", "Session", 55.0), ("Claude", "Weekly", 40.0)] {
                core.quotas.push(quota::Quota { provider: provider.into(), window: window.into(), remaining: Some(remaining),
                    reset: None, reset_text: None, updated: Some(quota::observed_now()), source: "Demo".into() });
            }
        }
        let core = Arc::new(Mutex::new(core));
        let ctx = cc.egui_ctx.clone();
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || ctx.request_repaint());
        let menu_events = Arc::new(Mutex::new(Vec::new()));
        let tray_clicks = Arc::new(Mutex::new(Vec::new()));
        let mut tray = None;
        if !demo {
            {
                use raw_window_handle::HasWindowHandle;
                if let Ok(h) = cc.window_handle() {
                    mac::glass_behind(h.as_raw());
                }
            }
            mac::set_menu_bar_app();
            let port = std::env::var("AGENTPAD_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or_else(|| core.lock().map(|c| c.settings.hook_port).unwrap_or(47800));
            server::start(core.clone(), port, wake.clone());
            integrations::upgrade_claude_hooks(port);
            if std::env::var("AGENTPAD_NO_KEYPAD").is_err() {
                hardware::start(core.clone(), wake.clone());
            }
            watch::start(core.clone(), wake.clone());
            local::start(core.clone(), wake.clone());
            quota::start(core.clone(), wake.clone());
            tray = tray::Tray::new();
            let clicks = tray_clicks.clone();
            let wake_tray = wake.clone();
            tray_icon::TrayIconEvent::set_event_handler(Some(move |event| {
                if let tray_icon::TrayIconEvent::Click { rect, button: tray_icon::MouseButton::Left, button_state: tray_icon::MouseButtonState::Up, .. } = event {
                    if let Ok(mut pending) = clicks.lock() { pending.push(rect); }
                    wake_tray();
                }
            }));
            // Repair a stale Accessibility entry left by an earlier build, once per signature.
            let rc = core.clone();
            std::thread::spawn(move || {
                let (in_use, last) = match rc.lock() {
                    Ok(c) => (!c.watched_apps().is_empty(), c.settings.accessibility_recovery_requirement.clone()),
                    Err(_) => return,
                };
                if let Some((req, ok)) = watch::recover_stale_accessibility(in_use, &last) {
                    if let Ok(mut c) = rc.lock() {
                        c.settings.accessibility_recovery_requirement = req;
                        c.accessibility_reset_failed = !ok;
                        c.settings.save();
                    }
                }
            });
            let events = menu_events.clone();
            let w2 = wake.clone();
            let mc = core.clone();
            tray_icon::menu::MenuEvent::set_event_handler(Some(move |e: tray_icon::menu::MenuEvent| {
                let id = e.id().0.clone();
                mac::log(&format!("menu: {id}"));
                if id == "show" || id == "settings" {
                    mac::show_app();
                }
                if let Some(key) = id.strip_prefix("open:") {
                    // Open now: this runs on the main thread even while the window is hidden.
                    let found = mc.lock().ok().and_then(|mut c| {
                        let (agent, session) = key.split_once(':')?;
                        let s = c.acknowledge_open(agent, session)?;
                        Some((c.agents.iter().find(|a| a.id == s.agent).cloned(), s))
                    });
                    match found {
                        Some((def, s)) => integrations::open_exact(def.as_ref(), &s),
                        None => mac::log(&format!("menu: no session for {key}")),
                    }
                    w2();
                    return;
                }
                if id == "toggle" {
                    if let Ok(mut c) = mc.lock() {
                        if c.in_control { c.release() } else { c.take_over() }
                    }
                    w2();
                    return;
                }
                if id == "quit" {
                    hardware::shutdown(&mc);
                    if let Ok(c) = mc.lock() {
                        c.settings.save();
                    }
                    std::process::exit(0);
                }
                if let Ok(mut v) = events.lock() {
                    v.push(id);
                }
                w2();
            }));
        }
        let start_page = std::env::args()
            .skip_while(|a| a != "--page")
            .nth(1)
            .and_then(|p| PAGES.iter().find(|(_, n, _)| n.to_lowercase().starts_with(&p.to_lowercase())).map(|(pg, _, _)| *pg))
            .unwrap_or(Page::Keypad);
        App {
            travels: Default::default(),
            core,
            page: start_page,
            tray,
            menu_events,
            tray_clicks,
            popup_open: screenshot_menu_bar || screenshot_island_expanded,
            popup_from_island: screenshot_island_expanded,
            island_geometry: None,
            island_progress: 0.0,
            popup_position: pos2(0.0, 24.0),
            popup_focused: false,
            popup_keys: [false; KEY_COUNT],
            popup_pages: [false; 2],
            screenshot,
            screenshot_menu_bar,
            screenshot_island,
            screenshot_island_expanded,
            frames: 0,
            shot_done: Arc::new(AtomicBool::new(false)),
            shot_failed,
            shot_requested: false,
            keys_prev: [false; KEY_COUNT],
            pages_prev: [false; 2],
            selected_agent: "claude-code".into(),
            agent_notes: Vec::new(),
            new_agent: None,
            settings_rev: 0,
            saved_rev: 0,
        }
    }

    fn run_actions(&mut self, ctx: &egui::Context) {
        let (actions, agents) = match self.core.lock() {
            Ok(mut c) => (std::mem::take(&mut c.actions), c.agents.clone()),
            Err(_) => return,
        };
        for a in actions {
            match a {
                Action::Open { agent, session } => {
                    mac::log(&format!("click: open {agent}:{session}"));
                    self.open_target(&agents, &agent, &session)
                }
                Action::OpenUrl(u) => integrations::open_url(&u),
                Action::ShowWindow => {
                    mac::show_app();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            }
        }
        let events: Vec<String> = self.menu_events.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        for id in events {
            match id.as_str() {
                "show" => {
                    self.page = Page::Keypad;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                "settings" => {
                    self.page = Page::Agents;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
                _ => {}
            }
        }
    }

    fn open_exact(&self, agents: &[AgentDef], agent: &str, session: &str) {
        let found = self.core.lock().ok().and_then(|mut c| c.acknowledge_open(agent, session));
        let def = agents.iter().find(|d| d.id == agent);
        match found {
            Some(s) => integrations::open_exact(def, &s),
            None => integrations::open_session(def, session),
        }
    }

    // Hardware and keypad presses already acknowledged their captured target on release.
    fn open_target(&self, agents: &[AgentDef], agent: &str, session: &str) {
        let found = self.core.lock().ok().and_then(|c| c.sessions.iter().find(|s| s.agent == agent && s.id == session).cloned());
        let def = agents.iter().find(|d| d.id == agent);
        match found {
            Some(s) => integrations::open_exact(def, &s),
            None => integrations::open_session(def, session),
        }
    }

    fn open_popup(&mut self, rect: tray_icon::Rect) {
        self.popup_open = true;
        self.popup_from_island = false;
        let (x, y) = mac::tray_popup_position(rect, 400.0, 480.0);
        self.popup_position = pos2(x, y);
        self.popup_focused = false;
        mac::log("tray keypad opened");
        mac::show_app();
    }

    fn island(&mut self, ctx: &egui::Context) {
        let (enabled, active, quotas, brightness, reduced) = match self.core.lock() {
            Ok(c) => (c.settings.island_enabled,
                island_agents(&c), island_quotas(&c),
                c.settings.brightness, c.settings.motion == Motion::Reduced),
            Err(_) => return,
        };
        if !enabled && self.popup_from_island { self.popup_open = false; }
        if !enabled || (active.is_empty() && quotas.is_empty() && !(self.popup_open && self.popup_from_island) && self.island_progress <= 0.0) {
            self.island_geometry = None;
            return;
        }
        let geometry = mac::island_geometry();
        let (x, y, notch, height) = geometry;
        let h = height.clamp(28.0, 40.0);
        let left_width = 6.0 + active.len().min(2) as f32 * 76.0;
        let quota_count: usize = quotas.iter().take(2).map(|q| usize::from(q.session.is_some()) + usize::from(q.weekly.is_some())).sum();
        let right_width = 6.0 + quota_count as f32 * 42.0;
        let width = (notch + 2.0 * (left_width.max(right_width) + 6.0)).max(400.0);
        let expanded = self.popup_open && self.popup_from_island;
        let body_width = notch + left_width + right_width + 12.0;
        let window_h = h + if expanded || self.island_progress > 0.0 { body_width * 1.2 + 4.0 } else { 4.0 };
        let island_x = x - (width - 400.0) / 2.0;
        let builder = egui::ViewportBuilder::default().with_title("Padgent Island")
            .with_inner_size([width, window_h]).with_position(pos2(island_x, y))
            .with_transparent(true).with_decorations(false).with_resizable(false)
            .with_active(false).with_always_on_top();
        let changed = self.island_geometry != Some(geometry);
        self.island_geometry = Some(geometry);
        let mut action = IslandAction::None;
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("agent-island"), builder, |ui, _| {
            if changed {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos2(island_x, y)));
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(width, window_h)));
            }
            let progress = if reduced { if expanded { 1.0 } else { 0.0 } } else {
                ui.ctx().animate_bool_with_time(egui::Id::unique("island-body"), expanded, 0.22)
            };
            self.island_progress = progress;
            let (next_action, pill) = Self::island_contents(ui, &active, &quotas, brightness, reduced, notch, h, progress > 0.0);
            action = next_action;
            self.island_body(ui, ctx, pill, h, progress);
        });
        mac::configure_island_window();
        if let IslandAction::Open(agent, session) = &action {
            let agents = self.core.lock().map(|c| c.agents.clone()).unwrap_or_default();
            self.open_exact(&agents, agent, session);
        }
        if matches!(action, IslandAction::Expand | IslandAction::Quotas) {
            ctx.request_repaint();
            if matches!(action, IslandAction::Quotas) {
                if let Ok(mut c) = self.core.lock() { c.quota_view = true; c.quota_page = 0; }
            }
            if self.popup_open && self.popup_from_island && matches!(action, IslandAction::Expand) {
                self.popup_open = false;
            } else {
                self.popup_open = true;
                self.popup_from_island = true;
                self.popup_focused = false;
                mac::show_app();
                ctx.send_viewport_cmd_to(egui::ViewportId::from_hash_of("agent-island"), egui::ViewportCommand::Focus);
            }
        }
    }

    fn island_body(&mut self, ui: &mut Ui, root: &egui::Context, pill: Rect, h: f32, progress: f32) {
        if progress <= 0.0 { return; }
        let body = Rect::from_min_size(pos2(pill.left(), pill.top() + h), vec2(pill.width(), pill.width() * 1.2));
        let reveal = Rect::from_min_size(body.min, vec2(body.width(), body.height() * progress));
        ui.painter().rect_filled(reveal, CornerRadius { nw: 0, ne: 0, sw: 14, se: 14 }, Color32::BLACK);
        if self.popup_open && self.popup_from_island {
            let mut child = ui.new_child(UiBuilder::new().max_rect(body).layout(egui::Layout::top_down(egui::Align::LEFT)));
            child.set_clip_rect(reveal.intersect(body));
            self.popup_contents(&mut child, root);
        }
    }

    fn island_contents(ui: &mut Ui, active: &[IslandAgent], quotas: &[IslandQuota], brightness: f32, reduced: bool, notch: f32, h: f32, expanded: bool) -> (IslandAction, Rect) {
        let rect = ui.max_rect();
        let p = ui.painter().clone();
        let gap = notch;
        let count = active.len().min(2);
        let quota_count: usize = quotas.iter().take(2).map(|q| usize::from(q.session.is_some()) + usize::from(q.weekly.is_some())).sum();
        let left_width = 6.0 + count as f32 * 76.0;
        let right_width = if quota_count == 0 { 6.0 } else { 6.0 + quota_count as f32 * 42.0 };
        let left_edge = rect.center().x - gap / 2.0 - left_width;
        let right_edge = rect.center().x + gap / 2.0 + right_width;
        let hit = Rect::from_min_max(pos2(left_edge - 6.0, rect.top()), pos2(right_edge + 6.0, rect.top() + h + 2.0));
        let expansion = if reduced { 0.0 } else { ui.ctx().animate_bool_with_time(egui::Id::unique("island-hover"), ui.rect_contains_pointer(hit), 0.22) };
        let pill = Rect::from_min_max(pos2(left_edge - expansion * 6.0, rect.top()), pos2(right_edge + expansion * 6.0, rect.top() + h + expansion * 2.0));
        let corners = if expanded { CornerRadius::ZERO } else { CornerRadius::same(14) };
        p.rect_filled(pill, corners, Color32::BLACK);
        let background = ui.interact(pill, egui::Id::unique("island-expand"), Sense::click()).on_hover_text("Open keypad");
        let mut action = if background.clicked() { IslandAction::Expand } else { IslandAction::None };
        let now = model::now();
        for (i, agent) in active.iter().take(2).enumerate() {
            let left = rect.center().x - gap / 2.0 - 76.0 - i as f32 * 76.0;
            let tile = Rect::from_min_size(pos2(left, rect.top() + 3.0 + expansion * 0.5), vec2(72.0, h - 6.0));
            let response = ui.interact(tile, egui::Id::unique(("island-session", &agent.agent, &agent.session)), Sense::click())
                .on_hover_text(format!("{} · {}", agent.name, agent.title));
            p.rect_filled(tile, CornerRadius::same(9), Color32::from_rgb(20, 22, 29));
            paint::compact_field(&p, tile, agent.activity, agent.tool, agent.since, agent.orb, agent.glow, brightness, reduced, now);
            if response.hovered() { p.rect_filled(tile, CornerRadius::same(9), white(0.09)); }
            let text_rect = tile.shrink2(vec2(7.0, 1.0));
            let label = if agent.title.trim().is_empty() { &agent.name } else { &agent.title };
            let font = FontId::proportional(11.0);
            let text_p = p.with_clip_rect(text_rect);
            if reduced {
                let mut job = egui::text::LayoutJob::simple_singleline(label.clone(), font, white(0.96));
                job.wrap.max_width = text_rect.width();
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                let galley = p.layout_job(job);
                text_p.galley(pos2(text_rect.left(), text_rect.center().y - galley.size().y / 2.0), galley, white(0.96));
            } else {
                let galley = p.layout_no_wrap(label.clone(), font, white(0.96));
                let overflow = (galley.size().x - text_rect.width()).max(0.0);
                let offset = if overflow > 0.0 { let period = overflow as f64 / 20.0 + 2.0; ((now % period - 1.0).max(0.0) * 20.0).min(overflow as f64) as f32 } else { 0.0 };
                text_p.galley(pos2(text_rect.left() - offset, text_rect.center().y - galley.size().y / 2.0), galley, white(0.96));
            }
            if response.clicked() { action = IslandAction::Open(agent.agent.clone(), agent.session.clone()); }
        }
        let mut quota_x = rect.center().x + gap / 2.0 + 6.0;
        for quota in quotas.iter().take(2) {
            for (window, value) in [("Session", quota.session), ("Weekly", quota.weekly)] {
                let Some(value) = value else { continue };
                let card = Rect::from_min_size(pos2(quota_x, rect.top() + 3.0 + expansion * 0.5), vec2(40.0, h - 6.0));
                quota_x += 42.0;
                let response = ui.interact(card, egui::Id::unique(("island-quota", &quota.provider, window)), Sense::click())
                    .on_hover_text(format!("{} · {} · {:.0}% remaining", quota.provider, window, value));
                if response.hovered() { p.rect_filled(card, CornerRadius::same(8), white(0.10)); }
                p.text(card.center(), Align2::CENTER_CENTER, format!("{value:.0}%"), FontId::proportional(11.0), c32(quota.colour));
                if response.clicked() { action = IslandAction::Quotas; }
            }
        }

        (action, pill)
    }

    fn popup(&mut self, ctx: &egui::Context) {
        let builder = egui::ViewportBuilder::default().with_title("Padgent keypad")
            .with_inner_size([400.0, 480.0]).with_position(self.popup_position)
            .with_resizable(false).with_decorations(false).with_always_on_top();
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("tray-keypad"), builder, |ui, _| {
            if self.popup_from_island && !self.popup_focused {
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            self.popup_contents(ui, ctx);
        });
        if !self.popup_open { self.cancel_popup_inputs(); }
    }

    fn popup_contents(&mut self, ui: &mut Ui, ctx: &egui::Context) {
            let child_ctx = ui.ctx().clone();
            let focused = child_ctx.input(|i| i.viewport().focused.unwrap_or(false));
            if focused && !self.popup_focused { self.popup_focused = true; }
            if !self.screenshot_menu_bar && (child_ctx.input(|i| i.key_pressed(Key::Escape) || i.viewport().close_requested())
                || (self.popup_focused && !focused)) {
                self.popup_open = false;
                mac::log("tray keypad dismissed");
            }
            if !self.popup_open { return; }
            let p = ui.painter().clone();
            let full = ui.max_rect();
            let corners = if self.popup_from_island { CornerRadius { nw: 0, ne: 0, sw: 14, se: 14 } } else { CornerRadius::same(14) };
            p.rect_filled(full, corners, if self.popup_from_island { Color32::BLACK } else { Color32::from_rgb(29, 31, 39) });
            let (faces, waiting, brightness, reduced, glyphs, dimmed, held) = {
                let Ok(mut c) = self.core.lock() else { return };
                c.tick();
                if c.settings.demo_mode { c.demo_tick(); }
                (c.faces(), c32(c.waiting_colour()), c.settings.brightness, c.settings.motion == Motion::Reduced,
                    c.settings.key_glyphs, c.dimmed(), [c.page_held(false), c.page_held(true)])
            };
            let t = model::now();
            let factor = full.width() / 400.0;
            let dev = Rect::from_min_size(full.min + vec2(12.0, 12.0) * factor, vec2(376.0, 414.0) * factor);
            let grid_min = dev.min + vec2(16.0, 16.0) * factor;
            let scale = (344.0 / 434.0) * factor;
            let size = model::KEY_SIZE as f32 * scale;
            let pitch = model::PITCH as f32 * scale;
            let mut down = [false; KEY_COUNT];
            for (i, face) in faces.iter().enumerate() {
                let base = Rect::from_min_size(grid_min + vec2((i % 3) as f32 * pitch, (i / 3) as f32 * pitch), vec2(size, size));
                let resp = ui.interact(base, egui::Id::unique(("tray-key", i)), Sense::click_and_drag());
                down[i] = resp.is_pointer_button_down_on();
                let size_factor = child_ctx.animate_value_with_time(egui::Id::unique(("tray-scale", i)),
                    if down[i] { 0.94 } else if resp.hovered() { 1.03 } else { 1.0 }, 0.14);
                let hold = self.core.lock().map(|c| c.hold_progress(i)).unwrap_or(0.0);
                paint::paint_key(&p, Rect::from_center_size(base.center(), base.size() * size_factor), face, &paint::KeyCx {
                    t, hover: resp.hovered(), pressed: down[i], hold, reduced_motion: reduced,
                    brightness: if dimmed { brightness * 0.6 } else { brightness }, glyphs, waiting,
                });
            }
            let mut page_down = [false; 2];
            for i in 0..2 {
                let r = Rect::from_min_size(pos2(dev.left() + (78.0 + i as f32 * 128.0) * factor, dev.bottom() - 42.0 * factor), vec2(92.0, 30.0) * factor);
                let resp = ui.interact(r, egui::Id::unique(("tray-page", i)), Sense::click_and_drag());
                page_down[i] = resp.is_pointer_button_down_on();
                paint::page_button(&p, r, if i == 0 { "‹" } else { "›" }, held[i], resp.hovered());
            }
            if let Ok(mut c) = self.core.lock() {
                for i in 0..KEY_COUNT {
                    if down[i] && !self.popup_keys[i] { c.key_down(i); }
                    if !down[i] && self.popup_keys[i] { c.key_up(i); }
                }
                for i in 0..2 { if page_down[i] != self.popup_pages[i] { c.page_button(i == 1, page_down[i]); } }
            }
            self.popup_keys = down;
            self.popup_pages = page_down;
            let r = Rect::from_min_size(full.min + vec2(12.0, 436.0) * factor, vec2(376.0, 30.0) * factor);
            if ui.put(r, egui::Button::new(RichText::new("Open Padgent & Settings…").size(14.0 * factor))).clicked() {
                self.popup_open = false;
                mac::log("tray keypad dismissed for full app");
                self.page = Page::Keypad;
                mac::show_app();
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
            }
            child_ctx.request_repaint_after(std::time::Duration::from_millis(33));
    }

    fn cancel_popup_inputs(&mut self) {
        if let Ok(mut c) = self.core.lock() {
            for i in 0..KEY_COUNT { if self.popup_keys[i] { c.cancel_key(i); } }
            for i in 0..2 { if self.popup_pages[i] { c.page_button(i == 1, false); } }
        }
        self.popup_keys = [false; KEY_COUNT];
        self.popup_pages = [false; 2];
    }

    fn sidebar(&mut self, ui: &mut Ui, rect: Rect) {
        let p = ui.painter().clone();
        paint::glass(&p, rect, 22.0, white(0.4), 0.9, true);
        let mut y = rect.top() + 46.0;
        p.text(pos2(rect.left() + 20.0, y), Align2::LEFT_CENTER, "Padgent", FontId::proportional(20.0), white(0.96));
        y += 18.0;
        let label = self.core.lock().map(|c| c.control_label()).unwrap_or("");
        p.text(pos2(rect.left() + 20.0, y), Align2::LEFT_CENTER, label, FontId::proportional(11.5), white(0.5));
        y += 26.0;
        for (page, name, glyph) in PAGES {
            let r = Rect::from_min_size(pos2(rect.left() + 10.0, y), vec2(rect.width() - 20.0, 34.0));
            let resp = ui.interact(r, egui::Id::unique(("nav", name)), Sense::click());
            let sel = self.page == page;
            let t = ui.ctx().animate_bool_with_time(egui::Id::unique(("navsel", name)), sel, 0.18);
            if t > 0.01 {
                p.rect_filled(r, CornerRadius::same(10), with_alpha(Color32::from_rgb(10, 132, 255), 0.85 * t));
            } else if resp.hovered() {
                p.rect_filled(r, CornerRadius::same(10), white(0.07));
            }
            // macOS-style coloured settings icon.
            let ir = Rect::from_center_size(pos2(r.left() + 20.0, r.center().y), vec2(20.0, 20.0));
            p.rect_filled(ir, CornerRadius::same(6), Color32::from_rgb(glyph[0], glyph[1], glyph[2]));
            p.circle_filled(ir.center(), 4.0, white(0.92));
            p.text(pos2(r.left() + 38.0, r.center().y), Align2::LEFT_CENTER, name, FontId::proportional(13.5), white(0.92));
            if resp.clicked() {
                self.page = page;
            }
            y += 38.0;
        }
        // Waiting summary at the bottom.
        if let Ok(c) = self.core.lock() {
            let w = c.waiting_count();
            let n = c.visible().len();
            let txt = if w > 0 { format!("{w} waiting · {n} sessions") } else { format!("{n} sessions") };
            let pos = pos2(rect.left() + 20.0, rect.bottom() - 26.0);
            p.circle_filled(pos, 4.0, if w > 0 { c32(c.waiting_colour()) } else { white(0.35) });
            p.text(pos + vec2(12.0, 0.0), Align2::LEFT_CENTER, txt, FontId::proportional(12.0), white(0.6));
        }
    }

    fn keypad_page(&mut self, ui: &mut Ui, rect: Rect) {
        let ctx = ui.ctx().clone();
        let p = ui.painter().clone();
        let t = model::now();
        let (faces, in_control, hw, waiting, brightness, reduced, glyphs, notice, dimmed, hand_back, page_held) = {
            let Ok(mut c) = self.core.lock() else { return };
            c.tick();
            if c.settings.demo_mode {
                c.demo_tick();
            }
            (
                c.faces(),
                c.in_control,
                c.hardware.clone(),
                c32(c.waiting_colour()),
                c.settings.brightness,
                c.settings.motion == Motion::Reduced,
                c.settings.key_glyphs,
                c.notice.clone(),
                c.dimmed(),
                c.hand_back_progress(),
                [c.page_held(false), c.page_held(true)],
            )
        };

        // Header.
        p.text(rect.min + vec2(4.0, 18.0), Align2::LEFT_CENTER, "Keypad", FontId::proportional(26.0), white(0.97));
        let sub = match &hw {
            Hardware::Off => "MX Creative Keypad, shown live on screen. Turn on the physical keypad in Menu bar and hand-over.".to_string(),
            Hardware::NoKeypad => "No keypad connected. Plug in the MX Creative Keypad with its USB-C cable.".to_string(),
            Hardware::Connected => "Keypad connected.".to_string(),
            Hardware::Driving => "Padgent has the keypad. Each key is one session in its agent's colour; the hub key counts what is waiting.".to_string(),
            Hardware::Problem(e) => e.clone(),
        };
        p.text(rect.min + vec2(4.0, 44.0), Align2::LEFT_CENTER, sub, FontId::proportional(12.5), white(0.55));
        let btn_rect = Rect::from_min_size(pos2(rect.right() - 340.0, rect.top() + 4.0), vec2(200.0, 32.0));
        let label = if in_control { "Give Keypad to Options+" } else { "Take Over Keypad" };
        if ui.put(btn_rect, egui::Button::new(label).corner_radius(16)).clicked() {
            if let Ok(mut c) = self.core.lock() {
                if c.in_control { c.release() } else { c.take_over() }
            }
        }
        let demo_rect = Rect::from_min_size(pos2(rect.right() - 130.0, rect.top() + 4.0), vec2(130.0, 32.0));
        let demo_on = self.core.lock().map(|c| c.settings.demo_mode).unwrap_or(false);
        if ui.put(demo_rect, egui::Button::new(if demo_on { "Hide samples" } else { "Show samples" }).corner_radius(16)).clicked() {
            if let Ok(mut c) = self.core.lock() {
                c.settings.demo_mode = !demo_on;
                if c.settings.demo_mode {
                    c.load_demo();
                } else {
                    c.sessions.retain(|s| !s.id.starts_with("demo-"));
                }
                c.revision += 1;
            }
            self.settings_rev += 1;
        }

        // Layout: device on the left, menu bar preview on the right.
        let preview_w = 300.0;
        let area = Rect::from_min_max(pos2(rect.left(), rect.top() + 70.0), pos2(rect.right() - preview_w - 20.0, rect.bottom()));
        let page_h = 46.0;
        let pad = 26.0;
        let avail = (area.width() - pad * 2.0).min(area.height() - pad * 3.0 - page_h - 34.0).max(200.0);
        let s = avail / 434.0;
        let grid = 434.0 * s;
        let dev_size = vec2(grid + pad * 2.0, grid + pad * 3.0 + page_h);
        let dev = Rect::from_center_size(area.center() - vec2(0.0, 14.0), dev_size);
        paint::paint_device(&p, dev);
        let grid_min = dev.min + vec2(pad, pad);

        let key_size = model::KEY_SIZE as f32 * s;
        let pitch = model::PITCH as f32 * s;
        let mut down_now = [false; KEY_COUNT];
        for (i, face) in faces.iter().enumerate() {
            let col = (i % 3) as f32;
            let row = (i / 3) as f32;
            let mut target = grid_min + vec2((col * pitch).round(), (row * pitch).round());
            // Keys glide to their new place when sessions reorder.
            // Same travel as the original (Travel.position 0x1005058e4): 0.8 s, ease-in-out cubic.
            if let model::KeyFace::Session(sf) = face {
                let slot = (target.x, target.y);
                let tr = self.travels.entry(sf.key.clone()).or_insert((slot, slot, t));
                if tr.1 != slot {
                    let (fx, fy) = travel_pos(*tr, t);
                    *tr = ((fx, fy), slot, t);
                }
                let (x, y) = if reduced { slot } else { travel_pos(*tr, t) };
                target = pos2(x, y);
            }
            let base = Rect::from_min_size(target, vec2(key_size, key_size));
            let resp = ui.interact(base, egui::Id::unique(("key", i)), Sense::click_and_drag());
            let pressed = resp.is_pointer_button_down_on();
            down_now[i] = pressed;
            let scale = ctx.animate_value_with_time(
                egui::Id::unique(("ks", i)),
                if pressed { 0.94 } else if resp.hovered() { 1.03 } else { 1.0 },
                0.14,
            );
            let r = Rect::from_center_size(base.center(), base.size() * scale);
            let hold = self.core.lock().map(|c| c.hold_progress(i)).unwrap_or(0.0);
            let cx = paint::KeyCx {
                t,
                hover: resp.hovered(),
                pressed,
                hold,
                reduced_motion: reduced,
                brightness: if dimmed { brightness * 0.6 } else { brightness },
                glyphs,
                waiting,
            };
            paint::paint_key(&p, r, face, &cx);
        }
        // Keyboard: 1–9 press keys.
        let digit_keys = [Key::Num1, Key::Num2, Key::Num3, Key::Num4, Key::Num5, Key::Num6, Key::Num7, Key::Num8, Key::Num9];
        let typing = ctx.egui_wants_keyboard_input();
        for (i, k) in digit_keys.iter().enumerate() {
            if !typing && ctx.input(|inp| inp.key_down(*k)) {
                down_now[i] = true;
            }
        }
        if let Ok(mut c) = self.core.lock() {
            for i in 0..KEY_COUNT {
                if down_now[i] && !self.keys_prev[i] {
                    c.key_down(i);
                } else if !down_now[i] && self.keys_prev[i] {
                    c.key_up(i);
                }
            }
        }
        self.keys_prev = down_now;

        // Previous / next page buttons (also ← and →).
        let bw = grid * 0.3;
        let by = dev.bottom() - pad - page_h;
        let prev_r = Rect::from_min_size(pos2(dev.center().x - bw - 12.0, by), vec2(bw, page_h));
        let next_r = Rect::from_min_size(pos2(dev.center().x + 12.0, by), vec2(bw, page_h));
        let mut page_now = [false; 2];
        for (i, r) in [prev_r, next_r].iter().enumerate() {
            let resp = ui.interact(*r, egui::Id::unique(("page", i)), Sense::click_and_drag());
            let key = if i == 0 { Key::ArrowLeft } else { Key::ArrowRight };
            page_now[i] = resp.is_pointer_button_down_on() || (!typing && ctx.input(|inp| inp.key_down(key)));
            paint::page_button(&p, *r, if i == 0 { "‹" } else { "›" }, page_held[i], resp.hovered());
        }
        if let Ok(mut c) = self.core.lock() {
            for i in 0..2 {
                if page_now[i] != self.pages_prev[i] {
                    c.page_button(i == 1, page_now[i]);
                }
            }
        }
        self.pages_prev = page_now;
        if hand_back > 0.02 {
            let c = pos2(dev.center().x, by + page_h / 2.0);
            paint::arc(&p, c, 14.0, -std::f32::consts::FRAC_PI_2, std::f32::consts::TAU * hand_back, Stroke::new(3.0, white(0.9)));
        }
        p.text(
            pos2(dev.center().x, dev.bottom() + 16.0),
            Align2::CENTER_CENTER,
            "Hold a key 0.6 s to archive its conversation until work resumes · ‹ › page · hold both 1.5 s to hand back to Options+",
            FontId::proportional(11.5),
            white(0.45),
        );

        if let Some((text, _)) = notice {
            let galley = p.layout(text, FontId::proportional(12.5), white(0.95), area.width() * 0.8);
            let r = Rect::from_center_size(pos2(area.center().x, area.top() + 4.0), galley.size() + vec2(28.0, 14.0));
            paint::glass(&p, r, r.height() / 2.0, white(0.6), 1.2, true);
            p.galley(r.min + vec2(14.0, 7.0), galley, white(0.95));
        }

        // Menu bar preview.
        let pr = Rect::from_min_max(pos2(rect.right() - preview_w, rect.top() + 70.0), pos2(rect.right(), rect.bottom()));
        self.menu_preview(ui, pr, t, reduced);
    }

    fn menu_preview(&mut self, ui: &mut Ui, r: Rect, t: f64, reduced: bool) {
        let p = ui.painter().clone();
        paint::glass(&p, r, 22.0, white(0.4), 1.0, true);
        p.text(r.min + vec2(18.0, 24.0), Align2::LEFT_CENTER, "Menu bar", FontId::proportional(15.0), white(0.95));
        let Ok(c) = self.core.lock() else { return };
        let vis: Vec<model::Session> = c.visible().into_iter().cloned().collect();
        let waiting = c.waiting_count();
        p.text(
            r.min + vec2(18.0, 44.0),
            Align2::LEFT_CENTER,
            if waiting > 0 { format!("{waiting} waiting for you") } else { "Nothing is waiting".into() },
            FontId::proportional(12.0),
            if waiting > 0 { c32(c.waiting_colour()) } else { white(0.5) },
        );
        let mut y = r.top() + 64.0;
        let row_h = 58.0;
        let mut open: Option<(String, String)> = None;
        for s in vis.iter() {
            if y + row_h > r.bottom() - 10.0 {
                p.text(pos2(r.left() + 18.0, y + 10.0), Align2::LEFT_CENTER, format!("+{} more", vis.len() - ((y - r.top() - 64.0) / row_h) as usize), FontId::proportional(12.0), white(0.5));
                break;
            }
            let row = Rect::from_min_size(pos2(r.left() + 8.0, y), vec2(r.width() - 16.0, row_h - 4.0));
            let resp = ui.interact(row, egui::Id::unique(("prow", s.key())), Sense::click());
            if resp.hovered() {
                p.rect_filled(row, CornerRadius::same(12), white(0.07));
            }
            let (orb, glow) = c.colours(s);
            let oc = pos2(row.left() + 20.0, row.top() + 18.0);
            let g = s.activity.group();
            let pulse = if reduced { 0.6 } else { 0.5 + 0.5 * (((t % 1000.0) as f32) * std::f32::consts::TAU / if g == Group::Waiting { 1.6 } else { 2.4 }).sin() };
            if matches!(g, Group::Working | Group::Waiting) {
                paint::glow(&p, oc, 16.0, c32(glow), 0.4 + 0.4 * pulse);
            }
            p.circle_filled(oc, 6.0, c32(orb));
            let agent = c.agent(&s.agent).map(|a| a.name.clone()).unwrap_or_else(|| s.agent.clone());
            let mut job = egui::text::LayoutJob::simple_singleline(s.title.clone(), FontId::proportional(13.5), white(0.95));
            job.wrap.max_width = row.width() - 46.0;
            job.wrap.max_rows = 1;
            job.wrap.overflow_character = Some('…');
            let title = p.layout_job(job);
            p.galley(pos2(row.left() + 38.0, row.top() + 9.0), title, white(0.95));
            let age = model::age_text(s.updated, model::now());
            let meta = if age.is_empty() || g != Group::Idle {
                format!("{agent} · {}", s.activity.label())
            } else {
                format!("{agent} · {} · {age}", s.activity.label())
            };
            p.text(pos2(row.left() + 38.0, row.top() + 33.0), Align2::LEFT_CENTER, meta, FontId::proportional(11.5), if g == Group::Waiting { c32(c.waiting_colour()) } else { white(0.55) });
            let recent = model::now() - s.text_at <= model::RECENT_TEXT_S;
            let line = model::key_text(if recent { &s.text } else { "" }, s.unread, s.activity);
            if !line.is_empty() {
                let mut job = egui::text::LayoutJob::simple_singleline(line.clone(), FontId::proportional(11.0), white(0.42));
                job.wrap.max_width = row.width() - 46.0;
                job.wrap.max_rows = 1;
                job.wrap.overflow_character = Some('…');
                let g2 = p.layout_job(job);
                p.galley(pos2(row.left() + 38.0, row.top() + 40.0), g2, white(0.42));
            }
            if resp.clicked() {
                open = Some((s.agent.clone(), s.id.clone()));
            }
            y += row_h;
        }
        if vis.is_empty() {
            let msg = "No agent sessions yet.\n\nInstall the Claude Code hooks in Agents, or let any agent post its state to this app. Sessions appear here and on the keypad.";
            let galley = p.layout(msg.to_string(), FontId::proportional(12.5), white(0.55), r.width() - 36.0);
            p.galley(pos2(r.left() + 18.0, y + 6.0), galley, white(0.55));
        }
        let agents = c.agents.clone();
        drop(c);
        if let Some((a, s)) = open {
            self.open_exact(&agents, &a, &s);
        }
    }

    fn agents_page(&mut self, ui: &mut Ui) {
        ui.heading("Agents");
        hint(ui, "Padgent shows your agents' sessions side by side, each in its own colour. Choose the agents you use. Add other kinds of agents by importing their file, and share yours by exporting it.");
        ui.add_space(6.0);
        let (agents, port, server_status, disabled, hues) = match self.core.lock() {
            Ok(c) => (c.agents.clone(), c.settings.hook_port, c.server_status.clone(), c.settings.disabled_agents.clone(), c.settings.agent_hues.clone()),
            Err(_) => return,
        };
        let mut changed = false;
        changed |= self.follow_apps_card(ui, &agents);
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if ui.button("Import agent…").clicked() {
                if let Some(path) = rfd::FileDialog::new().add_filter("Padgent agent", &["json", "orbagent"]).pick_file() {
                    self.agent_notes = match std::fs::read_to_string(&path).map_err(|e| e.to_string()).and_then(|t| model::parse_agent_file(&t)) {
                        Ok(list) => {
                            let mut notes = Vec::new();
                            for a in list {
                                match model::validate_agent(a) {
                                    Ok((a, n)) => {
                                        notes.extend(n);
                                        let _ = model::save_custom_agent(&a);
                                        notes.push(format!("Added {}.", a.name));
                                        self.selected_agent = a.id.clone();
                                    }
                                    Err(e) => notes.push(e),
                                }
                            }
                            changed = true;
                            notes
                        }
                        Err(e) => vec![e],
                    };
                }
            }
            if ui.button("Export selected…").clicked() {
                if let Some(a) = agents.iter().find(|a| a.id == self.selected_agent) {
                    if let Some(path) = rfd::FileDialog::new().set_file_name(format!("{}.orbagent.json", a.id)).save_file() {
                        let mut out = a.clone();
                        if let Some(h) = hues.get(&a.id) {
                            out.hue = *h;
                        }
                        self.agent_notes = vec![match std::fs::write(&path, serde_json::to_string_pretty(&out).unwrap_or_default()) {
                            Ok(_) => format!("Exported {}.", a.name),
                            Err(e) => format!("Export failed. Nothing was saved. ({e})"),
                        }];
                    }
                }
            }
            if ui.button("Export all…").clicked() {
                if let Some(path) = rfd::FileDialog::new().set_file_name("agents.orbagent.json").save_file() {
                    let bundle = serde_json::json!({ "format": model::AGENTS_FORMAT, "version": 1, "agents": agents });
                    self.agent_notes = vec![match std::fs::write(&path, serde_json::to_string_pretty(&bundle).unwrap_or_default()) {
                        Ok(_) => format!("Exported {} agents.", agents.len()),
                        Err(e) => format!("Export failed. Nothing was saved. ({e})"),
                    }];
                }
            }
            if ui.button("New agent…").clicked() {
                self.new_agent = Some(AgentDef {
                    format: model::AGENT_FORMAT.into(),
                    version: 1,
                    id: "com.example.myagent".into(),
                    name: "My Agent".into(),
                    badge: String::new(),
                    hue: colour::calm_default(),
                    description: String::new(),
                    state_colours: Default::default(),
                    app_name: String::new(),
                    open_url: String::new(),
                    events: Default::default(),
                    style: "orb".into(),
                    watch_app: String::new(),
                    electron: false,
                    models: Default::default(),
                    builtin: false,
                });
            }
        });
        for n in &self.agent_notes {
            hint(ui, n);
        }
        if let Some(mut a) = self.new_agent.take() {
            let mut keep = true;
            card(ui, "New agent", |ui| {
                egui::Grid::new("newagent").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Name");
                    ui.text_edit_singleline(&mut a.name);
                    ui.end_row();
                    ui.label("Id (reverse-DNS)");
                    ui.text_edit_singleline(&mut a.id);
                    ui.end_row();
                    ui.label("App to open");
                    ui.text_edit_singleline(&mut a.app_name);
                    ui.end_row();
                    ui.label("Follow app (bundle id)");
                    ui.text_edit_singleline(&mut a.watch_app);
                    ui.end_row();
                    ui.label("Colour");
                    ui.add(egui::Slider::new(&mut a.hue, 0.0..=359.0).suffix("°"));
                    ui.end_row();
                    ui.label("Key look");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut a.style, "orb".to_string(), "Orb");
                        ui.radio_value(&mut a.style, "dots".to_string(), "Status dots");
                    });
                    ui.end_row();
                });
                ui.horizontal(|ui| {
                    if ui.button("Add").clicked() {
                        match model::validate_agent(a.clone()) {
                            Ok((v, notes)) => {
                                let _ = model::save_custom_agent(&v);
                                self.agent_notes = notes;
                                self.agent_notes.push(format!("Added {}.", v.name));
                                self.selected_agent = v.id;
                                changed = true;
                                keep = false;
                            }
                            Err(e) => self.agent_notes = vec![e],
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        keep = false;
                    }
                });
            });
            if keep {
                self.new_agent = Some(a);
            }
        }
        ui.add_space(6.0);
        ui.columns(2, |cols| {
            card(&mut cols[0], "Your agents", |ui| {
                for a in &agents {
                    let on = !disabled.contains(&a.id);
                    let hue = hues.get(&a.id).copied().unwrap_or(a.hue);
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
                        ui.painter().circle_filled(r.center(), 6.0, c32(colour::oklch(0.78, 0.15, hue)));
                        let sel = self.selected_agent == a.id;
                        if ui.selectable_label(sel, RichText::new(&a.name).size(14.0)).clicked() {
                            self.selected_agent = a.id.clone();
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut v = on;
                            if ui.checkbox(&mut v, "").changed() {
                                if let Ok(mut c) = self.core.lock() {
                                    c.settings.disabled_agents.retain(|d| d != &a.id);
                                    if !v {
                                        c.settings.disabled_agents.push(a.id.clone());
                                    }
                                    c.revision += 1;
                                }
                                self.settings_rev += 1;
                            }
                            if !a.builtin {
                                ui.label(RichText::new("imported").size(11.0).color(white(0.45)));
                            }
                        });
                    });
                }
            });
            let ui = &mut cols[1];
            if let Some(a) = agents.iter().find(|a| a.id == self.selected_agent).cloned() {
                card(ui, &a.name, |ui| {
                    if !a.description.is_empty() {
                        hint(ui, &a.description);
                    }
                    let mut hue = hues.get(&a.id).copied().unwrap_or(a.hue);
                    ui.horizontal(|ui| {
                        ui.label("Colour");
                        if ui.add(egui::Slider::new(&mut hue, 0.0..=359.0).suffix("°")).changed() {
                            let (h, moved) = colour::keep_out_of_attention_band(hue);
                            if let Ok(mut c) = self.core.lock() {
                                c.settings.agent_hues.insert(a.id.clone(), h);
                                c.revision += 1;
                                if moved {
                                    c.notify("Colour adjusted to keep states distinct: that hue is kept for sessions waiting for you.");
                                }
                            }
                            self.settings_rev += 1;
                        }
                        if hues.contains_key(&a.id) && ui.small_button("Reset").clicked() {
                            if let Ok(mut c) = self.core.lock() {
                                c.settings.agent_hues.remove(&a.id);
                            }
                            self.settings_rev += 1;
                        }
                    });
                    ui.label(RichText::new(format!("Key look: {}", if a.style == "dots" { "status dots and last message" } else { "orb" })).size(12.5).color(white(0.6)));
                    ui.separator();
                    ui.label(RichText::new("Connect it").strong());
                    if a.id == "claude-code" {
                        let installed = integrations::claude_hooks_installed();
                        ui.horizontal(|ui| {
                            if ui.button(if installed { "Reinstall hooks" } else { "Install hooks" }).clicked() {
                                self.agent_notes = vec![integrations::install_claude_hooks(port).unwrap_or_else(|e| e)];
                            }
                            if installed && ui.button("Remove hooks").clicked() {
                                self.agent_notes = vec![integrations::remove_claude_hooks().unwrap_or_else(|e| e)];
                            }
                        });
                        hint(ui, "Adds a small hook to ~/.claude/settings.json that reports each session's state to this app. Your other hooks are kept and the original file is backed up.");
                    } else {
                        hint(ui, "Point the agent's hooks (or any script) at this command; it reads the hook JSON from standard input:");
                        let cmd = integrations::hook_command(&a.id, port);
                        // Read-only but selectable, so ⌘C works on a selection too.
                        ui.add(egui::TextEdit::multiline(&mut cmd.as_str()).font(egui::TextStyle::Monospace).desired_rows(2).desired_width(f32::INFINITY));
                        if ui.button("Copy hook command").clicked() {
                            ui.ctx().copy_text(cmd.clone());
                            self.agent_notes = vec![if integrations::copy_to_clipboard(&cmd) { "Hook command copied.".into() } else { "Couldn't copy: select the command and press ⌘C.".into() }];
                        }
                        hint(ui, "Or post a state directly:");
                        let ex = integrations::example_event(&a.id, port);
                        ui.add(egui::TextEdit::multiline(&mut ex.as_str()).font(egui::TextStyle::Monospace).desired_rows(2).desired_width(f32::INFINITY));
                        if ui.button("Copy example").clicked() {
                            ui.ctx().copy_text(ex.clone());
                            self.agent_notes = vec![if integrations::copy_to_clipboard(&ex) { "Example copied.".into() } else { "Couldn't copy: select the example and press ⌘C.".into() }];
                        }
                    }
                    ui.separator();
                    ui.label(RichText::new("Follow its desktop app").strong());
                    hint(ui, "Follows the app's open conversations, cloud or local, without any login here: whether a reply is being written and the last message. Uses macOS Accessibility, read-only.");
                    let current = self.core.lock().ok().map(|c| c.settings.watch_apps.get(&a.id).cloned().unwrap_or_else(|| a.watch_app.clone())).unwrap_or_default();
                    let mut bundle = current.clone();
                    ui.horizontal(|ui| {
                        ui.add(egui::TextEdit::singleline(&mut bundle).hint_text("App bundle id, e.g. com.openai.chat").desired_width(240.0));
                        egui::ComboBox::from_id_salt(("pickapp", &a.id)).selected_text("Pick a running app").width(170.0).show_ui(ui, |ui| {
                            for (name, b) in watch::running_apps() {
                                if ui.selectable_label(bundle == b, format!("{name}  ({b})")).clicked() {
                                    bundle = b;
                                }
                            }
                        });
                    });
                    if bundle != current {
                        if let Ok(mut c) = self.core.lock() {
                            c.settings.watch_apps.insert(a.id.clone(), bundle.trim().to_string());
                        }
                        self.settings_rev += 1;
                    }
                    if !bundle.trim().is_empty() && !watch::trusted() {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Accessibility is off for this app.").color(Color32::from_rgb(255, 180, 90)));
                            if ui.button("Allow in System Settings").clicked() {
                                watch::request_access();
                            }
                        });
                    }
                    if a.id == "claude-code" {
                        ui.separator();
                        ui.label(RichText::new("Model roles").strong());
                        let mut roles = self.core.lock().map(|c| c.settings.model_roles.clone()).unwrap_or_default();
                        let mut edited = false;
                        egui::Grid::new("roles").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                            for (role, label) in [("swarm", "Swarm (sub-agents)"), ("code", "Code"), ("orchestration", "Orchestration")] {
                                ui.label(label);
                                let v = roles.entry(role.to_string()).or_default();
                                edited |= ui.add(egui::TextEdit::singleline(v).desired_width(220.0)).changed();
                                ui.end_row();
                            }
                        });
                        if edited {
                            if let Ok(mut c) = self.core.lock() {
                                c.settings.model_roles = roles.clone();
                            }
                            self.settings_rev += 1;
                        }
                        ui.horizontal(|ui| {
                            if ui.button("Apply to Claude Code").clicked() {
                                self.agent_notes = vec![integrations::apply_claude_models(&roles).unwrap_or_else(|e| e)];
                            }
                            if ui.small_button("Defaults").clicked() {
                                if let Ok(mut c) = self.core.lock() {
                                    c.settings.model_roles = model::default_model_roles();
                                }
                                self.settings_rev += 1;
                            }
                        });
                        hint(ui, "Haiku for the swarm, Sonnet for code, Opus for orchestration.");
                    }
                    ui.separator();
                    hint(ui, &server_status);
                    if !a.builtin && ui.button("Remove this agent").clicked() {
                        model::delete_custom_agent(&a.id);
                        changed = true;
                        self.selected_agent = "claude-code".into();
                    }
                });
            }
        });
        if changed {
            if let Ok(mut c) = self.core.lock() {
                let mut list = model::builtin_agents();
                list.extend(model::load_custom_agents());
                c.agents = list;
                c.revision += 1;
            }
        }
    }

    /// Apps followed through Accessibility, with buttons to add more and to allow access.
    fn follow_apps_card(&mut self, ui: &mut Ui, agents: &[AgentDef]) -> bool {
        let mut changed = false;
        let watched = match self.core.lock() {
            Ok(c) => c.watched_apps(),
            Err(_) => return false,
        };
        let running: Vec<(String, String)> = watch::running_apps();
        let trusted = watch::trusted();
        card(ui, "Apps to follow", |ui| {
            hint(ui, "Padgent follows the open conversations of these desktop apps, cloud or local, without any login here: whether a reply is being written and its last message. macOS asks you to allow Padgent in Accessibility first.");
            ui.horizontal(|ui| {
                let (r, _) = ui.allocate_exact_size(vec2(10.0, 10.0), Sense::hover());
                ui.painter().circle_filled(r.center(), 5.0, if trusted { Color32::from_rgb(48, 209, 88) } else { Color32::from_rgb(255, 159, 10) });
                ui.label(if trusted { "Accessibility: allowed" } else { "Accessibility: not allowed yet" });
                if ui.button(if trusted { "Open Accessibility settings" } else { "Allow Padgent in Accessibility" }).clicked() {
                    watch::request_access();
                }
                if ui.button("Allow Input Monitoring (keypad)").clicked() {
                    watch::request_input_monitoring();
                }
            });
            if self.core.lock().map(|c| c.accessibility_reset_failed).unwrap_or(false) && !trusted {
                ui.label(RichText::new("macOS kept the Accessibility entry of an older AgentPad, which it no longer accepts. In Accessibility, select the old AgentPad entry and click –, then click +, choose Padgent in Applications and turn it on.").color(Color32::from_rgb(255, 180, 90)));
            }
            ui.separator();
            if watched.is_empty() {
                hint(ui, "No apps followed yet.");
            }
            for (agent_id, bundle, _) in &watched {
                let name = agents.iter().find(|a| &a.id == agent_id).map(|a| a.name.clone()).unwrap_or_else(|| agent_id.clone());
                let is_running = running.iter().any(|(_, b)| b == bundle);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(&name).strong());
                    ui.label(RichText::new(bundle).size(12.0).color(white(0.5)));
                    ui.label(RichText::new(if is_running { "running" } else { "not open" }).size(12.0).color(if is_running { Color32::from_rgb(48, 209, 88) } else { white(0.4) }));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Stop following").clicked() {
                            if let Ok(mut c) = self.core.lock() {
                                c.settings.watch_apps.insert(agent_id.clone(), String::new());
                            }
                            self.settings_rev += 1;
                        }
                    });
                });
            }
            ui.separator();
            ui.horizontal(|ui| {
                let mut add: Option<(String, String)> = None;
                egui::ComboBox::from_id_salt("add-running-app").selected_text("+ Add a running app").width(230.0).show_ui(ui, |ui| {
                    for (name, b) in &running {
                        if watched.iter().any(|(_, wb, _)| wb == b) || b == "com.agentpad.app" {
                            continue;
                        }
                        if ui.selectable_label(false, format!("{name}  ({b})")).clicked() {
                            add = Some((name.clone(), b.clone()));
                        }
                    }
                });
                if ui.button("+ Choose an app…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().set_directory("/Applications").add_filter("App", &["app"]).pick_folder() {
                        match watch::app_info(&path) {
                            Some(info) => add = Some(info),
                            None => self.agent_notes = vec!["That doesn't look like an app.".into()],
                        }
                    }
                }
                if let Some((name, bundle)) = add {
                    // Turn a built-in agent back on if it already knows this app.
                    let builtin = agents.iter().find(|a| a.watch_app == bundle).map(|a| a.id.clone());
                    if let Some(id) = builtin {
                        if let Ok(mut c) = self.core.lock() {
                            c.settings.watch_apps.remove(&id);
                            c.settings.disabled_agents.retain(|d| d != &id);
                        }
                        self.agent_notes = vec![format!("Following {name} again.")];
                    } else {
                        let a = model::agent_for_app(&name, &bundle);
                        match model::validate_agent(a) {
                            Ok((a, _)) => {
                                let _ = model::save_custom_agent(&a);
                                self.selected_agent = a.id.clone();
                                self.agent_notes = vec![format!("Following {name}. Open a conversation in it and it gets a key.")];
                                changed = true;
                            }
                            Err(e) => self.agent_notes = vec![e],
                        }
                    }
                    self.settings_rev += 1;
                    if !trusted {
                        watch::request_access();
                    }
                }
            });
        });
        changed
    }

    fn look_page(&mut self, ui: &mut Ui) {
        ui.heading("Look");
        hint(ui, "Theme, colours, brightness and motion.");
        let Ok(mut c) = self.core.lock() else { return };
        let before = format!("{:?}", c.settings);
        let s = &mut c.settings;
        card(ui, "Key colours", |ui| {
            ui.horizontal(|ui| {
                ui.label("Colour keys by");
                ui.selectable_value(&mut s.colour_keys_by, ColourKeysBy::Agent, "Agent");
                ui.selectable_value(&mut s.colour_keys_by, ColourKeysBy::State, "State");
            });
            hint(ui, "Agent gives each agent its own colour. State gives every agent the same colour for each state; the badge on each key shows which agent it is.");
            if s.colour_keys_by == ColourKeysBy::State {
                for k in ["working", "done", "idle"] {
                    let mut col = s.state_colours.get(k).and_then(|h| colour::Rgb::parse(h)).unwrap_or(colour::Rgb(140, 140, 150));
                    let mut arr = [col.0, col.1, col.2];
                    ui.horizontal(|ui| {
                        if ui.color_edit_button_srgb(&mut arr).changed() {
                            col = colour::Rgb(arr[0], arr[1], arr[2]);
                            s.state_colours.insert(k.to_string(), col.hex());
                        }
                        ui.label(format!("Key colour when {k}"));
                    });
                }
            }
            ui.horizontal(|ui| {
                ui.label("Waiting for you");
                ui.add(egui::Slider::new(&mut s.waiting_hue, colour::ATTENTION_BAND.0..=colour::ATTENTION_BAND.1).suffix("°"));
            });
            hint(ui, "A session that waits for you glows the same colour, whichever agent runs it. Error always stays red.");
        });
        card(ui, "Keypad", |ui| {
            ui.horizontal(|ui| {
                ui.label("Keypad brightness");
                ui.add(egui::Slider::new(&mut s.brightness, 0.1..=1.0));
            });
            hint(ui, "Sets the keypad's backlight while Padgent drives it. When Padgent releases the keypad, its previous brightness returns.");
            ui.checkbox(&mut s.key_glyphs, "Show tool glyphs on working keys");
            ui.horizontal(|ui| {
                ui.label("Live text speed");
                ui.add(egui::Slider::new(&mut s.text_speed, 6.0..=60.0).suffix(" characters/s"));
            });
        });
        card(ui, "Motion", |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut s.motion, Motion::Full, "Full liquid motion");
                ui.selectable_value(&mut s.motion, Motion::Reduced, "Reduced motion");
            });
            hint(ui, "Keys glide when sessions reorder, press like glass and their lights breathe. Reduced motion keeps everything still.");
        });
        if format!("{:?}", c.settings) != before {
            c.revision += 1;
            self.settings_rev += 1;
        }
    }

    fn keys_page(&mut self, ui: &mut Ui) {
        ui.heading("Keys and sessions");
        hint(ui, "Key actions, order, recent sessions.");
        let Ok(mut c) = self.core.lock() else { return };
        let before = format!("{:?}", c.settings);
        let s = &mut c.settings;
        card(ui, "Order", |ui| {
            ui.horizontal(|ui| {
                ui.label("Move keys");
                ui.selectable_value(&mut s.reorder_on, ReorderOn::Turns, "When a turn starts or ends");
                ui.selectable_value(&mut s.reorder_on, ReorderOn::All, "On every change");
            });
            hint(ui, "Working and waiting sessions always keep a key. Keys sort by state: waiting, working, just finished, then idle. A new wait moves up at once.");
        });
        card(ui, "Sessions", |ui| {
            ui.horizontal(|ui| {
                ui.label("Keep recent sessions for");
                ui.add(egui::Slider::new(&mut s.recency_minutes, 5..=720).suffix(" min"));
            });
            ui.horizontal(|ui| {
                ui.label("Dim the keypad after");
                ui.add(egui::Slider::new(&mut s.idle_dim_minutes, 1..=60).suffix(" min idle"));
            });
            hint(ui, "After this long with every session idle, the keypad dims and redraws at 10 fps. The next change brings it back.");
        });
        card(ui, "Key actions", |ui| {
            ui.checkbox(&mut s.key_actions_enabled, "A tap opens the session's app");
            ui.horizontal(|ui| {
                ui.label("Empty key opens");
                ui.add(egui::TextEdit::singleline(&mut s.empty_key_url).hint_text("https://… or app link").desired_width(300.0));
            });
            hint(ui, "Optional. Pressing a key with no session opens this link on this computer. Leave blank to do nothing.");
            hint(ui, "Hold a session key for 0.6 s to archive it until the conversation resumes work.");
        });
        if format!("{:?}", c.settings) != before {
            c.revision += 1;
            self.settings_rev += 1;
        }
    }

    fn menubar_page(&mut self, ui: &mut Ui) {
        ui.heading("Menu bar and hand-over");
        hint(ui, "Indicators, startup and keypad hand-over.");
        let Ok(mut c) = self.core.lock() else { return };
        let before = format!("{:?}", c.settings);
        let hw = c.hardware.clone();
        let in_control = c.in_control;
        let mut toggle = false;
        let mut preview = false;
        {
            let s = &mut c.settings;
            card(ui, "Menu bar", |ui| {
                ui.horizontal(|ui| {
                    ui.label("Waiting indicator");
                    ui.selectable_value(&mut s.menu_indicator, MenuIndicator::Off, "Off");
                    ui.selectable_value(&mut s.menu_indicator, MenuIndicator::Dot, "Dot");
                    ui.selectable_value(&mut s.menu_indicator, MenuIndicator::DotAndCount, "Dot and count");
                });
                hint(ui, "Dot shows a terracotta dot on the menu bar icon. Dot and count adds the number of waiting sessions next to it. The menu previews every session with its state and last message.");
                if ui.button("Preview menu-bar keypad").clicked() { preview = true; }
                ui.checkbox(&mut s.island_enabled, "Show active agents around the camera notch");
                hint(ui, "Click a conversation to open it, a quota to see usage, or the island background to unfold the keypad. The island responds gently to hover.");
                let mut login = s.launch_at_login;
                if ui.checkbox(&mut login, "Launch Padgent at login").changed() {
                    if integrations::set_launch_at_login(login).is_ok() {
                        s.launch_at_login = login;
                    }
                }
            });
            card(ui, "Keypad hand-over", |ui| {
                ui.checkbox(&mut s.auto_take_over_on_launch, "Take over the keypad when Padgent starts");
                hint(ui, "Off: Padgent starts released and Options+ keeps the keypad until you switch.");
                if ui.button(if in_control { "Give Keypad to Options+" } else { "Take Over Keypad" }).clicked() {
                    toggle = true;
                }
                hint(ui, "While Padgent is in control, the keypad shows your sessions and your Options+ profile for it pauses. To hand it back from the keypad itself, hold both page buttons for 1.5 s. Quitting always gives it back.");
            });
            card(ui, "Physical keypad (experimental)", |ui| {
                ui.checkbox(&mut s.drive_physical_keypad, "Drive the MX Creative Keypad over USB");
                let status = match &hw {
                    Hardware::Off => "Off: the keypad is shown on screen only.".to_string(),
                    Hardware::NoKeypad => "No keypad connected. Plug in the MX Creative Keypad with its USB-C cable.".into(),
                    Hardware::Connected => "Keypad connected; it stays with Options+ until you take it over.".into(),
                    Hardware::Driving => "Padgent has the keypad.".into(),
                    Hardware::Problem(e) => e.clone(),
                };
                hint(ui, &status);
                hint(ui, "Uses the keypad's protocol: exclusive access (Options+ pauses), the page buttons, key presses and key pictures. macOS may ask to allow Input Monitoring.");
            });
        }
        if toggle {
            if in_control { c.release() } else { c.take_over() }
        }
        card(ui, "Settings file", |ui| {
            ui.horizontal(|ui| {
                if ui.button("Export settings…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().set_file_name("padgent-settings.json").save_file() {
                        let ok = std::fs::write(&path, serde_json::to_string_pretty(&c.settings).unwrap_or_default()).is_ok();
                        c.notify(if ok { "Settings exported." } else { "Export failed. Nothing was saved." });
                    }
                }
                if ui.button("Import settings…").clicked() {
                    if let Some(path) = rfd::FileDialog::new().add_filter("Settings", &["json"]).pick_file() {
                        match std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Settings>(&t).ok()) {
                            Some(s) if s.format == "agentpad.settings" && s.version <= 1 => {
                                c.settings = s;
                                c.notify("Settings imported.");
                            }
                            _ => c.notify("This file is not a Padgent settings document."),
                        }
                    }
                }
            });
        });
        if format!("{:?}", c.settings) != before {
            c.revision += 1;
            self.settings_rev += 1;
        }
        drop(c);
        if preview {
            if let Some(rect) = self.tray.as_ref().and_then(|tray| tray.rect()) {
                self.open_popup(rect);
                ui.ctx().request_repaint();
            }
        }
    }

    fn about_page(&mut self, ui: &mut Ui) {
        ui.heading("About");
        card(ui, "Padgent", |ui| {
            hint(ui, "An open-source Rust app for agent sessions on a keypad and in the menu bar. It is lightweight: it redraws only while something moves, and drops to 10 fps when everything is idle.");
            hint(ui, "Not affiliated with Logitech, Anthropic or OpenAI. Logitech, MX and Options+ are trademarks of Logitech. Claude is a trademark of Anthropic. Codex and ChatGPT are trademarks of OpenAI.");
            let c = self.core.lock();
            if let Ok(c) = c {
                hint(ui, &c.server_status);
            }
        });
    }
}

fn request_screenshot(ctx: &egui::Context, path: String, done: Arc<AtomicBool>, failed: Arc<AtomicBool>) {
    ctx.request_screenshot(move |img| {
        let [w, h] = img.size;
        let mut buf = Vec::with_capacity(w * h * 3);
        for p in &img.pixels {
            let [r, g, b, a] = p.to_srgba_unmultiplied();
            let a = a as f32 / 255.0;
            let bg = [14.0, 15.0, 22.0];
            buf.push((r as f32 * a + bg[0] * (1.0 - a)) as u8);
            buf.push((g as f32 * a + bg[1] * (1.0 - a)) as u8);
            buf.push((b as f32 * a + bg[2] * (1.0 - a)) as u8);
        }
        let result = (|| -> Result<(), Box<dyn std::error::Error>> {
            if let Some(dir) = std::path::Path::new(&path).parent().filter(|dir| !dir.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir)?;
            }
            image::save_buffer(&path, &buf, w as u32, h as u32, image::ExtendedColorType::Rgb8)?;
            Ok(())
        })();
        match result {
            Ok(()) => eprintln!("Saved screenshot {w}x{h} to {path}"),
            Err(err) => {
                eprintln!("Failed to save screenshot {w}x{h} to {path}: {err}");
                failed.store(true, Ordering::SeqCst);
            }
        }
        done.store(true, Ordering::SeqCst);
    });
}

impl eframe::App for App {
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        [0.0, 0.0, 0.0, 0.0]
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.run_actions(ctx);
        let clicks = self.tray_clicks.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default();
        for rect in clicks {
            if self.popup_open {
                self.popup_open = false;
                mac::log("tray keypad closed");
            } else {
                self.open_popup(rect);
            }
        }
        // Immediate child windows share this context's texture atlas. A hidden root
        // still needs a paint pass so atlas updates reach the GPU for those windows.
        if self.screenshot.is_none() {
            let needs_children = self.popup_open || self.core.lock().map(|c|
                c.settings.island_enabled && (!island_agents(&c).is_empty() || !island_quotas(&c).is_empty())
            ).unwrap_or(false);
            if needs_children { ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden); }
            if !self.popup_open { self.cancel_popup_inputs(); }
        }
        if ctx.input(|i| i.viewport().close_requested()) && self.screenshot.is_none() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        if let (Some(tray), Ok(c)) = (&mut self.tray, self.core.lock()) {
            tray.update(&c);
        }
        if self.settings_rev != self.saved_rev {
            self.saved_rev = self.settings_rev;
            if self.screenshot.is_none() {
                if let Ok(c) = self.core.lock() {
                    c.settings.save();
                }
            }
        }
        // Screenshot mode: draw about 120 frames, save the window, quit.
        if let Some(path) = self.screenshot.clone() {
            self.frames += 1;
            if self.frames >= 120 && !self.shot_requested {
                self.shot_requested = true;
                request_screenshot(ctx, path.clone(), self.shot_done.clone(), self.shot_failed.clone());
            }
            if self.frames >= 600 && !self.shot_done.load(Ordering::SeqCst) {
                eprintln!("Screenshot timed out: no rendered image received for {path}");
                self.shot_failed.store(true, Ordering::SeqCst);
                self.shot_done.store(true, Ordering::SeqCst);
            }
            if self.shot_done.load(Ordering::SeqCst) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ctx.request_repaint();
        } else {
            let (fps, animate) = self
                .core
                .lock()
                .map(|c| (c.fps(), c.settings.motion == Motion::Full || c.visible().iter().any(|s| s.activity.animated())))
                .unwrap_or((30, true));
            if animate {
                ctx.request_repaint_after(std::time::Duration::from_millis(1000 / fps as u64));
            } else {
                ctx.request_repaint_after(std::time::Duration::from_millis(500));
            }
        }
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if self.screenshot.is_none() {
            let ctx = ui.ctx().clone();
            self.island(&ctx);
            if self.popup_open && !self.popup_from_island { self.popup(&ctx); }
            if !self.popup_open { self.cancel_popup_inputs(); }
            if !ctx.input(|i| i.viewport().visible().unwrap_or(true)) { return; }
        }
        if self.screenshot_island {
            let pill = self.core.lock().ok().map(|c| {
                Self::island_contents(ui, &island_agents(&c), &island_quotas(&c), c.settings.brightness, c.settings.motion == Motion::Reduced, 180.0, 32.0, self.screenshot_island_expanded).1
            });
            if let (true, Some(pill)) = (self.screenshot_island_expanded, pill) {
                let ctx = ui.ctx().clone();
                self.island_body(ui, &ctx, pill, 32.0, 1.0);
            }
            return;
        }
        if self.screenshot_menu_bar {
            let ctx = ui.ctx().clone();
            self.popup_contents(ui, &ctx);
            return;
        }
        let full = ui.max_rect();
        let t = model::now();
        let reduced = self.core.lock().map(|c| c.settings.motion == Motion::Reduced).unwrap_or(false);
        paint::ambient_background(ui.painter(), full, t, self.screenshot.is_some(), reduced);
        let side = Rect::from_min_size(full.min + vec2(12.0, 12.0), vec2(232.0, full.height() - 24.0));
        self.sidebar(ui, side);
        let content = Rect::from_min_max(pos2(side.right() + 24.0, full.top() + 30.0), pos2(full.right() - 24.0, full.bottom() - 20.0));
        if self.page == Page::Keypad {
            self.keypad_page(ui, content);
            return;
        }
        let mut child = ui.new_child(UiBuilder::new().max_rect(content).layout(egui::Layout::top_down(egui::Align::LEFT)));
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
            ui.set_max_width(content.width().min(900.0));
            match self.page {
                Page::Agents => self.agents_page(ui),
                Page::Look => self.look_page(ui),
                Page::Keys => self.keys_page(ui),
                Page::MenuBar => self.menubar_page(ui),
                Page::About => self.about_page(ui),
                Page::Keypad => {}
            }
            ui.add_space(20.0);
        });
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if self.screenshot.is_none() {
            if let Ok(c) = self.core.lock() {
                c.settings.save();
            }
            hardware::shutdown(&self.core);
        }
    }
}

fn main() -> eframe::Result {
    let args: Vec<String> = std::env::args().collect();
    // `--icon <file.png> [size]` draws the app icon (used when packaging Padgent.app).
    if let Some(i) = args.iter().position(|a| a == "--icon") {
        let path = args.get(i + 1).cloned().unwrap_or_else(|| "icon.png".into());
        let size = args.get(i + 2).and_then(|v| v.parse().ok()).unwrap_or(1024);
        if let Err(e) = icon::save(&path, size) {
            eprintln!("icon: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }
    let menu_screenshot = args.iter().position(|a| a == "--screenshot-menu-bar").and_then(|i| args.get(i + 1).cloned());
    let screenshot_menu_bar = menu_screenshot.is_some();
    let expanded_screenshot = args.iter().position(|a| a == "--screenshot-island-expanded").and_then(|i| args.get(i + 1).cloned());
    let screenshot_island_expanded = expanded_screenshot.is_some();
    let island_screenshot = expanded_screenshot.or_else(|| args.iter().position(|a| a == "--screenshot-island").and_then(|i| args.get(i + 1).cloned()));
    let screenshot_island = island_screenshot.is_some();
    let screenshot = menu_screenshot.or(island_screenshot).or_else(|| args.iter().position(|a| a == "--screenshot").and_then(|i| args.get(i + 1).cloned()));

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Padgent")
        .with_inner_size([1240.0, 780.0])
        .with_min_inner_size([980.0, 640.0])
        .with_transparent(true)
        .with_fullsize_content_view(true)
        .with_title_shown(false)
        .with_titlebar_shown(false);
    if screenshot_island {
        viewport = egui::ViewportBuilder::default().with_title("Padgent Island preview")
            .with_inner_size([500.0, if screenshot_island_expanded { 600.0 } else { 36.0 }]).with_resizable(false).with_decorations(false).with_transparent(true);
    } else if screenshot_menu_bar {
        viewport = egui::ViewportBuilder::default().with_title("Padgent keypad")
            .with_inner_size([400.0, 480.0]).with_resizable(false).with_decorations(false);
    }
    let mut options = eframe::NativeOptions::default();
    if screenshot.is_some() {
        viewport = viewport.with_active(false);
        #[cfg(target_os = "windows")]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
        #[cfg(target_os = "macos")]
        {
            options.event_loop_builder = Some(Box::new(|b| {
                use winit::platform::macos::EventLoopBuilderExtMacOS;
                b.with_activate_ignoring_other_apps(false);
            }));
        }
        options.run_and_return = true;
    }
    options.viewport = viewport;
    let shot = screenshot.clone();
    let shot_failed = Arc::new(AtomicBool::new(false));
    let app_shot_failed = shot_failed.clone();
    let result = eframe::run_native("Padgent", options, Box::new(move |cc| Ok(Box::new(App::new(cc, shot, screenshot_menu_bar, screenshot_island, screenshot_island_expanded, app_shot_failed)))));
    if shot_failed.load(Ordering::SeqCst) { std::process::exit(1); }
    result
}

#[cfg(test)]
mod island_tests {
    use super::*;

    fn add_session(core: &mut Core, agent: &str, id: &str, state: &str) {
        core.apply(model::Incoming {
            agent: agent.into(),
            event: None,
            body: serde_json::json!({"session": id, "state": state, "title": format!("title-{id}")}),
        });
    }

    #[test]
    fn compact_island_filters_inactive_archived_and_disabled_sessions_and_keeps_ids() {
        let mut settings = Settings::default();
        settings.disabled_agents.push("disabled-agent".into());
        let mut core = Core::new(settings);
        add_session(&mut core, "codex", "working-1", "thinking");
        add_session(&mut core, "codex", "waiting-1", "waiting_permission");
        add_session(&mut core, "codex", "waiting-2", "waiting_question");
        add_session(&mut core, "codex", "idle", "idle");
        add_session(&mut core, "codex", "archived", "thinking");
        add_session(&mut core, "disabled-agent", "disabled", "waiting_question");
        core.sessions.iter_mut().find(|s| s.id == "archived").unwrap().archived = true;

        let active = island_agents(&core);
        assert_eq!(active.len(), 3);
        assert!(active.iter().all(|item| item.agent == "codex"));
        assert!(active.iter().all(|item| matches!(item.activity.group(), Group::Waiting | Group::Working)));
        assert!(!active.iter().any(|item| ["idle", "archived", "disabled"].contains(&item.session.as_str())));

        // The renderer displays the first two, so waits must occupy both compact slots first.
        assert_eq!(active.iter().take(2).filter(|item| item.activity.group() == Group::Waiting).count(), 2);
        assert!(active.iter().any(|item| item.agent == "codex" && item.session == "working-1" && item.title == "title-working-1"));
        assert!(active.iter().any(|item| item.agent == "codex" && item.session == "waiting-1" && item.title == "title-waiting-1"));
        assert!(active.iter().any(|item| item.agent == "codex" && item.session == "waiting-2" && item.title == "title-waiting-2"));
    }

    fn quota(provider: &str, window: &str, remaining: Option<f64>, updated: Option<String>, reset: Option<i64>) -> quota::Quota {
        quota::Quota {
            provider: provider.into(), window: window.into(), remaining, reset,
            reset_text: None, updated, source: "test".into(),
        }
    }

    #[test]
    fn compact_island_shows_only_fresh_observed_session_and_weekly_quota_values() {
        let mut core = Core::new(Settings::default());
        let now = chrono::Utc::now();
        core.quotas = vec![
            quota("Codex", "Session", Some(74.0), Some(now.to_rfc3339()), None),
            quota("Codex", "Weekly", Some(31.0), Some(now.to_rfc3339()), Some(now.timestamp() - 1)),
            quota("Codex", "Monthly", Some(88.0), Some(now.to_rfc3339()), None),
            quota("Claude", "Weekly", None, Some(now.to_rfc3339()), None),
        ];

        let rows = island_quotas(&core);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].provider, "Codex");
        assert_eq!(rows[0].session, Some(74.0));
        assert_eq!(rows[0].weekly, None);
    }

    #[test]
    fn compact_island_hides_quota_rows_without_fresh_remaining_values() {
        let mut core = Core::new(Settings::default());
        core.quotas = vec![
            quota("Codex", "Session", Some(50.0), Some("2020-01-01T00:00:00Z".into()), None),
            quota("Codex", "Weekly", None, Some(chrono::Utc::now().to_rfc3339()), None),
        ];
        assert!(island_quotas(&core).is_empty());
    }

    #[test]
    fn completed_session_survives_decay_and_expiry_until_open_then_hides_everywhere() {
        let mut core = Core::new(Settings::default());
        add_session(&mut core, "codex", "finished", "done");
        let t = model::now();
        let finished = core.sessions.iter_mut().find(|s| s.id == "finished").unwrap();
        finished.since = t - model::DONE_DECAY_S - 5.0;
        finished.updated = t - model::REMOVE_AFTER_S - 5.0;

        core.tick();
        assert_eq!(core.sessions[0].activity, model::Activity::Idle);
        assert!(core.sessions[0].pending_attention);
        assert!(core.visible().iter().any(|s| s.id == "finished"));
        assert!(matches!(&core.faces()[0], model::KeyFace::Session(face) if face.key == "codex:finished"));
        assert_eq!(island_agents(&core).iter().map(|s| s.session.as_str()).collect::<Vec<_>>(), ["finished"]);

        let captured = core.acknowledge_open("codex", "finished").expect("the exact session is still available to open");
        assert_eq!(captured.id, "finished");
        assert!(core.sessions[0].archived);
        assert!(!core.sessions[0].pending_attention);
        assert!(core.visible().is_empty());
        assert!(core.faces().iter().all(|face| !matches!(face, model::KeyFace::Session(_))));
        assert!(island_agents(&core).is_empty());
    }

    #[test]
    fn unread_inactive_survives_expiry_open_hides_stale_polls_and_work_resumes_it() {
        let mut core = Core::new(Settings::default());
        add_session(&mut core, "codex", "reply", "idle");
        core.apply(model::Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"reply", "state":"idle", "_unread":4}),
        });
        let t = model::now();
        core.sessions.iter_mut().find(|s| s.id == "reply").unwrap().updated = t - model::REMOVE_AFTER_S - 10.0;

        core.tick();
        assert_eq!(core.sessions.len(), 1, "pending unread keeps an otherwise expired record");
        assert!(core.sessions[0].pending_attention);
        assert_eq!(core.sessions[0].unread, 4);
        assert_eq!(island_agents(&core).iter().map(|s| s.session.as_str()).collect::<Vec<_>>(), ["reply"]);
        assert!(matches!(&core.faces()[0], model::KeyFace::Session(face) if face.key == "codex:reply"));

        core.acknowledge_open("codex", "reply").unwrap();
        assert!(core.sessions[0].archived);
        assert_eq!(core.sessions[0].unread, 0);
        assert!(core.visible().is_empty());
        assert!(island_agents(&core).is_empty());

        // A delayed title/read-receipt poll cannot restore the acknowledged inactive item.
        core.apply(model::Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"reply", "state":"idle", "title":"stale title", "_unread":4}),
        });
        assert!(core.sessions[0].archived);
        assert!(core.visible().is_empty());
        assert!(core.faces().iter().all(|face| !matches!(face, model::KeyFace::Session(_))));
        assert!(island_agents(&core).is_empty());

        // A genuinely active update reopens the record and restores it to both views.
        core.apply(model::Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"reply", "state":"thinking", "title":"resumed"}),
        });
        assert!(!core.sessions[0].archived);
        assert!(core.sessions[0].pending_attention, "the unread count remains until a read/open acknowledgement");
        assert!(core.visible().iter().any(|s| s.id == "reply"));
        assert!(island_agents(&core).iter().any(|s| s.agent == "codex" && s.session == "reply"));
    }

    #[test]
    fn opening_working_session_does_not_archive_or_remove_it() {
        let mut core = Core::new(Settings::default());
        add_session(&mut core, "codex", "working", "thinking");
        assert!(core.acknowledge_open("codex", "working").is_some());
        assert!(!core.sessions[0].archived);
        assert!(!core.sessions[0].pending_attention);
        assert!(core.visible().iter().any(|s| s.id == "working"));
        assert!(island_agents(&core).iter().any(|s| s.session == "working"));
    }

    #[test]
    fn captured_completed_tile_is_acknowledged_after_reorder_without_opening_new_slot() {
        let mut core = Core::new(Settings::default());
        core.in_control = true;
        add_session(&mut core, "codex", "finished", "done");
        add_session(&mut core, "codex", "other", "idle");

        let target = model::target_for_face(&core.faces()[0], core.quota_view);
        assert_eq!(target, model::KeyPressTarget::Session("codex:finished".into()));
        core.key_down_with_target(0, target);
        core.apply(model::Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"other", "state":"waiting_question"}),
        });
        assert!(matches!(&core.faces()[0], model::KeyFace::Session(face) if face.key == "codex:other"));

        core.key_up(0);
        assert!(matches!(core.actions.as_slice(), [model::Action::Open { agent, session }] if agent == "codex" && session == "finished"));
        assert!(core.sessions.iter().find(|s| s.id == "finished").unwrap().archived);
        assert!(!core.sessions.iter().find(|s| s.id == "other").unwrap().archived);
        assert!(island_agents(&core).iter().all(|s| s.session != "finished"));
    }
}
