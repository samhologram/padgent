//! Menu bar icon with a live preview of every agent session.

use crate::model::{Activity, Core, Group, MenuIndicator};
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct Tray {
    icon: TrayIcon,
    signature: Option<(String, bool, usize)>,
    rows: Vec<MenuRow>,
    rebuilt: std::time::Instant,
    last_indicator: Option<(usize, MenuIndicator)>,
}

#[derive(PartialEq, Eq)]
struct MenuRow {
    key: String,
    heading: String,
    preview: String,
}

fn emoji(g: Group) -> &'static str {
    match g {
        Group::Waiting => "🟠",
        Group::Working => "🔵",
        Group::Done => "🟢",
        Group::Idle => "⚪️",
        Group::Error => "🔴",
        Group::Offline => "⚫️",
    }
}

fn indicator_state(waiting: usize, indicator: MenuIndicator) -> (bool, Option<String>) {
    let dot = waiting > 0 && indicator != MenuIndicator::Off;
    let title = if waiting > 0 && indicator == MenuIndicator::DotAndCount { Some(waiting.to_string()) } else { None };
    (dot, title)
}

fn preview_text(text: &str, text_at: f64, unread: u32, activity: Activity, now: f64) -> String {
    let recent = now - text_at <= crate::model::RECENT_TEXT_S;
    let line = crate::model::key_text(if recent { text } else { "" }, unread, activity);
    let text: String = line.chars().take(64).collect();
    if line.chars().count() > 64 { format!("{text}…") } else { text }
}

/// 36×36 icon: a ring with an orb; with `dot`, a terracotta dot shows waiting sessions.
fn icon(dot: bool) -> Icon {
    let n = 36usize;
    let mut px = vec![0u8; n * n * 4];
    let c = n as f32 / 2.0;
    for y in 0..n {
        for x in 0..n {
            let (dx, dy) = (x as f32 + 0.5 - c, y as f32 + 0.5 - c);
            let d = (dx * dx + dy * dy).sqrt();
            let ring = (1.6 - (d - 13.0).abs()).clamp(0.0, 1.0);
            let orb = (6.6 - d).clamp(0.0, 1.0);
            let a = ring.max(orb);
            let i = (y * n + x) * 4;
            let shade = if dot { 120 } else { 0 };
            px[i] = shade;
            px[i + 1] = shade;
            px[i + 2] = shade;
            px[i + 3] = (a * 255.0) as u8;
            if dot {
                let (ex, ey) = (x as f32 + 0.5 - 29.0, y as f32 + 0.5 - 7.0);
                let dd = (ex * ex + ey * ey).sqrt();
                let da = (6.5 - dd).clamp(0.0, 1.0);
                if da > 0.0 {
                    px[i] = 226;
                    px[i + 1] = 114;
                    px[i + 2] = 76;
                    px[i + 3] = ((da.max(a)) * 255.0) as u8;
                }
            }
        }
    }
    Icon::from_rgba(px, n as u32, n as u32).expect("icon")
}

impl Tray {
    pub fn new() -> Option<Tray> {
        let icon = TrayIconBuilder::new()
            .with_icon_templated(icon(false))
            .with_tooltip("AgentPad")
            .with_menu(Box::new(Menu::new()))
            .build()
            .ok()?;
        Some(Tray { icon, signature: None, rows: Vec::new(), rebuilt: std::time::Instant::now(), last_indicator: None })
    }

