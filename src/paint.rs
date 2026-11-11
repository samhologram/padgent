//! Liquid-glass drawing for the on-screen keypad and panels.

use crate::colour::Rgb;
use crate::model::{Activity, Group, HubFace, KeyFace, SessionFace};
use eframe::egui::{
    self, Align2, Color32, FontId, Mesh, Painter, Pos2, Rect, Shape, Stroke, StrokeKind, pos2, vec2,
};
use eframe::epaint::{CornerRadius, Shadow};
use std::f32::consts::{PI, TAU};

pub fn c32(c: Rgb) -> Color32 {
    Color32::from_rgb(c.0, c.1, c.2)
}

pub fn with_alpha(c: Color32, a: f32) -> Color32 {
    let [r, g, b, _] = c.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

pub fn white(a: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(255, 255, 255, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

/// Outline of a rounded rectangle (clockwise from the top-left corner's start).
pub fn rounded_points(r: Rect, radius: f32, segs: usize) -> Vec<Pos2> {
    let rad = radius.min(r.width() / 2.0).min(r.height() / 2.0);
    let corners = [
        (pos2(r.right() - rad, r.top() + rad), -PI / 2.0),
        (pos2(r.right() - rad, r.bottom() - rad), 0.0),
        (pos2(r.left() + rad, r.bottom() - rad), PI / 2.0),
        (pos2(r.left() + rad, r.top() + rad), PI),
    ];
    let mut pts = Vec::with_capacity(segs * 4 + 4);
    for (c, a0) in corners {
        for i in 0..=segs {
            let a = a0 + (i as f32 / segs as f32) * (PI / 2.0);
            pts.push(c + vec2(a.cos(), a.sin()) * rad);
        }
    }
    pts
}

/// Rounded rectangle filled with a vertical gradient.
pub fn gradient_rounded(p: &Painter, r: Rect, radius: f32, top: Color32, bottom: Color32) {
    let pts = rounded_points(r, radius, 8);
    let mut mesh = Mesh::default();
    let col = |y: f32| {
        let t = ((y - r.top()) / r.height().max(1.0)).clamp(0.0, 1.0);
        lerp_color(top, bottom, t)
    };
    mesh.colored_vertex(r.center(), col(r.center().y));
    for q in &pts {
        mesh.colored_vertex(*q, col(q.y));
    }
    let n = pts.len() as u32;
    for i in 0..n {
        mesh.add_triangle(0, 1 + i, 1 + (i + 1) % n);
    }
    p.add(Shape::mesh(mesh));
}

pub fn lerp_color(a: Color32, b: Color32, t: f32) -> Color32 {
    let a = a.to_array();
    let b = b.to_array();
    let m = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgba_premultiplied(m(a[0], b[0]), m(a[1], b[1]), m(a[2], b[2]), m(a[3], b[3]))
}

/// Soft radial light.
pub fn glow(p: &Painter, c: Pos2, radius: f32, colour: Color32, strength: f32) {
    if strength <= 0.01 || radius <= 0.5 {
        return;
    }
    let rings = [(0.0, 1.0), (0.25, 0.62), (0.5, 0.3), (0.75, 0.1), (1.0, 0.0)];
    let segs = 40;
    let mut mesh = Mesh::default();
    for (ri, &(f, a)) in rings.iter().enumerate() {
        for s in 0..segs {
            let ang = s as f32 / segs as f32 * TAU;
            let q = if ri == 0 { c } else { c + vec2(ang.cos(), ang.sin()) * radius * f };
            mesh.colored_vertex(q, with_alpha(colour, a * strength).gamma_multiply(1.0));
        }
    }
    for ri in 0..rings.len() as u32 - 1 {
        for s in 0..segs as u32 {
            let a = ri * segs as u32 + s;
            let b = ri * segs as u32 + (s + 1) % segs as u32;
            let c2 = a + segs as u32;
            let d = b + segs as u32;
            mesh.add_triangle(a, b, c2);
            mesh.add_triangle(b, d, c2);
        }
    }
    p.add(Shape::mesh(mesh));
}

/// Frosted glass slab: shadow, gradient body, hairline border and a top specular edge.
pub fn glass(p: &Painter, r: Rect, radius: f32, tint: Color32, strength: f32, shadow: bool) {
    if shadow {
        let sh = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(70) };
        p.add(sh.as_shape(r, CornerRadius::same(radius.min(255.0) as u8)));
    }
    let top = lerp_color(white(0.16 * strength), with_alpha(tint, 0.25 * strength), 0.25);
    let bottom = lerp_color(white(0.05 * strength), with_alpha(tint, 0.12 * strength), 0.4);
    gradient_rounded(p, r, radius, top, bottom);
    p.rect_stroke(r, CornerRadius::same(radius.min(255.0) as u8), Stroke::new(1.0, white(0.14)), StrokeKind::Inside);
    // Specular highlight along the top edge, brightest at the top-left.
    let pts = rounded_points(r.shrink(1.0), radius - 1.0, 8);
    let n = pts.len();
    let start = n * 3 / 4; // top-left corner
    for i in 0..(n / 2) {
        let a = pts[(start + i) % n];
        let b = pts[(start + i + 1) % n];
        let fade = 1.0 - i as f32 / (n as f32 / 2.0);
        p.line_segment([a, b], Stroke::new(1.2, white(0.42 * fade * strength)));
    }
}

pub fn arc(p: &Painter, c: Pos2, radius: f32, a0: f32, sweep: f32, stroke: Stroke) {
    let steps = ((sweep.abs() / TAU) * 64.0).max(6.0) as usize;
    let pts: Vec<Pos2> = (0..=steps)
        .map(|i| {
            let a = a0 + sweep * i as f32 / steps as f32;
            c + vec2(a.cos(), a.sin()) * radius
        })
        .collect();
    p.add(Shape::line(pts, stroke));
}

fn text_block(p: &Painter, pos: Pos2, width: f32, text: &str, size: f32, colour: Color32, rows: usize, center: bool) -> f32 {
    let mut job = egui::text::LayoutJob::simple(text.to_string(), FontId::proportional(size), colour, width);
    job.wrap.max_rows = rows;
    job.wrap.break_anywhere = false;
    job.wrap.overflow_character = Some('…');
    if center {
        job.halign = egui::Align::Center;
    }
    let galley = p.layout_job(job);
    let h = galley.size().y;
    let x = if center { pos.x } else { pos.x };
    p.galley(pos2(x, pos.y), galley, colour);
    h
}

pub struct KeyCx {
    pub t: f64,
    pub hover: bool,
    pub pressed: bool,
    pub hold: f32,
    pub reduced_motion: bool,
    pub brightness: f32,
    pub glyphs: bool,
    pub waiting: Color32,
}

pub fn paint_key(p: &Painter, r: Rect, face: &KeyFace, cx: &KeyCx) {
    let k = r.width();
    let rad = k * 0.2;
    let t = cx.t;
    let wave = |period: f64| if cx.reduced_motion { 0.5 } else { (0.5 + 0.5 * ((t % 1000.0) * TAU as f64 / period).sin()) as f32 };

    // Waiting halo behind the key.
    if let KeyFace::Session(f) = face {
        if f.activity.group() == Group::Waiting {
            let b = 0.35 + 0.45 * wave(1.6);
            glow(p, r.center(), k * 0.85, cx.waiting, b * 0.55);
        }
    }

    let tint = match face {
        KeyFace::Session(f) => c32(f.glow),
        KeyFace::FocusText { colour, .. } => c32(*colour),
        _ => white(0.5),
    };
    let strength = match face {
        KeyFace::Released | KeyFace::Empty => 0.55,
        _ => 1.0,
    };
    if matches!(face, KeyFace::Session(_)) {
        // Dark base under the light; the glass veil is drawn over the light in paint_session.
        let sh = Shadow { offset: [0, 10], blur: 28, spread: 0, color: Color32::from_black_alpha(70) };
        p.add(sh.as_shape(r, CornerRadius::same(rad.min(255.0) as u8)));
        gradient_rounded(p, r, rad, Color32::from_rgba_unmultiplied(26, 27, 36, 235), Color32::from_rgba_unmultiplied(12, 13, 18, 245));
    } else {
        glass(p, r, rad, tint, strength, !matches!(face, KeyFace::Released));
    }

    match face {
        KeyFace::Released => {
            p.circle_stroke(r.center() - vec2(0.0, k * 0.08), k * 0.1, Stroke::new(1.5, white(0.22)));
            p.text(r.center() + vec2(0.0, k * 0.16), Align2::CENTER_CENTER, "Options+", FontId::proportional(k * 0.1), white(0.38));
        }
        KeyFace::Empty => {
            p.circle_filled(r.center(), k * 0.025, white(0.12));
        }
        KeyFace::Back => {
            p.text(r.center(), Align2::CENTER_CENTER, "‹  Back", FontId::proportional(k * 0.14), white(0.85));
        }
        KeyFace::FocusText { text, colour } => {
            p.rect_filled(
                Rect::from_min_size(r.min + vec2(k * 0.08, k * 0.14), vec2(3.0, k * 0.72)),
                CornerRadius::same(2),
                with_alpha(c32(*colour), 0.8),
            );
            text_block(p, r.min + vec2(k * 0.15, k * 0.12), k * 0.76, text, k * 0.105, white(0.9), 5, false);
        }
        KeyFace::Session(f) => paint_session(p, r, f, cx),
        KeyFace::Hub(h) => paint_hub(p, r, h, cx),
    }

    // Long-press progress towards the focus view.
    if cx.hold > 0.02 {
        let c = r.center();
        arc(p, c, k * 0.46, -PI / 2.0, TAU * cx.hold, Stroke::new(2.5, white(0.75)));
    }
    // Keypad LCD backlight.
    let dim = (1.0 - cx.brightness).clamp(0.0, 0.9) * 0.7;
    if dim > 0.01 {
        p.rect_filled(r, CornerRadius::same(rad as u8), Color32::from_black_alpha((dim * 255.0) as u8));
    }
    if cx.hover && !cx.pressed {
        p.rect_stroke(r, CornerRadius::same(rad as u8), Stroke::new(1.0, white(0.3)), StrokeKind::Inside);
    }
}

/// The light field (see field.rs) drawn behind a frosted glass key.
fn paint_field(p: &Painter, r: Rect, fp: &crate::field::Field, orb: Color32, glowc: Color32, t: f32) {
    let k = r.width();
    let at = |x: f32, y: f32| pos2(r.left() + x * k, r.bottom() - (y - 0.1) * k);
    let p = p.with_clip_rect(r.shrink(1.0));
    let c = at(fp.cx, fp.cy);
    if fp.floor > 0.001 {
        gradient_rounded(&p, r, k * 0.2, with_alpha(orb, fp.floor * 0.9), with_alpha(orb, fp.floor * 0.3));
    }
    if fp.hi > 0.001 {
        glow(&p, c, fp.hs * k * 1.2, glowc, (fp.hi * 1.3).min(1.0));
    }
    for (rr, rw, ri) in [(fp.rr, fp.rw, fp.ri), (fp.r2r, fp.r2w, fp.r2i)] {
        if ri > 0.005 && rr > 0.0 {
            p.circle_stroke(c, rr * k, Stroke::new(rw * k * 1.8, with_alpha(glowc, ri * 0.25)));
            p.circle_stroke(c, rr * k, Stroke::new((rw * k * 0.55).max(1.0), with_alpha(orb, ri)));
        }
    }
    for (lx, ly, ls, li) in [fp.l1, fp.l2] {
        if li > 0.005 {
            glow(&p, at(lx, ly), ls * k * 2.4, glowc, (li * 1.5).min(1.0));
            p.circle_filled(at(lx, ly), (ls * k * 0.4).max(3.0), with_alpha(lerp_color(orb, white(1.0), 0.55), li.min(1.0)));
        }
    }
    if fp.ripple > 0.001 {
        for i in 0..3 {
            let s = (t / 1.5 + i as f32 / 3.0).fract();
            p.circle_stroke(c, k * (fp.cs + 0.26 * crate::field::ANIM_SCALE * s), Stroke::new(1.4, with_alpha(orb, fp.ripple * 2.5 * (1.0 - s))));
        }
    }
    if fp.ci > 0.001 {
        glow(&p, c, fp.cs * k * 2.4, orb, fp.ci.min(1.0));
        p.circle_filled(c, fp.cs * k * 0.62, with_alpha(lerp_color(orb, white(1.0), 0.45), fp.ci));
    }
    if fp.grain > 0.001 {
        // Sparkle that re-seeds at the state's grain rate.
        let step = (t / fp.grain_rate.max(0.02)) as u32;
        for i in 0..14u32 {
            let ang = crate::field::hash(i, step) * TAU;
            let d = crate::field::hash(i + 97, step) * fp.hs * k;
            let q = c + vec2(ang.cos(), ang.sin()) * d;
            p.circle_filled(q, (k * 0.012).max(1.6), with_alpha(lerp_color(orb, white(1.0), 0.6), (fp.grain * 7.0 * crate::field::hash(i + 7, step)).min(1.0)));
        }
    }
}

fn paint_session(p: &Painter, r: Rect, f: &SessionFace, cx: &KeyCx) {
    let k = r.width();
    let t = cx.t;
    let age = (t - f.since).max(0.0) as f32;
    let reduced = cx.reduced_motion;
    let phase = if reduced { 0.0 } else { (t % 1000.0) as f32 };
    let orb = c32(f.orb);
    let glowc = c32(f.glow);
    let g = f.activity.group();
    let mut fp = crate::field::params(f.activity, f.tool, phase, age, reduced).scaled(crate::field::ANIM_SCALE);
    if f.dots {
        fp.ci *= 0.35;
    }

    // The light lives behind the glass; the glass veil and its specular edge go on top.
    paint_field(p, r, &fp, orb, glowc, phase);
    gradient_rounded(p, r, k * 0.2, white(0.10), white(0.025));
    glass(p, r, k * 0.2, glowc, 0.55, false);
    if f.unread > 0 {
        let amber = Color32::from_rgb(255, 185, 35);
        p.rect_filled(r, CornerRadius::same((k * 0.2) as u8), with_alpha(amber, 0.22));
        p.rect_stroke(r, CornerRadius::same((k * 0.2) as u8), Stroke::new((k * 0.035).max(3.0), amber), StrokeKind::Inside);
    }

    let c = pos2(r.left() + fp.cx * k, r.bottom() - (fp.cy - 0.1) * k);
    let core = (fp.cs * k).max(k * 0.08);
    let ink = with_alpha(c32(crate::colour::ink_on(f.orb)), 0.85);
    if f.dots {
        // GPT-style status dots on the glass.
        let dr = core * 0.3;
        for i in 0..3 {
            let x = c.x + (i as f32 - 1.0) * dr * 2.9;
            let (dy, a) = match g {
                Group::Working => {
                    let s = (phase * 6.0 - i as f32 * 0.7).sin();
                    (-s.max(0.0) * dr * 1.4, 0.55 + 0.45 * s.max(0.0))
                }
                Group::Waiting => (0.0, 0.5 + 0.5 * ((phase * TAU / 1.6) - i as f32 * 0.5).sin().abs()),
                Group::Offline => (0.0, 0.25),
                Group::Idle => (0.0, 0.45),
                _ => (0.0, 1.0),
            };
            let col = if g == Group::Offline { white(0.25) } else { with_alpha(lerp_color(orb, white(1.0), 0.3), a) };
            p.circle_filled(pos2(x, c.y + dy), dr, col);
        }
    } else {
        match f.activity {
            Activity::Tool if cx.glyphs => {
                if let Some(tool) = f.tool {
                    p.text(c, Align2::CENTER_CENTER, tool.glyph(), FontId::monospace(core * 0.7), ink);
                }
            }
            Activity::Done => {
                let s = core * 0.45;
                let pts = vec![c + vec2(-s * 0.9, 0.0), c + vec2(-s * 0.25, s * 0.65), c + vec2(s, -s * 0.7)];
                p.add(Shape::line(pts, Stroke::new(s * 0.32, ink)));
            }
            Activity::Error => {
                p.text(c, Align2::CENTER_CENTER, "!", FontId::proportional(core * 1.1), ink);
            }
            Activity::WaitingQuestion => {
                p.text(c, Align2::CENTER_CENTER, "?", FontId::proportional(core * 1.0), ink);
            }
            _ => {}
        }
    }
    if f.unread > 0 {
        let count = if f.unread > 99 { "99+".to_owned() } else { f.unread.to_string() };
        let badge = Rect::from_min_size(pos2(r.left() + k * 0.08, r.top() + k * 0.055), vec2(k * 0.52, k * 0.14));
        p.rect_filled(badge, CornerRadius::same((k * 0.055) as u8), Color32::from_rgb(255, 185, 35));
        p.text(badge.center(), Align2::CENTER_CENTER, format!("UNREAD {count}"), FontId::proportional(k * 0.078), Color32::from_rgb(35, 22, 0));
    }
    if f.subagents > 0 {
        p.text(pos2(r.right() - k * 0.09, r.top() + k * 0.12), Align2::RIGHT_CENTER, format!("×{}", f.subagents), FontId::proportional(k * 0.075), white(0.75));
    }

    // Agent badge and name, then the state and its age, then what it's
    // doing (live), its unread count or its last turn, and the last message.
    let x = r.left() + k * 0.08;
    let w = k * 0.84;
    let title_y = r.top() + k * 0.5;
    let badge_rect = Rect::from_min_size(pos2(x, title_y + k * 0.01), vec2(k * 0.2, k * 0.12));
    p.rect_filled(badge_rect, CornerRadius::same((k * 0.055) as u8), with_alpha(c32(f.agent_colour), 0.55));
    p.text(badge_rect.center(), Align2::CENTER_CENTER, &f.badge, FontId::proportional(k * 0.08), white(0.95));
    let tx = badge_rect.right() + k * 0.035;
    let mut job = egui::text::LayoutJob::simple_singleline(f.title.clone(), FontId::proportional(k * 0.125), white(0.96));
    job.wrap.max_width = r.right() - k * 0.08 - tx;
    job.wrap.max_rows = 1;
    job.wrap.overflow_character = Some('…');
    let galley = p.layout_job(job);
    p.galley(pos2(tx, title_y), galley, white(0.96));

    let sy = title_y + k * 0.155;
    let state_col = match g {
        Group::Waiting => lerp_color(cx.waiting, white(1.0), 0.2),
        Group::Error => Color32::from_rgb(240, 110, 90),
        Group::Working => lerp_color(orb, white(1.0), 0.4),
        Group::Offline => white(0.4),
        _ => white(0.6),
    };
    p.text(pos2(x, sy), Align2::LEFT_TOP, state_word(f.activity), FontId::proportional(k * 0.095), state_col);
    let corner = crate::model::age_compact(f.last_turn, t);
    if !corner.is_empty() && !matches!(g, Group::Working | Group::Waiting) {
        p.text(pos2(r.right() - k * 0.08, sy), Align2::RIGHT_TOP, corner, FontId::proportional(k * 0.085), white(0.5));
    }
    let ly = sy + k * 0.125;
    if f.focus_header {
        text_block(p, pos2(x, ly), w, f.activity.label(), k * 0.088, white(0.6), 2, false);
    } else if matches!(g, Group::Working | Group::Waiting) {
        let live = if f.text.is_empty() {
            if g == Group::Working { "…".to_string() } else { f.activity.label().to_string() }
        } else {
            f.text.clone()
        };
        let caret = matches!(f.activity, Activity::Streaming | Activity::Thinking) && (t * 2.0) as i64 % 2 == 0 && !f.text.is_empty();
        let shown = if caret { format!("{live}▍") } else { live };
        let col = if g == Group::Waiting { lerp_color(cx.waiting, white(0.9), 0.35) } else { white(0.7) };
        text_block(p, pos2(x, ly), w, &shown, k * 0.088, col, 2, false);
    } else {
        let line = if f.unread > 0 {
            format!("{} unread", f.unread)
        } else if g == Group::Offline {
            format!("{} not running", f.agent_name)
        } else {
            format!("last turn {}", crate::model::age_long(f.last_turn, t))
        };
        let col = if f.unread > 0 { lerp_color(orb, white(1.0), 0.5) } else { white(0.5) };
        let h = text_block(p, pos2(x, ly), w, &line, k * 0.088, col, 1, false);
        if !f.last_message.is_empty() {
            text_block(p, pos2(x, ly + h), w, &f.last_message, k * 0.088, white(0.4), 1, false);
        }
    }
}

/// State word on a tile.
fn state_word(a: Activity) -> &'static str {
    if a == Activity::Empty { "Idle" } else { a.label() }
}

fn paint_hub(p: &Painter, r: Rect, h: &HubFace, cx: &KeyCx) {
    let k = r.width();
    let wc = c32(h.waiting_colour);
    let c = r.center() - vec2(0.0, k * 0.1);
    if h.waiting > 0 {
        let b = if cx.reduced_motion { 0.6 } else { 0.45 + 0.35 * (0.5 + 0.5 * ((cx.t % 1000.0) as f32 * TAU / 1.6).sin()) };
        glow(p, c, k * 0.4, wc, b);
        p.text(c, Align2::CENTER_CENTER, h.waiting.to_string(), FontId::proportional(k * 0.3), white(0.97));
        p.text(c + vec2(0.0, k * 0.2), Align2::CENTER_CENTER, "waiting", FontId::proportional(k * 0.085), lerp_color(wc, white(1.0), 0.4));
    } else {
        glow(p, c, k * 0.32, white(0.8), if h.dimmed { 0.08 } else { 0.18 });
        if h.working == 0 {
            p.circle_stroke(c, k * 0.1, Stroke::new(2.0, white(0.7)));
        }
        if h.working > 0 {
            p.text(c, Align2::CENTER_CENTER, h.working.to_string(), FontId::proportional(k * 0.2), white(0.9));
        }
        let label = if h.working > 0 { "working" } else { "none waiting" }; // "{n} working" (0x100b95e40)
        p.text(c + vec2(0.0, k * 0.2), Align2::CENTER_CENTER, label, FontId::proportional(k * 0.075), white(0.6));
    }
    // One small orb per session.
    let n = h.dots.len().min(12);
    if n > 0 {
        let per_row = n.min(6);
        let gap = k * 0.105;
        for (i, d) in h.dots.iter().take(12).enumerate() {
            let row = i / per_row;
            let col = i % per_row;
            let row_n = if row == 0 { per_row } else { n - per_row };
            let x = r.center().x + (col as f32 - (row_n as f32 - 1.0) / 2.0) * gap;
            let y = r.top() + k * 0.74 + row as f32 * gap * 0.8;
            p.circle_filled(pos2(x, y), k * 0.025, c32(*d));
        }
    }
    if h.pages > 1 {
        for i in 0..h.pages {
            let x = r.center().x + (i as f32 - (h.pages as f32 - 1.0) / 2.0) * k * 0.07;
            let on = i == h.page;
            p.circle_filled(pos2(x, r.bottom() - k * 0.07), k * if on { 0.022 } else { 0.016 }, white(if on { 0.9 } else { 0.3 }));
        }
    }
}

/// The hardware keypad body around the keys.
pub fn paint_device(p: &Painter, r: Rect) {
    let sh = Shadow { offset: [0, 18], blur: 48, spread: 2, color: Color32::from_black_alpha(110) };
    p.add(sh.as_shape(r, CornerRadius::same(36)));
    gradient_rounded(p, r, 36.0, Color32::from_rgba_unmultiplied(44, 46, 54, 235), Color32::from_rgba_unmultiplied(20, 21, 26, 245));
    p.rect_stroke(r, CornerRadius::same(36), Stroke::new(1.0, white(0.12)), StrokeKind::Inside);
    let inner = r.shrink(10.0);
    p.rect_stroke(inner, CornerRadius::same(28), Stroke::new(1.0, Color32::from_black_alpha(90)), StrokeKind::Inside);
}

pub fn page_button(p: &Painter, r: Rect, label: &str, held: bool, hover: bool) {
    glass(p, r, r.height() / 2.0, white(0.5), if held { 1.4 } else { 1.0 }, true);
    if hover {
        p.rect_stroke(r, CornerRadius::same((r.height() / 2.0) as u8), Stroke::new(1.0, white(0.3)), StrokeKind::Inside);
    }
    p.text(r.center(), Align2::CENTER_CENTER, label, FontId::proportional(r.height() * 0.5), white(if held { 1.0 } else { 0.8 }));
}

pub fn ambient_background(p: &Painter, r: Rect, t: f64, opaque: bool, reduced: bool) {
    let a = if opaque { 255 } else { 150 };
    gradient_rounded(
        p,
        r.expand(2.0),
        0.0,
        Color32::from_rgba_unmultiplied(22, 24, 38, a),
        Color32::from_rgba_unmultiplied(10, 11, 18, a),
    );
    // Slowly drifting colour blobs behind the glass.
    let tt = if reduced { 0.0 } else { (t % 10_000.0) as f32 * 0.05 };
    let blobs = [
        (Rgb(255, 150, 70), 0.18, 0.78, 0.0),
        (Rgb(120, 110, 255), 0.82, 0.22, 2.0),
        (Rgb(60, 200, 190), 0.6, 0.95, 4.0),
    ];
    for (col, fx, fy, off) in blobs {
        let c = pos2(
            r.left() + r.width() * (fx + 0.04 * (tt + off).sin()),
            r.top() + r.height() * (fy + 0.04 * (tt * 0.8 + off).cos()),
        );
        glow(p, c, r.width().max(r.height()) * 0.45, c32(col), 0.22);
    }
}
