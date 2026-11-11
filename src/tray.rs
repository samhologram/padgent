//! Menu bar icon with a live preview of every agent session.

use crate::model::{Core, Group, MenuIndicator};
use tray_icon::menu::{Menu, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

pub struct Tray {
    icon: TrayIcon,
    signature: String,
    last_waiting: Option<usize>,
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
            .with_icon(icon(false))
            .with_icon_as_template(true)
            .with_tooltip("AgentPad")
            .with_menu(Box::new(Menu::new()))
            .build()
            .ok()?;
        Some(Tray { icon, signature: String::new(), last_waiting: None })
    }

    pub fn update(&mut self, core: &Core) {
        let vis = core.visible();
        let waiting = core.waiting_count();
        let indicator = core.settings.menu_indicator;
        if self.last_waiting != Some(waiting) {
            let show_dot = waiting > 0 && indicator != MenuIndicator::Off;
            if show_dot {
                let _ = self.icon.set_icon(Some(icon(true)));
                self.icon.set_icon_as_template(false);
            } else {
                let _ = self.icon.set_icon(Some(icon(false)));
                self.icon.set_icon_as_template(true);
            }
            let title = if indicator == MenuIndicator::DotAndCount && waiting > 0 { Some(format!("{waiting}")) } else { None };
            self.icon.set_title(title);
            self.last_waiting = Some(waiting);
        }

        let mut sig = format!("{}|{}|", core.control_label(), core.in_control);
        for s in vis.iter().take(12) {
            let text: String = s.text.chars().take(60).collect();
            sig.push_str(&format!("{}:{:?}:{}:{};", s.key(), s.activity, s.title, text));
        }
        if sig == self.signature {
            return;
        }
        self.signature = sig;

        let menu = Menu::new();
        let _ = menu.append(&MenuItem::new(core.control_label(), false, None));
        let _ = menu.append(&PredefinedMenuItem::separator());
        if vis.is_empty() {
            let _ = menu.append(&MenuItem::new("No agent sessions yet", false, None));
        }
        for s in vis.iter().take(12) {
            let agent = core.agent(&s.agent).map(|a| a.name.clone()).unwrap_or_else(|| s.agent.clone());
            let line = format!("{}  {} — {} · {}", emoji(s.activity.group()), s.title, agent, s.activity.label());
            let _ = menu.append(&MenuItem::with_id(format!("open:{}", s.key()), line, true, None));
            if !s.text.is_empty() {
                let text: String = s.text.chars().take(64).collect();
                let more = if s.text.chars().count() > 64 { "…" } else { "" };
                let _ = menu.append(&MenuItem::new(format!("        {text}{more}"), false, None));
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