    pub fn update(&mut self, core: &Core) {
        let vis = core.visible();
        let waiting = core.waiting_count();
        let indicator = core.settings.menu_indicator;
        if self.last_indicator != Some((waiting, indicator)) {
            let (show_dot, title) = indicator_state(waiting, indicator);
            if show_dot {
                let _ = self.icon.set_icon(Some(icon(true)));
            } else {
                let _ = self.icon.set_icon_templated(Some(icon(false)));
            }
            self.icon.set_title(title);
            self.last_indicator = Some((waiting, indicator));
        }

        let now = crate::model::now();
        let rows: Vec<_> = vis.iter().take(12).map(|s| {
            let agent = core.agent(&s.agent).map(|a| a.name.as_str()).unwrap_or(&s.agent);
            let heading = format!("{}  {} — {} · {}", emoji(s.activity.group()), s.title, agent, s.activity.label());
            let preview = preview_text(&s.text, s.text_at, s.unread, s.activity, now);
            MenuRow { key: s.key(), heading, preview }
        }).collect();
        let sig = (core.control_label().to_string(), core.in_control, vis.len());
        let structural = self.signature.as_ref() != Some(&sig)
            || self.rows.len() != rows.len()
            || self.rows.iter().zip(&rows).any(|(old, new)| old.key != new.key || old.heading != new.heading);
        let text_only = !structural && self.rows.iter().zip(&rows).any(|(old, new)| old.preview != new.preview);
        if !structural && !(text_only && self.rebuilt.elapsed().as_secs_f64() >= 2.0) {
            return;
        }
        self.signature = Some(sig);
        self.rows = rows;
        self.rebuilt = std::time::Instant::now();

        let menu = Menu::new();
        let _ = menu.append(&MenuItem::new(core.control_label(), false, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        if vis.is_empty() {
            let _ = menu.append(&MenuItem::new("No agent sessions yet", false, None));
        }
        for row in &self.rows {
            let _ = menu.append(&MenuItem::with_id(format!("open:{}", row.key), &row.heading, true, None));
            if !row.preview.is_empty() {
                let _ = menu.append(&MenuItem::new(format!("        {}", row.preview), false, None));
            }
        }
        if vis.len() > 12 {
            let _ = menu.append(&MenuItem::new(format!("+{} more on the keypad", vis.len() - 12), false, None));
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("show", "Show Keypad", true, None));
        let toggle = if core.in_control { "Switch Keypad to Options+" } else { "Take Over Keypad" };
        let _ = menu.append(&MenuItem::with_id("toggle", toggle, true, None));
        let _ = menu.append(&MenuItem::with_id("settings", "Settings…", true, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&MenuItem::with_id("quit", "Quit AgentPad", true, None));
        self.icon.set_menu(Some(Box::new(menu)));
    }
}

#[cfg(test)]
mod tests {
    use super::{indicator_state, preview_text};
    use crate::model::{Activity, MenuIndicator, RECENT_TEXT_S};

    #[test]
    fn indicator_state_tracks_setting_changes_at_unchanged_waiting_count() {
        assert_eq!(indicator_state(2, MenuIndicator::Off), (false, None));
        assert_eq!(indicator_state(2, MenuIndicator::Dot), (true, None));
        assert_eq!(indicator_state(2, MenuIndicator::DotAndCount), (true, Some("2".into())));
    }

    #[test]
    fn indicator_state_hides_dot_and_count_when_nobody_is_waiting() {
        assert_eq!(indicator_state(0, MenuIndicator::Dot), (false, None));
        assert_eq!(indicator_state(0, MenuIndicator::DotAndCount), (false, None));
    }

    #[test]
    fn preview_expires_recent_text_and_falls_back_to_unread_count() {
        let now = 1_000.0;
        assert_eq!(preview_text("hello", now - RECENT_TEXT_S, 0, Activity::Idle, now), "hello");
        assert_eq!(preview_text("hello", now - RECENT_TEXT_S - 0.01, 3, Activity::Idle, now), "3 unread");
    }

    #[test]
    fn preview_reflects_unread_changes_without_new_text() {
        assert_eq!(preview_text("", 0.0, 1, Activity::Idle, 1_000.0), "1 unread");
        assert_eq!(preview_text("", 0.0, 4, Activity::Idle, 1_000.0), "4 unread");
    }

    #[test]
    fn preview_truncation_uses_sixty_four_characters_and_adds_ellipsis_only_when_needed() {
        let exact = "a".repeat(64);
        let over = "b".repeat(65);
        assert_eq!(preview_text(&exact, 1_000.0, 0, Activity::Idle, 1_000.0), exact);
        assert_eq!(preview_text(&over, 1_000.0, 0, Activity::Idle, 1_000.0), format!("{}…", "b".repeat(64)));
    }

    #[test]
    fn preview_truncation_preserves_unicode_characters() {
        let text = "é".repeat(65);
        assert_eq!(preview_text(&text, 1_000.0, 0, Activity::Idle, 1_000.0), format!("{}…", "é".repeat(64)));
    }
}
