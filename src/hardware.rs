//! Experimental driver for the Logitech MX Creative Keypad, built from the protocol notes in
//! MECHANICS.md: exclusive open of the vendor HID interface (which locks Options+ out),
//! page buttons diverted through feature 0x1B04, key presses on report 0x13, and 118×118
//! JPEG key images sent as 4095-byte packets on report 0x14. Off by default.

use crate::colour::Rgb;
use crate::model::{
    Activity, Core, Group, Hardware, HubFace, KeyFace, KeyPressTarget, SessionFace, KEY_COUNT, KEY_SIZE, ORIGIN_X, ORIGIN_Y, PITCH,
};
use ab_glyph::{Font, FontVec, PxScale, ScaleFont, point};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, GradientStop, Paint, PathBuilder, Pixmap, Point, RadialGradient, SpreadMode, Stroke, Transform};

/// FrameLoop.reschedule (0x10015d4dc): keep the tick interval unless it changes by more than this.
/// Keypad typography: key title 15 pt (weight 600), state line 13 pt mono (500),
/// body text 12 pt mono (400).
const TITLE_PT: f32 = 15.0;
const STATE_PT: f32 = 13.0;
const BODY_PT: f32 = 12.0;
const RESCHEDULE_THRESHOLD_S: f64 = 0.0001;
const VENDOR: u16 = 0x046D;
const USAGE_PAGE: u16 = 0xFF43;
const USAGE: u16 = 0x1A10;
const CID_PREV: u16 = 0x01A1;
const CID_NEXT: u16 = 0x01A2;
const FEAT_REPROG: u16 = 0x1B04;
const FEAT_BRIGHTNESS: u16 = 0x8040;
/// KEEP_ALIVE (performTakeover closure 0x1004a4ca4 looks up 0x0008; sendKeepAlive 0x1004a4f20
/// calls function 1 with params 0B B8 = 3000 ms, read from the static array at 0x100b970d8).
const FEAT_KEEP_ALIVE: u16 = 0x0008;
const KEEP_ALIVE_PARAMS: [u8; 2] = [0x0B, 0xB8];
const PACKET: usize = 4095;
const MAX_INPUT_REPORTS_PER_POLL: usize = 64;
const MAX_SINGLE_JPEG: usize = 4075;
// KeypadDevice.Timing from KeypadDeviceFactory.makeWithDiagnostics (0x1004ac734).
const KEEP_ALIVE_INTERVAL_S: f64 = 1.0;
const CONNECTION_SETTLE_S: f64 = 10.0;
const CONNECTION_QUIET_S: f64 = 2.0;
const REQUEST_TIMEOUT_S: f64 = 1.0;
const RELEASE_REQUEST_TIMEOUT_S: f64 = 0.2;
const RETRY_BASE_DELAY_S: f64 = 2.0;
const RETRY_MAX_DELAY_S: f64 = 60.0;
const MAX_AUTO_RETRIES: usize = 6;
/// Silent image-recovery retries (static array at 0x100b97108).
const IMAGE_RECOVERY_DELAYS_S: [f64; 4] = [2.0, 5.0, 15.0, 30.0];
const RECOVERY_STABLE_INTERVAL: Duration = Duration::from_secs(30);

/// scheduleRetry closure (0x1004a63dc): min(base · 2^(attempt-1), max).
fn retry_delay(attempt: usize) -> f64 {
    (RETRY_BASE_DELAY_S * 2f64.powi(attempt as i32 - 1)).min(RETRY_MAX_DELAY_S)
}

pub fn load_font() -> FontVec {
    for p in ["/System/Library/Fonts/SFNS.ttf", "/System/Library/Fonts/Helvetica.ttc"] {
        if let Ok(bytes) = std::fs::read(p) {
            if let Ok(f) = FontVec::try_from_vec(bytes) {
                return f;
            }
        }
    }
    FontVec::try_from_vec(epaint_default_fonts::UBUNTU_LIGHT.to_vec()).expect("bundled font")
}

// ------------------------------------------------------------------ rendering

fn color(c: Rgb, a: f32) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn fill_circle(pm: &mut Pixmap, x: f32, y: f32, r: f32, c: Color) {
    if let Some(path) = PathBuilder::from_circle(x, y, r.max(0.5)) {
        let mut paint = Paint::default();
        paint.set_color(c);
        paint.anti_alias = true;
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn glow(pm: &mut Pixmap, x: f32, y: f32, r: f32, c: Rgb, strength: f32) {
    let stops = vec![
        GradientStop::new(0.0, color(c, 0.9 * strength)),
        GradientStop::new(0.35, color(c, 0.4 * strength)),
        GradientStop::new(1.0, color(c, 0.0)),
    ];
    if let (Some(shader), Some(path)) = (
        RadialGradient::new(Point::from_xy(x, y), Point::from_xy(x, y), r, stops, SpreadMode::Pad, Transform::identity()),
        PathBuilder::from_circle(x, y, r),
    ) {
        let paint = Paint { shader, anti_alias: true, ..Paint::default() };
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
}

fn stroke_poly(pm: &mut Pixmap, pts: &[(f32, f32)], width: f32, c: Color) {
    let mut pb = PathBuilder::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        if i == 0 { pb.move_to(*x, *y) } else { pb.line_to(*x, *y) }
    }
    if let Some(path) = pb.finish() {
        let mut paint = Paint::default();
        paint.set_color(c);
        paint.anti_alias = true;
        let stroke = Stroke { width, line_cap: tiny_skia::LineCap::Round, ..Stroke::default() };
        pm.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    }
}

fn arc_pts(x: f32, y: f32, r: f32, a0: f32, sweep: f32) -> Vec<(f32, f32)> {
    (0..=40).map(|i| {
        let a = a0 + sweep * i as f32 / 40.0;
        (x + a.cos() * r, y + a.sin() * r)
    }).collect()
}

fn text_width(font: &FontVec, size: f32, s: &str) -> f32 {
    let sf = font.as_scaled(PxScale::from(size));
    s.chars().map(|c| sf.h_advance(font.glyph_id(c))).sum()
}

fn draw_text(pm: &mut Pixmap, font: &FontVec, size: f32, x: f32, baseline: f32, s: &str, c: Rgb, alpha: f32) {
    let sf = font.as_scaled(PxScale::from(size));
    let (w, h) = (pm.width() as i32, pm.height() as i32);
    let data = pm.data_mut();
    let mut pen = x;
    for ch in s.chars() {
        let id = font.glyph_id(ch);
        let g = id.with_scale_and_position(PxScale::from(size), point(pen, baseline));
        pen += sf.h_advance(id);
        if let Some(og) = font.outline_glyph(g) {
            let b = og.px_bounds();
            og.draw(|gx, gy, cov| {
                let px = b.min.x as i32 + gx as i32;
                let py = b.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= w || py >= h {
                    return;
                }
                let a = (cov * alpha).clamp(0.0, 1.0);
                let i = ((py * w + px) * 4) as usize;
                for (k, v) in [c.0, c.1, c.2].iter().enumerate() {
                    data[i + k] = (data[i + k] as f32 * (1.0 - a) + *v as f32 * a) as u8;
                }
                data[i + 3] = 255;
            });
        }
    }
}

fn fit(font: &FontVec, size: f32, s: &str, max_w: f32) -> String {
    if text_width(font, size, s) <= max_w {
        return s.to_string();
    }
    let mut out: String = String::new();
    for ch in s.chars() {
        let mut t = out.clone();
        t.push(ch);
        t.push('…');
        if text_width(font, size, &t) > max_w {
            break;
        }
        out.push(ch);
    }
    out + "…"
}

fn wrap(font: &FontVec, size: f32, s: &str, max_w: f32, rows: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        let cand = if cur.is_empty() { word.to_string() } else { format!("{cur} {word}") };
        if text_width(font, size, &cand) > max_w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
            cur = word.to_string();
            if lines.len() == rows {
                break;
            }
        } else {
            cur = cand;
        }
    }
    if !cur.is_empty() && lines.len() < rows {
        lines.push(cur);
    }
    if let Some(last) = lines.last_mut() {
        *last = fit(font, size, last, max_w);
    }
    lines
}

/// Renders one key to a 118×118 picture, matching the on-screen key as closely as a
/// small LCD allows.
pub fn render_key(face: &KeyFace, t: f64, font: &FontVec) -> Pixmap {
    let k = KEY_SIZE as f32;
    let mut pm = Pixmap::new(KEY_SIZE, KEY_SIZE).expect("pixmap");
    pm.fill(Color::from_rgba8(14, 15, 20, 255));
    let phase = (t % 1000.0) as f32;
    let white = Rgb(255, 255, 255);
    match face {
        KeyFace::Released | KeyFace::Empty => {}
        KeyFace::Quota(q) => {
            let label = fit(font, 12.0, &format!("{} · {}", q.provider, q.window), k - 16.0);
            draw_text(&mut pm, font, 12.0, (k - text_width(font, 12.0, &label)) / 2.0, k * 0.18, &label, white, 0.75);
            let remaining = crate::quota::displayed_remaining(q, crate::model::now() as i64);
            let hue = if q.provider.eq_ignore_ascii_case("Claude") { 95.0 } else { 270.0 };
            let accent = crate::colour::oklch(0.76, 0.15, hue);
            let (left, right, y) = (k * 0.16, k * 0.84, k * 0.585);
            stroke_poly(&mut pm, &[(left, y), (right, y)], k * 0.07, color(white, 0.16));
            if let Some(value) = remaining {
                let percent = value.clamp(0.0, 100.0).round() as u32;
                let title = format!("{percent}% Free");
                draw_text(&mut pm, font, 20.0, (k - text_width(font, 20.0, &title)) / 2.0, k * 0.43, &title, white, 0.96);
                if value > 0.0 {
                    let end = left + (right - left) * (value.clamp(0.0, 100.0) as f32 / 100.0);
                    stroke_poly(&mut pm, &[(left, y), (end, y)], k * 0.07, color(accent, 1.0));
                }
                let used = format!("{}% Used", 100 - percent);
                draw_text(&mut pm, font, 12.0, (k - text_width(font, 12.0, &used)) / 2.0, k * 0.76, &used, white, 0.7);
            } else {
                let title = "Unknown";
                draw_text(&mut pm, font, 20.0, (k - text_width(font, 20.0, title)) / 2.0, k * 0.43, title, white, 0.8);
            }
            let detail = fit(font, 10.0, &crate::quota::detail(q), k - 16.0);
            draw_text(&mut pm, font, 10.0, (k - text_width(font, 10.0, &detail)) / 2.0, k * 0.93, &detail, white, 0.6);
        }
        KeyFace::QuotaHub => { let label = "Quotas"; draw_text(&mut pm, font, 18.0, (k - text_width(font, 18.0, label)) / 2.0, 64.0, label, white, 0.9); }
        KeyFace::Step(step) => {
            for (line, y, size) in [(&step.kind, 20.0, 11.0), (&step.title, 42.0, 14.0), (&step.status, 60.0, 10.0)] {
                let text = fit(font, size, line, k - 12.0);
                draw_text(&mut pm, font, size, (k - text_width(font, size, &text)) / 2.0, y, &text, white, 0.85);
            }
            for (i, line) in wrap(font, 10.0, &step.text, k - 12.0, 3).iter().enumerate() {
                let text = fit(font, 10.0, line, k - 12.0);
                draw_text(&mut pm, font, 10.0, (k - text_width(font, 10.0, &text)) / 2.0, 79.0 + i as f32 * 13.0, &text, white, 0.85);
            }
        }
        KeyFace::Back => { let label = "← Back"; draw_text(&mut pm, font, 16.0, (k - text_width(font, 16.0, label)) / 2.0, 64.0, label, white, 0.9); }
        KeyFace::Session(f) => render_session(&mut pm, f, phase, t, font),
        KeyFace::Hub(h) => render_hub(&mut pm, h, (t % 1248.0) as f32, font),
    }
    if let KeyFace::Session(f) = face {
        for pixel in pm.data_mut().chunks_exact_mut(4) {
            for channel in &mut pixel[..3] { *channel = (*channel as f32 * f.opacity).round() as u8; }
        }
    }
    pm
}

fn render_session(pm: &mut Pixmap, f: &SessionFace, phase: f32, t: f64, font: &FontVec) {
    // Same light field as the on-screen key (field.rs).
    let k = KEY_SIZE as f32;
    if f.unread > 0 {
        pm.fill(Color::from_rgba8(98, 64, 8, 255));
    }
    let g = f.activity.group();
    let age = (t - f.since).max(0.0) as f32;
    let mut fp = crate::field::params(f.activity, f.tool, phase, age, false).scaled(crate::field::ANIM_SCALE);
    if f.dots {
        fp.ci *= 0.35;
    }
    // Same placement as the on-screen key: y measured up from the bottom, light near the top.
    let at = |x: f32, y: f32| (x * k, k - (y - 0.1) * k);
    let (cx, cy) = at(fp.cx, fp.cy);
    let rr = (fp.cs * k).max(k * 0.08);
    if fp.floor > 0.001 {
        if let Some(rect) = tiny_skia::Rect::from_xywh(0.0, 0.0, k, k) {
            let path = PathBuilder::from_rect(rect);
            let mut paint = Paint::default();
            paint.set_color(color(f.orb, fp.floor * 0.6));
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
    if fp.hi > 0.001 {
        glow(pm, cx, cy, fp.hs * k * 1.2, f.glow, (fp.hi * 1.3).min(1.0));
    }
    for (r, w, i) in [(fp.rr, fp.rw, fp.ri), (fp.r2r, fp.r2w, fp.r2i)] {
        if i > 0.005 && r > 0.0 {
            stroke_poly(pm, &arc_pts(cx, cy, r * k, 0.0, std::f32::consts::TAU), (w * k * 0.55).max(1.0), color(f.orb, i));
        }
    }
    for (lx, ly, ls, li) in [fp.l1, fp.l2] {
        if li > 0.005 {
            let (x, y) = at(lx, ly);
            glow(pm, x, y, ls * k * 2.4, f.glow, (li * 1.5).min(1.0));
            // A solid centre so the moving lights read as dots on the keypad's small screen.
            fill_circle(pm, x, y, (ls * k * 0.4).max(2.0), color(f.orb.mix(Rgb(255, 255, 255), 0.55), li.min(1.0)));
        }
    }
    if fp.ripple > 0.001 {
        for i in 0..3 {
            let s = (phase / 1.5 + i as f32 / 3.0).fract();
            stroke_poly(pm, &arc_pts(cx, cy, k * (fp.cs + 0.26 * crate::field::ANIM_SCALE * s), 0.0, std::f32::consts::TAU), 1.2, color(f.orb, fp.ripple * 2.5 * (1.0 - s)));
        }
    }
    if fp.ci > 0.001 {
        glow(pm, cx, cy, fp.cs * k * 2.4, f.orb, fp.ci.min(1.0));
        fill_circle(pm, cx, cy, fp.cs * k * 0.62, color(f.orb.mix(Rgb(255, 255, 255), 0.45), fp.ci));
    }
    if fp.grain > 0.001 {
        // Sparkle that re-seeds at the state's grain rate, sized to show at 118 px.
        let step = (phase / fp.grain_rate.max(0.02)) as u32;
        for i in 0..14u32 {
            let ang = crate::field::hash(i, step) * std::f32::consts::TAU;
            let d = crate::field::hash(i + 97, step) * fp.hs * k;
            let a = (fp.grain * 7.0 * crate::field::hash(i + 7, step)).min(1.0);
            fill_circle(pm, cx + ang.cos() * d, cy + ang.sin() * d, 1.6, color(f.orb.mix(Rgb(255, 255, 255), 0.6), a));
        }
    }
    let ink = crate::colour::ink_on(f.orb);
    if f.dots {
        let dr = rr * 0.3;
        for i in 0..3 {
            let x = cx + (i as f32 - 1.0) * dr * 2.9;
            let s = (phase * 6.0 - i as f32 * 0.7).sin().max(0.0);
            let (dy, a) = if g == Group::Working { (-s * dr * 1.4, 0.55 + 0.45 * s) } else if g == Group::Idle { (0.0, 0.45) } else { (0.0, 1.0) };
            fill_circle(pm, x, cy + dy, dr, color(f.orb.mix(Rgb(255, 255, 255), 0.3), a));
        }
    } else {
        match f.activity {
            Activity::Done => {
                let s = rr * 0.45;
                stroke_poly(pm, &[(cx - s * 0.9, cy), (cx - s * 0.25, cy + s * 0.65), (cx + s, cy - s * 0.7)], s * 0.32, color(ink, 0.9));
            }
            Activity::Error => draw_text(pm, font, rr * 1.1, cx - rr * 0.15, cy + rr * 0.38, "!", ink, 0.9),
            Activity::WaitingQuestion => draw_text(pm, font, rr, cx - rr * 0.25, cy + rr * 0.35, "?", ink, 0.9),
            _ => {}
        }
    }
    if f.unread > 0 {
        let count = if f.unread > 99 { "99+".to_owned() } else { f.unread.to_string() };
        let label = format!("UNREAD {count}");
        if let Some(rect) = tiny_skia::Rect::from_xywh(7.0, 6.0, 58.0, 15.0) {
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(255, 185, 35, 255));
            pm.fill_path(&PathBuilder::from_rect(rect), &paint, FillRule::Winding, Transform::identity(), None);
        }
        draw_text(pm, font, 9.0, 36.0 - text_width(font, 9.0, &label) / 2.0, 17.0, &label, Rgb(35, 22, 0), 1.0);
    }
    if f.scheduled {
        let (x, y, r) = (k - 13.0, 24.0, 5.0);
        let tint = color(Rgb(255, 255, 255), 0.65);
        stroke_poly(pm, &arc_pts(x, y, r, 0.0, std::f32::consts::TAU), 1.2, tint);
        stroke_poly(pm, &[(x, y - 3.0), (x, y), (x + 2.5, y + 1.5)], 1.2, tint);
    }
    // Badge and name under the light, then the state, then the live text,
    // unread count or last turn.
    let white = Rgb(255, 255, 255);
    let ty = k * 0.61;
    fill_circle(pm, 13.0, ty - 5.0, 7.5, color(f.agent_colour, 0.9));
    let ink = crate::colour::ink_on(f.agent_colour);
    draw_text(pm, font, 8.0, 13.0 - text_width(font, 8.0, &f.badge) / 2.0, ty - 2.0, &f.badge, ink, 0.95);
    let title = fit(font, TITLE_PT, &f.title, k - 32.0);
    draw_text(pm, font, TITLE_PT, 25.0, ty, &title, white, 0.95);
    // Session activity label.
    let state = if f.activity == Activity::Empty { "Idle" } else { f.activity.label() };
    let sc = match g {
        Group::Waiting => waiting_tint(f.orb),
        Group::Working => f.orb.mix(white, 0.4),
        _ => Rgb(160, 160, 165),
    };
    let sy = k * 0.745;
    draw_text(pm, font, STATE_PT, 7.0, sy, state, sc, 0.95);
    let corner = crate::model::age_compact(f.last_turn, t);
    if !corner.is_empty() && !matches!(g, Group::Working | Group::Waiting) {
        draw_text(pm, font, BODY_PT, k - 7.0 - text_width(font, BODY_PT, &corner), sy, &corner, Rgb(150, 150, 155), 0.9);
    }
    let live = if matches!(g, Group::Working | Group::Waiting) {
        crate::model::key_text(&f.text, f.unread, f.activity)
    } else if f.unread > 0 {
        format!("{} unread", f.unread)
    } else {
        format!("last turn {}", crate::model::age_long(f.last_turn, t))
    };
    let tc = if g == Group::Waiting { f.orb.mix(white, 0.35) } else { Rgb(195, 195, 200) };
    for (i, l) in wrap(font, BODY_PT, &live, k - 14.0, 2).iter().enumerate() {
        draw_text(pm, font, BODY_PT, 7.0, k * 0.865 + i as f32 * 13.0, l, tc, 0.85);
    }
}

fn waiting_tint(c: Rgb) -> Rgb {
    c.mix(Rgb(255, 255, 255), 0.15)
}

fn render_hub(pm: &mut Pixmap, h: &HubFace, phase: f32, font: &FontVec) {
    let k = KEY_SIZE as f32;
    let white = Rgb(255, 255, 255);
    // The LCD is square; preserve a dark rounded margin matching the on-screen shell.
    for y in 0..KEY_SIZE {
        for x in 0..KEY_SIZE {
            let nx = (x as f32 + 0.5) / k;
            let ny = (y as f32 + 0.5) / k;
            let radius = 0.20_f32;
            let dx = (radius - nx.min(1.0 - nx)).max(0.0);
            let dy = (radius - ny.min(1.0 - ny)).max(0.0);
            if dx * dx + dy * dy <= radius * radius {
                let c = crate::field::hub_colour(nx, ny, if h.reduced_motion { 0.0 } else { (phase * 2.0).floor() / 2.0 });
                let i = ((y * KEY_SIZE + x) * 4) as usize;
                pm.data_mut()[i..i + 4].copy_from_slice(&[c.0, c.1, c.2, 255]);
            }
        }
    }
    if h.waiting > 0 || h.working > 0 {
        let (count, label) = if h.waiting > 0 { (h.waiting, "waiting") } else { (h.working, "working") };
        let count = count.to_string();
        draw_text(pm, font, 33.0, (k - text_width(font, 33.0, &count)) / 2.0, k * 0.51, &count, white, 0.98);
        draw_text(pm, font, 11.0, (k - text_width(font, 11.0, label)) / 2.0, k * 0.67, label, white, 0.72);
    } else {
        for (label, size, y, opacity) in [("Padgent", 16.0, 0.51, 0.93), ("all clear", 10.0, 0.67, 0.62)] {
            draw_text(pm, font, size, (k - text_width(font, size, label)) / 2.0, k * y, label, white, opacity);
        }
    }
    if h.pages > 1 {
        let page = format!("{} / {}", h.page + 1, h.pages);
        draw_text(pm, font, 9.0, (k - text_width(font, 9.0, &page)) / 2.0, k * 0.93, &page, white, 0.58);
    }
}

pub fn jpeg(pm: &Pixmap) -> Vec<u8> {
    let rgb: Vec<u8> = pm.data().chunks(4).flat_map(|p| [p[0], p[1], p[2]]).collect();
    let mut q = 88u8;
    loop {
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q);
        if enc.write_image(&rgb, KEY_SIZE, KEY_SIZE, image::ExtendedColorType::Rgb8).is_err() {
            return Vec::new();
        }
        if out.len() <= MAX_SINGLE_JPEG || q <= 40 {
            return out;
        }
        q -= 12;
    }
}

use image::ImageEncoder;

/// Image packets for one key (VLPFraming.packets, 0x1004af5c0).
pub fn packets(jpeg: &[u8], key: usize, defer_update: bool) -> Vec<Vec<u8>> {
    let col = (key % 3) as u32;
    let row = (key / 3) as u32;
    let (x, y, w, h) = (ORIGIN_X + col * PITCH, ORIGIN_Y + row * PITCH, KEY_SIZE, KEY_SIZE);
    let len = jpeg.len();
    let mut out = Vec::new();
    let mut offset = 0usize;
    let mut counter = 1u8;
    while offset < len || out.is_empty() {
        let header = if offset == 0 { 20 } else { 5 };
        let take = (PACKET - header).min(len - offset);
        let mut p = vec![0u8; PACKET];
        p[0..4].copy_from_slice(&[0x14, 0xFF, 0x02, 0x2B]);
        let last = offset + take >= len;
        let mut flags = 0x20 | (counter & 0x0F);
        if offset == 0 {
            flags |= 0x80;
        }
        if last {
            flags |= 0x40;
        }
        p[4] = flags;
        if offset == 0 {
            p[5] = 1;
            p[6] = defer_update as u8;
            p[7] = 1;
            p[9] = (x >> 8) as u8;
            p[10] = x as u8;
            p[11] = (y >> 8) as u8;
            p[12] = y as u8;
            p[13] = (w >> 8) as u8;
            p[14] = w as u8;
            p[15] = (h >> 8) as u8;
            p[16] = h as u8;
            p[17] = (len >> 16) as u8;
            p[18] = (len >> 8) as u8;
            p[19] = len as u8;
        }
        p[header..header + take].copy_from_slice(&jpeg[offset..offset + take]);
        out.push(p);
        offset += take;
        counter = counter.wrapping_add(1);
        if take == 0 {
            break;
        }
    }
    out
}

// ------------------------------------------------------------------ device

struct Device {
    dev: hidapi::HidDevice,
    reprog: u8,
    keep_alive: u8,
    last_keep_alive: Instant,
    brightness: u8,
    saved_brightness: Option<[u8; 2]>,
    pressed: Vec<usize>,
    pages: [bool; 2],
    sent: Vec<Option<Vec<u8>>>,
    last_faces: Vec<Option<KeyFace>>,
    displayed_targets: [KeyPressTarget; KEY_COUNT],
    displayed_view: Option<DisplayView>,
    last_frame: Instant,
    next_key: usize,
    image_retry_at: [Option<Instant>; KEY_COUNT],
    image_retry_stage: [usize; KEY_COUNT],
}

fn hidpp(dev: &hidapi::HidDevice, feature: u8, function: u8, params: &[u8]) -> Option<[u8; 20]> {
    hidpp_timeout(dev, feature, function, params, REQUEST_TIMEOUT_S)
}

fn hidpp_timeout(dev: &hidapi::HidDevice, feature: u8, function: u8, params: &[u8], timeout: f64) -> Option<[u8; 20]> {
    let mut req = [0u8; 20];
    req[0] = 0x11;
    req[1] = 0xFF;
    req[2] = feature;
    req[3] = (function << 4) | 0x0E;
    for (i, b) in params.iter().take(16).enumerate() {
        req[4 + i] = *b;
    }
    write_report(dev, &req).ok()?;
    let deadline = Instant::now() + Duration::from_secs_f64(timeout);
    let mut buf = [0u8; 64];
    while Instant::now() < deadline {
        let n = dev.read_timeout(&mut buf, 50).ok()?;
        if n >= 20 && buf[0] == 0x11 && buf[1] == 0xFF {
            if buf[2] == feature && buf[3] == req[3] {
                let mut out = [0u8; 20];
                out.copy_from_slice(&buf[..20]);
                return Some(out);
            }
            if buf[2] == 0xFF && buf[3] == feature {
                return None; // HID++ error
            }
        }
    }
    None
}

fn feature_index(dev: &hidapi::HidDevice, id: u16) -> Option<u8> {
    hidpp(dev, 0x00, 0, &[(id >> 8) as u8, id as u8]).map(|r| r[4]).filter(|i| *i != 0)
}

fn find(api: &mut hidapi::HidApi) -> (Option<std::ffi::CString>, bool) {
    let refreshed = api.refresh_devices().is_ok();
    let path = api.device_list()
        .find(|d| d.vendor_id() == VENDOR && d.usage_page() == USAGE_PAGE && d.usage() == USAGE)
        .map(|d| d.path().to_owned());
    (path, refreshed)
}

fn take_over(api: &hidapi::HidApi, path: &std::ffi::CStr, brightness: f32) -> Result<Device, String> {
    #[cfg(target_os = "macos")]
    api.set_open_exclusive(true);
    let dev = api.open_path(path).map_err(|e| {
        let s = e.to_string();
        if s.contains("exclusive") || s.contains("0xe00002c5") {
            "Another app is using the keypad. Release it there, then try again.".to_string()
        } else if s.to_lowercase().contains("not permitted") {
            "macOS blocked the keypad. Allow this app in Privacy & Security › Input Monitoring.".to_string()
        } else {
            format!("Could not take over the keypad: {s}")
        }
    })?;
    let reprog = feature_index(&dev, FEAT_REPROG).unwrap_or(0);
    for cid in [CID_PREV, CID_NEXT] {
        if reprog != 0 {
            // setCidReporting: divert + valid.
            let _ = hidpp(&dev, reprog, 3, &[(cid >> 8) as u8, cid as u8, 0x03]);
        }
    }
    let bidx = feature_index(&dev, FEAT_BRIGHTNESS).unwrap_or(0);
    let mut saved = None;
    if bidx != 0 {
        if let (Some(info), Some(cur)) = (hidpp(&dev, bidx, 0, &[]), hidpp(&dev, bidx, 1, &[])) {
            saved = Some([cur[4], cur[5]]);
            let max = u16::from_be_bytes([info[4], info[5]]).max(1);
            let v = (max as f32 * brightness.clamp(0.05, 1.0)) as u16;
            let _ = hidpp(&dev, bidx, 2, &v.to_be_bytes());
        }
    }
    let keep_alive = feature_index(&dev, FEAT_KEEP_ALIVE).unwrap_or(0);
    if keep_alive != 0 {
        let _ = hidpp(&dev, keep_alive, 1, &KEEP_ALIVE_PARAMS);
    }
    let _ = dev.set_blocking_mode(false);
    Ok(Device {
        dev,
        reprog,
        keep_alive,
        last_keep_alive: Instant::now(),
        brightness: bidx,
        saved_brightness: saved,
        pressed: Vec::new(),
        pages: [false; 2],
        sent: vec![None; KEY_COUNT],
        last_faces: vec![None; KEY_COUNT],
        displayed_targets: std::array::from_fn(|_| KeyPressTarget::Disabled),
        displayed_view: None,
        last_frame: Instant::now() - Duration::from_secs(5),
        next_key: 0,
        image_retry_at: [None; KEY_COUNT],
        image_retry_stage: [0; KEY_COUNT],
    })
}

fn release(d: Device) {
    for cid in [CID_PREV, CID_NEXT] {
        if d.reprog != 0 {
            let _ = hidpp_timeout(&d.dev, d.reprog, 3, &[(cid >> 8) as u8, cid as u8, 0x02], RELEASE_REQUEST_TIMEOUT_S);
        }
    }
    if let (true, Some(b)) = (d.brightness != 0, d.saved_brightness) {
        let _ = hidpp_timeout(&d.dev, d.brightness, 2, &b, RELEASE_REQUEST_TIMEOUT_S);
    }
    drop(d); // closing the exclusive handle gives the keypad back to Options+
}

/// Parse an image acknowledgement using the last output packet's part byte.
fn image_ack(report: &[u8], expected_part: u8) -> Option<Result<(), [u8; 2]>> {
    if report.len() < 7 || report[..4] != [0x13, 0xFF, 0x02, 0x2B] || report[4] != expected_part {
        return None;
    }
    let status = [report[5], report[6]];
    Some(if status == [0x00, 0x01] { Ok(()) } else { Err(status) })
}

/// Dispatch a single input report. Image acknowledgements are handled separately.
fn handle_input_report(d: &mut Device, core: &Arc<Mutex<Core>>, buf: &[u8]) -> bool {
    let n = buf.len();
    if n < 4 || buf[1] != 0xFF {
        return false;
    }
    if buf[0] == 0x13 && buf[2] == 0x02 && buf[3] == 0x00 && n >= 6 && buf[5] == 0x01 {
        let now_pressed: Vec<usize> = buf[6..n].iter().take_while(|b| **b != 0).filter(|b| **b <= 9).map(|b| (*b - 1) as usize).collect();
        crate::mac::log(&format!("keypad keys {now_pressed:?}"));
        if let Ok(mut c) = core.lock() {
            for k in &now_pressed {
                if !d.pressed.contains(k) {
                    c.key_down_with_target(*k, d.displayed_targets[*k].clone());
                }
            }
            for k in &d.pressed {
                if !now_pressed.contains(k) {
                    c.key_up(*k);
                }
            }
        }
        d.pressed = now_pressed;
        return true;
    } else if buf[0] == 0x11 && d.reprog != 0 && buf[2] == d.reprog && buf[3] == 0x00 {
        let mut pages = [false; 2];
        for pair in buf[4..n.min(20)].chunks(2) {
            if pair.len() == 2 {
                match u16::from_be_bytes([pair[0], pair[1]]) {
                    CID_PREV => pages[0] = true,
                    CID_NEXT => pages[1] = true,
                    _ => {}
                }
            }
        }
        crate::mac::log(&format!("keypad page buttons {pages:?}"));
        if let Ok(mut c) = core.lock() {
            for i in 0..2 {
                if pages[i] != d.pages[i] {
                    c.page_button(i == 1, pages[i]);
                }
            }
        }
        d.pages = pages;
        return true;
    }
    false
}

/// Reads key and page-button input (InputParser.parse, 0x1004aff60).
fn poll_input(d: &mut Device, core: &Arc<Mutex<Core>>) -> Result<bool, String> {
    let mut buf = [0u8; PACKET];
    let mut any = false;
    for _ in 0..MAX_INPUT_REPORTS_PER_POLL {
        let n = d.dev.read_timeout(&mut buf, 0).map_err(|e| format!("Keypad input failed: {e}"))?;
        if n == 0 { break; }
        any |= handle_input_report(d, core, &buf[..n]);
    }
    Ok(any)
}

/// Wait for the device to accept the last part, dispatching input while it responds.
fn wait_image_ack(d: &mut Device, core: &Arc<Mutex<Core>>, expected_part: u8, key: usize, jpeg_len: usize, packet_count: usize) -> Result<(bool, bool), String> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buf = [0u8; PACKET];
    let mut input = false;
    loop {
        service_keep_alive(d)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(format!("Keypad image acknowledgement timed out for key {key} (part {expected_part:02x})"));
        }
        let timeout_ms = remaining.as_millis().min(50).max(1) as i32;
        let n = d.dev.read_timeout(&mut buf, timeout_ms).map_err(|e| format!("Keypad input failed while waiting for image acknowledgement: {e}"))?;
        if n == 0 { continue; }
        let report = &buf[..n];
        if let Some(ack) = image_ack(report, expected_part) {
            if let Err(status) = ack {
                let prefix = report.iter().take(16).map(|byte| format!("{byte:02x}")).collect::<Vec<_>>().join(" ");
                let x = ORIGIN_X + (key % 3) as u32 * PITCH;
                let y = ORIGIN_Y + (key / 3) as u32 * PITCH;
                crate::mac::log(&format!("keypad image rejection key {key}, origin {x},{y}, size {KEY_SIZE}x{KEY_SIZE}, jpeg {jpeg_len} bytes, packets {packet_count}, status {:02x} {:02x}, ack length {n}, prefix {prefix}", status[0], status[1]));
                return Ok((input, false));
            }
            return Ok((input, true));
        }
        input |= handle_input_report(d, core, report);
    }
}

fn checked_write_result(result: Result<usize, String>, expected: usize) -> Result<(), String> {
    let written = result?;
    if written != expected {
        return Err(format!("short HID write ({written} of {expected} bytes)"));
    }
    Ok(())
}

fn write_report(dev: &hidapi::HidDevice, report: &[u8]) -> Result<(), String> {
    checked_write_result(dev.write(report).map_err(|e| e.to_string()), report.len())
}

fn service_keep_alive(d: &mut Device) -> Result<(), String> {
    if d.keep_alive != 0 && d.last_keep_alive.elapsed().as_secs_f64() >= KEEP_ALIVE_INTERVAL_S {
        let mut r = [0u8; 20];
        r[0] = 0x11;
        r[1] = 0xFF;
        r[2] = d.keep_alive;
        r[3] = (1 << 4) | 0x0E;
        r[4..6].copy_from_slice(&KEEP_ALIVE_PARAMS);
        write_report(&d.dev, &r).map_err(|e| format!("Keypad keep-alive failed: {e}"))?;
        d.last_keep_alive = Instant::now();
    }
    Ok(())
}

fn image_needs_send(previous: Option<&[u8]>, current: &[u8], refresh: bool) -> bool {
    refresh || previous != Some(current)
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DisplayView {
    quota_view: bool,
    quota_page: usize,
    page: usize,
    in_control: bool,
    detail: Option<(String, usize)>,
}

fn display_view(core: &Core) -> DisplayView {
    DisplayView { quota_view: core.quota_view, quota_page: core.quota_page, page: core.page.min(core.pages() - 1), in_control: core.in_control, detail: core.detail_key.clone().map(|key| (key, core.detail_page)) }
}

// A page transition must redraw every key, including empty slots.
fn prepare_display_view(previous: &mut Option<DisplayView>, current: &DisplayView, sent: &mut [Option<Vec<u8>>]) -> bool {
    if previous.as_ref() == Some(current) {
        return false;
    }
    sent.fill(None);
    *previous = Some(current.clone());
    true
}

/// A new view must not inherit a rejected tile's retry deadline from the old view.
fn prepare_device_display_view(
    previous: &mut Option<DisplayView>,
    current: &DisplayView,
    sent: &mut [Option<Vec<u8>>],
    last_faces: &mut [Option<KeyFace>],
    retry_at: &mut [Option<Instant>],
    retry_stage: &mut [usize],
) -> bool {
    if !prepare_display_view(previous, current, sent) {
        return false;
    }
    last_faces.fill(None);
    retry_at.fill(None);
    retry_stage.fill(0);
    true
}

/// Preserve the original driver rule: render changed faces and animated states.
fn face_needs_render(previous: Option<&KeyFace>, current: &KeyFace, refresh: bool) -> bool {
    refresh || previous != Some(current)
        || matches!(current, KeyFace::Session(session) if session.activity.animated())
        || matches!(current, KeyFace::Hub(hub) if !hub.reduced_motion || hub.waiting > 0)
}

/// The scheduler visits each key in circular order, including empty slots.
fn scheduled_key(start: usize, offset: usize) -> usize {
    (start + offset) % KEY_COUNT
}

/// Delay retries for a rejected image without reopening the HID device.
fn image_rejection_delay(stage: usize) -> Duration {
    Duration::from_secs_f64(IMAGE_RECOVERY_DELAYS_S[stage.min(IMAGE_RECOVERY_DELAYS_S.len() - 1)])
}

fn image_retry_due(retry_at: Option<Instant>, now: Instant) -> bool {
    retry_at.is_none_or(|deadline| now >= deadline)
}

fn rejected_image_retry(stage: usize, now: Instant) -> (usize, Instant) {
    (stage.saturating_add(1), now + image_rejection_delay(stage))
}

/// Send changed images in circular order, waiting for each acknowledgement.
fn send_frame(d: &mut Device, core: &Arc<Mutex<Core>>, view: &DisplayView, faces: &[KeyFace; KEY_COUNT], t: f64, font: &FontVec) -> Result<bool, String> {
    service_keep_alive(d)?;
    if prepare_device_display_view(&mut d.displayed_view, view, &mut d.sent, &mut d.last_faces, &mut d.image_retry_at, &mut d.image_retry_stage) {
        let empty = faces.iter().filter(|face| matches!(face, KeyFace::Empty)).count();
        crate::mac::log(&format!("keypad refresh page {} empty {empty}", view.page + 1));
    }
    let refresh = d.keep_alive == 0 && d.last_frame.elapsed().as_secs_f64() > KEEP_ALIVE_INTERVAL_S;
    let start_key = d.next_key;
    let mut pressed = false;
    for offset in 0..KEY_COUNT {
        service_keep_alive(d)?;
        let i = scheduled_key(start_key, offset);
        if !image_retry_due(d.image_retry_at[i], Instant::now()) {
            continue;
        }
        if core.lock().map(|c| display_view(&c) != *view).unwrap_or(true) {
            break;
        }
        let refresh_key = refresh && i == crate::model::HUB_KEY;
        if !face_needs_render(d.last_faces[i].as_ref(), &faces[i], refresh_key) {
            continue;
        }
        let pm = render_key(&faces[i], t, font);
        let jpg = jpeg(&pm);
        if jpg.is_empty() {
            return Err(format!("Could not encode keypad image for key {i}"));
        }
        if !image_needs_send(d.sent[i].as_deref(), &jpg, refresh_key) {
            d.last_faces[i] = Some(faces[i].clone());
            d.displayed_targets[i] = crate::model::target_for_face(&faces[i], view.quota_view);
            continue;
        }
        let image_packets = packets(&jpg, i, false);
        let expected_part = image_packets.last().expect("image has a packet")[4] & !0x20;
        d.next_key = scheduled_key(i, 1);
        for p in &image_packets {
            write_report(&d.dev, p).map_err(|e| format!("Keypad stopped accepting images: {e}"))?;
        }
        let (input, accepted) = wait_image_ack(d, core, expected_part, i, jpg.len(), image_packets.len())?;
        if input {
            run_key_actions(core);
            pressed = true;
        }
        if accepted {
            d.sent[i] = Some(jpg);
            d.last_faces[i] = Some(faces[i].clone());
            d.displayed_targets[i] = crate::model::target_for_face(&faces[i], view.quota_view);
            d.image_retry_at[i] = None;
            d.image_retry_stage[i] = 0;
            d.last_frame = Instant::now();
        } else {
            let (stage, deadline) = rejected_image_retry(d.image_retry_stage[i], Instant::now());
            d.image_retry_stage[i] = stage;
            d.image_retry_at[i] = Some(deadline);
        }
        if poll_input(d, core)? {
            run_key_actions(core);
            pressed = true;
        }
    }
    Ok(pressed)
}

/// Opens what a physical key asked for on the main thread, like a click in the window.
fn run_key_actions(core: &Arc<Mutex<Core>>) {
    let Ok(mut c) = core.lock() else { return };
    let actions = std::mem::take(&mut c.actions);
    if actions.is_empty() {
        return;
    }
    let (agents, sessions) = (c.agents.clone(), c.sessions.clone());
    drop(c);
    crate::mac::on_main(Box::new(move || {
        for a in actions {
            match a {
                crate::model::Action::Open { agent, session } => {
                    let def = agents.iter().find(|d| d.id == agent);
                    match sessions.iter().find(|s| s.agent == agent && s.id == session) {
                        Some(s) => crate::integrations::open_exact(def, s),
                        None => crate::integrations::open_session(def, &session),
                    }
                }
                crate::model::Action::OpenUrl(u) => crate::integrations::open_url(&u),
                crate::model::Action::ShowWindow => crate::mac::show_app(),
            }
        }
    }));
}

pub fn start(core: Arc<Mutex<Core>>, wake: Arc<dyn Fn() + Send + Sync>) {
    std::thread::Builder::new()
        .name("keypad".into())
        .spawn(move || {
            let font = load_font();
            let mut api: Option<hidapi::HidApi> = None;
            let mut dev: Option<Device> = None;
            let mut last_scan = Instant::now() - Duration::from_secs(10);
            let mut path: Option<std::ffi::CString> = None;
            let mut retries = 0usize;
            let mut recovery_stage = 0usize;
            let mut connected_at: Option<Instant> = None;
            let mut retry_at = Instant::now();
            // Wait for Options+ to finish initialising a newly plugged keypad before taking
            // over: quiet for 2 s, at most 10 s ("waiting for Options+ initialization").
            let mut arrived: Option<Instant> = None;
            let mut first_scan = true;
            let set = |c: &Arc<Mutex<Core>>, h: Hardware| {
                if let Ok(mut c) = c.lock() {
                    if c.hardware != h {
                        crate::mac::log(&format!("keypad hardware: {:?} -> {:?}", c.hardware, h));
                        c.hardware = h;
                        c.revision += 1;
                    }
                }
            };
            let set_presence = |presence: Option<bool>| {
                if let Ok(mut c) = core.lock() {
                    if c.keypad_connected != presence {
                        c.keypad_connected = presence;
                        c.revision += 1;
                        wake();
                    }
                }
            };
            let mut interval = 1.0 / 30.0_f64;
            loop {
                let frame_start = Instant::now();
                let (drive, control, fps, brightness) = match core.lock() {
                    Ok(c) => (c.settings.drive_physical_keypad, c.in_control, c.fps(), c.settings.brightness),
                    Err(_) => return,
                };
                if !drive {
                    if let Some(d) = dev.take() {
                        release(d);
                    }
                    connected_at = None;
                    let observe = core.lock().map(|c| matches!(c.settings.island_visibility, crate::model::IslandVisibility::WhenDisconnected | crate::model::IslandVisibility::QuotasAlways)).unwrap_or(false);
                    if observe {
                        if api.is_none() { api = hidapi::HidApi::new().ok(); }
                        if last_scan.elapsed() > Duration::from_secs(2) {
                            last_scan = Instant::now();
                            let found = api.as_mut().map(find);
                            set_presence(found.as_ref().and_then(|(p, refreshed)| refreshed.then_some(p.is_some())));
                        }
                    } else {
                        api = None;
                        set_presence(None);
                    }
                    set(&core, Hardware::Off);
                    std::thread::sleep(Duration::from_millis(500));
                    continue;
                }
                if api.is_none() {
                    api = hidapi::HidApi::new().ok();
                    if api.is_none() {
                        set_presence(None);
                        set(&core, Hardware::Problem("USB access is unavailable".into()));
                        std::thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                }
                let a = api.as_mut().unwrap();
                if dev.is_none() && last_scan.elapsed() > Duration::from_secs(2) {
                    last_scan = Instant::now();
                    let was = path.is_some();
                    let (found, refreshed) = find(a);
                    set_presence(refreshed.then_some(found.is_some()));
                    path = found;
                    if path.is_none() {
                        retries = 0;
                        arrived = None;
                        set(&core, Hardware::NoKeypad);
                    } else {
                        if !was && !first_scan {
                            arrived = Some(Instant::now());
                        }
                        if !control {
                            set(&core, Hardware::Connected);
                        }
                    }
                    first_scan = false;
                }
                match (&mut dev, control) {
                    (Some(_), false) => {
                        // Switch to Options+ (menu, page-button chord, quit).
                        release(dev.take().unwrap());
                        connected_at = None;
                        set(&core, Hardware::Connected);
                    }
                    (None, true) => {
                        let settling = arrived
                            .map(|a| a.elapsed().as_secs_f64() < CONNECTION_QUIET_S.min(CONNECTION_SETTLE_S))
                            .unwrap_or(false);
                        if settling {
                            set(&core, Hardware::Problem("Waiting for Options+ to finish with the keypad before taking over…".into()));
                        }
                        if let (Some(p), false) = (&path, settling) {
                            if Instant::now() >= retry_at {
                                match take_over(a, p, brightness) {
                                    Ok(d) => {
                                        retries = 0;
                                        connected_at = Some(Instant::now());
                                        dev = Some(d);
                                        set_presence(Some(true));
                                        set(&core, Hardware::Driving);
                                    }
                                    Err(e) => {
                                        if retries < MAX_AUTO_RETRIES {
                                            retries += 1;
                                            let delay = retry_delay(retries);
                                            retry_at = Instant::now() + Duration::from_secs_f64(delay);
                                            set(&core, Hardware::Problem(format!("{e} Automatic retry {retries} of {MAX_AUTO_RETRIES} in {delay:.1} s.")));
                                        } else {
                                            retry_at = Instant::now() + Duration::from_secs(3600 * 24);
                                            set(&core, Hardware::Problem(format!("{e} Take over again to retry.")));
                                        }
                                    }
                                }
                            }
                        }
                    }
                    (Some(d), true) => {
                        let result = (|| -> Result<bool, String> {
                            let mut changed = poll_input(d, &core)?;
                            if changed {
                                run_key_actions(&core);
                            }
                            let snapshot = core.lock().ok().map(|mut c| {
                                c.tick();
                                (display_view(&c), c.faces())
                            });
                            if let Some((view, faces)) = snapshot {
                                changed |= send_frame(d, &core, &view, &faces, crate::model::now(), &font)?;
                            }
                            Ok(changed)
                        })();
                        match result {
                            Ok(changed) => {
                                if connected_at.is_some_and(|since| since.elapsed() >= RECOVERY_STABLE_INTERVAL) {
                                    recovery_stage = 0;
                                }
                                if changed {
                                    wake();
                                }
                            },
                            Err(e) => {
                                crate::mac::log(&format!("keypad transport error: {e}"));
                                // Reopen after 2, 5, 15, then 30 s on input or output failure.
                                dev = None;
                                connected_at = None;
                                path = None;
                                let delay = IMAGE_RECOVERY_DELAYS_S[recovery_stage.min(IMAGE_RECOVERY_DELAYS_S.len() - 1)];
                                recovery_stage = recovery_stage.saturating_add(1);
                                retry_at = Instant::now() + Duration::from_secs_f64(delay);
                                set(&core, Hardware::Problem(format!("{e} Trying again in {delay:.0} s.")));
                            }
                        }
                    }
                    (None, false) => {}
                }
                // Steady frame clock: sleep only what's left of this frame's budget. Like
                // FrameLoop.reschedule, the interval only changes when it moves by more than
                // RESCHEDULE_THRESHOLD_S.
                let wanted = 1.0 / fps.max(1) as f64;
                if (wanted - interval).abs() > RESCHEDULE_THRESHOLD_S {
                    interval = wanted;
                }
                let budget = Duration::from_secs_f64(interval);
                std::thread::sleep(budget.saturating_sub(frame_start.elapsed()).max(Duration::from_millis(2)));
            }
        })
        .ok();
}

/// Called on quit: the keypad always goes back to Options+.
pub fn shutdown(core: &Arc<Mutex<Core>>) {
    if let Ok(mut c) = core.lock() {
        c.in_control = false;
    }
    std::thread::sleep(Duration::from_millis(150));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_packet_layout() {
        let jpg = vec![0xAB; 3000];
        let p = packets(&jpg, 4, false);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].len(), PACKET);
        assert_eq!(&p[0][0..4], &[0x14, 0xFF, 0x02, 0x2B]);
        assert_eq!(p[0][4], 0x80 | 0x40 | 0x20 | 1);
        assert_eq!(u16::from_be_bytes([p[0][9], p[0][10]]), 181);
        assert_eq!(u16::from_be_bytes([p[0][11], p[0][12]]), 164);
        assert_eq!(p[0][14], 118);
        assert_eq!(((p[0][17] as usize) << 16) | ((p[0][18] as usize) << 8) | p[0][19] as usize, 3000);
        assert_eq!(p[0][20], 0xAB);
    }

    #[test]
    fn retry_schedule_doubles_to_sixty() {
        let v: Vec<f64> = (1..=MAX_AUTO_RETRIES).map(retry_delay).collect();
        assert_eq!(v, vec![2.0, 4.0, 8.0, 16.0, 32.0, 60.0]);
    }

    #[test]
    fn image_scheduler_visits_each_key_once_then_wraps_fairly() {
        let start = KEY_COUNT - 2;
        let cycle: Vec<_> = (0..KEY_COUNT).map(|offset| scheduled_key(start, offset)).collect();
        let mut expected: Vec<_> = (start..KEY_COUNT).collect();
        expected.extend(0..start);
        assert_eq!(cycle, expected);
        let unique: std::collections::HashSet<_> = cycle.iter().copied().collect();
        assert_eq!(unique.len(), KEY_COUNT);
        assert_eq!(scheduled_key(start, KEY_COUNT), start);
        assert_eq!(scheduled_key(scheduled_key(start, 1), 0), KEY_COUNT - 1);
    }

    #[test]
    fn changed_face_gate_preserves_original_animation_and_state_transitions() {
        let face_for = |state: &str| {
            let mut core = crate::model::Core::new(crate::model::Settings::default());
            core.in_control = true;
            core.apply(crate::model::Incoming {
                agent: "codex".into(), event: None,
                body: serde_json::json!({"session": state, "state": state, "title": state}),
            });
            core.faces()[0].clone()
        };

        let idle = face_for("idle");
        assert!(!face_needs_render(Some(&idle), &idle, false));
        assert!(face_needs_render(Some(&idle), &KeyFace::Empty, false));
        let mut changed_idle = idle.clone();
        if let KeyFace::Session(face) = &mut changed_idle { face.title.push_str(" renamed"); }
        assert!(face_needs_render(Some(&idle), &changed_idle, false));

        for state in ["thinking", "waiting_question", "done"] {
            let face = face_for(state);
            assert!(face_needs_render(Some(&face), &face, false), "{state} should keep its existing animation");
        }
        assert!(!face_needs_render(Some(&idle), &idle, false));
        assert!(face_needs_render(Some(&idle), &idle, true));

        let mut waiting_core = crate::model::Core::new(crate::model::Settings::default());
        waiting_core.in_control = true;
        waiting_core.apply(crate::model::Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"waiting-hub", "state":"waiting_question"}),
        });
        let hub = waiting_core.faces()[crate::model::HUB_KEY].clone();
        assert!(face_needs_render(Some(&hub), &hub, false));
    }

    #[test]
    fn page_change_clears_old_page_retry_deadline_and_render_cache() {
        let first = DisplayView { quota_view: false, quota_page: 0, page: 0, in_control: true, detail: None };
        let second = DisplayView { quota_view: false, quota_page: 0, page: 1, in_control: true, detail: None };
        let future = Instant::now() + Duration::from_secs(30);
        let mut previous = Some(first);
        let mut sent = vec![Some(vec![0xAA]); KEY_COUNT];
        let mut last_faces = vec![Some(KeyFace::Empty); KEY_COUNT];
        let mut retry_at = [Some(future); KEY_COUNT];
        let mut retry_stage = [4; KEY_COUNT];

        assert!(!image_retry_due(retry_at[0], Instant::now()));
        assert!(prepare_device_display_view(&mut previous, &second, &mut sent, &mut last_faces, &mut retry_at, &mut retry_stage));
        assert!(sent.iter().all(Option::is_none));
        assert!(last_faces.iter().all(Option::is_none));
        assert!(retry_at.iter().all(Option::is_none));
        assert!(retry_stage.iter().all(|stage| *stage == 0));
        assert!(image_retry_due(retry_at[0], Instant::now()), "a previous page's rejection must not block the new page");

        retry_at[0] = Some(future);
        retry_stage[0] = 2;
        assert!(!prepare_device_display_view(&mut previous, &second, &mut sent, &mut last_faces, &mut retry_at, &mut retry_stage));
        assert_eq!(retry_at[0], Some(future));
        assert_eq!(retry_stage[0], 2);
    }

    #[test]
    fn rejected_image_retries_back_off_per_attempt_and_respect_the_deadline() {
        let start = Instant::now();
        let mut retry_stage = 0;
        let mut at = start;
        for (delay, expected_stage) in [(2, 1), (5, 2), (15, 3), (30, 4), (30, 5)] {
            let (next_stage, deadline) = rejected_image_retry(retry_stage, at);
            assert_eq!(next_stage, expected_stage);
            assert_eq!(deadline.duration_since(at), Duration::from_secs(delay));
            assert!(!image_retry_due(Some(deadline), deadline - Duration::from_nanos(1)));
            assert!(image_retry_due(Some(deadline), deadline));
            assert!(image_retry_due(None, at));
            retry_stage = next_stage;
            at = deadline;
        }
    }

    #[test]
    fn checked_write_accepts_only_a_complete_report() {
        assert_eq!(checked_write_result(Ok(20), 20), Ok(()));
        let err = checked_write_result(Ok(19), 20).unwrap_err();
        assert!(err.contains("19 of 20"));
        assert_eq!(checked_write_result(Err("disconnected".into()), 20), Err("disconnected".into()));
    }

    #[test]
    fn image_ack_accepts_only_success_for_the_expected_packet_part() {
        let success = [0x13, 0xFF, 0x02, 0x2B, 0xC1, 0x00, 0x01];
        assert_eq!(image_ack(&success, 0xC1), Some(Ok(())));
        assert_eq!(image_ack(&success, 0xC2), None);

        let truncated = [0x13, 0xFF, 0x02, 0x2B, 0xC1, 0x00];
        assert_eq!(image_ack(&truncated, 0xC1), None);
    }

    #[test]
    fn image_ack_rejects_non_success_status_and_wrong_headers() {
        let rejected = [0x13, 0xFF, 0x02, 0x2B, 0xC1, 0x00, 0x00];
        assert_eq!(image_ack(&rejected, 0xC1), Some(Err([0x00, 0x00])));
        let device_error = [0x13, 0xFF, 0x02, 0x2B, 0xC1, 0x01, 0x01];
        assert_eq!(image_ack(&device_error, 0xC1), Some(Err([0x01, 0x01])));

        for report in [
            [0x12, 0xFF, 0x02, 0x2B, 0xC1, 0x00, 0x01],
            [0x13, 0x00, 0x02, 0x2B, 0xC1, 0x00, 0x01],
            [0x13, 0xFF, 0x03, 0x2B, 0xC1, 0x00, 0x01],
            [0x13, 0xFF, 0x02, 0x2A, 0xC1, 0x00, 0x01],
        ] {
            assert_eq!(image_ack(&report, 0xC1), None, "unexpected acknowledgement {report:?}");
        }
    }

    #[test]
    fn image_send_is_skipped_for_identical_bytes_unless_refresh_is_due() {
        let jpg = [0xFF, 0xD8, 0x01, 0x02];
        assert!(image_needs_send(None, &jpg, false));
        assert!(!image_needs_send(Some(&jpg), &jpg, false));
        assert!(image_needs_send(Some(&jpg), &[0xFF, 0xD8, 0x03], false));
        assert!(image_needs_send(Some(&jpg), &jpg, true));
    }

    #[test]
    fn page_transition_invalidates_occupied_cache_so_empty_slot_image_is_sent() {
        let mut previous = Some(DisplayView { quota_view: false, quota_page: 0, page: 0, in_control: true, detail: None });
        let mut sent = vec![Some(vec![0x11, 0x22]); KEY_COUNT];
        let next = DisplayView { quota_view: false, quota_page: 0, page: 1, in_control: true, detail: None };

        assert!(prepare_display_view(&mut previous, &next, &mut sent));
        assert!(sent.iter().all(Option::is_none));
        let blank_slot_jpeg = [0xFF, 0xD8, 0x00, 0xFF, 0xD9];
        assert!(image_needs_send(sent[0].as_deref(), &blank_slot_jpeg, false));
    }

    #[test]
    fn page_transition_forces_resend_even_when_rendered_bytes_are_equal() {
        let mut previous = Some(DisplayView { quota_view: false, quota_page: 0, page: 0, in_control: true, detail: None });
        let same_hub_jpeg = vec![0xFF, 0xD8, 0x42, 0xFF, 0xD9];
        let mut sent = vec![Some(same_hub_jpeg.clone()); KEY_COUNT];
        let next = DisplayView { quota_view: false, quota_page: 0, page: 1, in_control: true, detail: None };

        assert!(prepare_display_view(&mut previous, &next, &mut sent));
        assert!(image_needs_send(sent[crate::model::HUB_KEY].as_deref(), &same_hub_jpeg, false));
        assert!(!prepare_display_view(&mut previous, &next, &mut sent));
    }

    #[test]
    fn aborted_page_transition_cannot_leave_partial_cache_as_current() {
        let first = DisplayView { quota_view: false, quota_page: 0, page: 0, in_control: true, detail: None };
        let second = DisplayView { quota_view: false, quota_page: 0, page: 1, in_control: true, detail: None };
        let third = DisplayView { quota_view: false, quota_page: 0, page: 2, in_control: true, detail: None };
        let mut previous = Some(first);
        let mut sent = vec![Some(vec![0x10]); KEY_COUNT];

        assert!(prepare_display_view(&mut previous, &second, &mut sent));
        // Only one key from this view reaches the device before a new view is observed.
        sent[0] = Some(vec![0x20]);
        assert!(prepare_display_view(&mut previous, &third, &mut sent));
        assert_eq!(previous, Some(third));
        assert!(sent.iter().all(Option::is_none));
    }

    #[test]
    fn multi_packet_flags() {
        let p = packets(&vec![1u8; 6000], 0, false);
        assert_eq!(p.len(), 2);
        assert_eq!(p[0][4] & 0xC0, 0x80);
        assert_eq!(p[1][4] & 0xC0, 0x40);
        assert_eq!(p[1][4] & 0x0F, 2);
    }

    #[test]
    fn key_picture_fits_one_packet() {
        let font = load_font();
        let mut core = crate::model::Core::new(crate::model::Settings::default());
        core.in_control = true;
        core.load_demo();
        for f in core.faces().iter() {
            let pm = render_key(f, crate::model::now(), &font);
            let j = jpeg(&pm);
            assert!(!j.is_empty() && j.len() <= MAX_SINGLE_JPEG, "jpeg {} bytes", j.len());
        }
    }

    #[test]
    fn acknowledged_key_reduces_full_render_intensity() {
        let font = load_font();
        let mut core = crate::model::Core::new(crate::model::Settings::default());
        core.in_control = true;
        core.apply(crate::model::Incoming {
            agent: "codex".into(),
            event: None,
            body: serde_json::json!({"session":"acknowledged-render", "state":"idle", "text":"Finished response"}),
        });
        core.sessions[0].acknowledged = true;
        let mut face = core.faces().iter().find_map(|f| match f {
            KeyFace::Session(face) if face.opacity < 1.0 => Some(face.clone()),
            _ => None,
        }).expect("acknowledged demo session");
        let dim = render_key(&KeyFace::Session(face.clone()), 10.0, &font);
        face.opacity = 1.0;
        let bright = render_key(&KeyFace::Session(face), 10.0, &font);
        let sum = |pm: &Pixmap| -> u64 { pm.data().chunks_exact(4).flat_map(|p| p[..3].iter()).map(|&x| x as u64).sum() };
        let ratio = sum(&dim) as f32 / sum(&bright) as f32;
        assert!((ratio - crate::model::ACKNOWLEDGED_OPACITY).abs() < 0.015, "ratio {ratio}");
    }

    #[test]
    fn unread_tile_differs_from_read_tile_and_keeps_animated_light() {
        let font = load_font();
        let mut core = crate::model::Core::new(crate::model::Settings::default());
        core.in_control = true;
        core.apply(crate::model::Incoming {
            agent: "codex".into(),
            event: None,
            body: serde_json::json!({"session":"unread-animation", "state":"thinking", "_unread":2}),
        });
        let face = match &core.faces()[0] {
            KeyFace::Session(face) => face.clone(),
            other => panic!("expected session face, got {other:?}"),
        };
        assert_eq!(face.unread, 2);
        let mut read_face = face.clone();
        read_face.unread = 0;

        let unread_at = render_key(&KeyFace::Session(face.clone()), 1.1, &font);
        let read_at = render_key(&KeyFace::Session(read_face), 1.1, &font);
        assert_ne!(unread_at.data(), read_at.data(), "unread state must be visible in the device render");
        let unread_later = render_key(&KeyFace::Session(face), 1.7, &font);
        assert_ne!(unread_at.data(), unread_later.data(), "the unread background must not freeze the animated light");
    }

    #[test]
    fn unchanged_idle_faces_are_cached_but_original_activity_animations_continue() {
        let face_for = |state: &str| {
            let mut core = crate::model::Core::new(crate::model::Settings::default());
            core.in_control = true;
            core.apply(crate::model::Incoming {
                agent: "codex".into(), event: None,
                body: serde_json::json!({"session": state, "state": state, "title": state}),
            });
            core.faces()[0].clone()
        };

        let idle = face_for("idle");
        assert!(!face_needs_render(Some(&idle), &idle, false));
        assert!(face_needs_render(Some(&idle), &KeyFace::Empty, false));
        let mut changed_idle = idle.clone();
        if let KeyFace::Session(face) = &mut changed_idle { face.title.push_str(" renamed"); }
        assert!(face_needs_render(Some(&idle), &changed_idle, false));
        for state in ["thinking", "waiting_question", "done"] {
            let animated = face_for(state);
            assert!(face_needs_render(Some(&animated), &animated, false), "{state} stays animated");
        }
        assert!(face_needs_render(Some(&idle), &idle, true));

        let mut core = crate::model::Core::new(crate::model::Settings::default());
        core.in_control = true;
        core.apply(crate::model::Incoming {
            agent: "codex".into(), event: None,
            body: serde_json::json!({"session":"waiting-hub", "state":"waiting_question"}),
        });
        let hub = core.faces()[crate::model::HUB_KEY].clone();
        assert!(face_needs_render(Some(&hub), &hub, false), "a waiting hub remains animated");
    }

    #[test]
    fn quota_render_changes_when_available_percentage_changes() {
        let font = load_font();
        let make = |remaining| KeyFace::Quota(crate::quota::Quota {
            provider: "Codex".into(), window: "Weekly".into(), remaining: Some(remaining), reset: None,
            reset_text: Some("Resets tomorrow".into()), updated: Some(crate::quota::observed_now()), source: "test".into(),
        });
        let low = render_key(&make(37.0), 10.0, &font);
        let high = render_key(&make(74.0), 10.0, &font);
        assert_ne!(low.data(), high.data(), "the physical quota meter must reflect a changed available percentage");
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    #[ignore]
    fn frame_cost() {
        let font = load_font();
        let mut c = crate::model::Core::new(crate::model::Settings::default());
        for (i, st) in ["thinking", "streaming", "tool", "waiting_permission", "idle", "done", "thinking", "compacting"].iter().enumerate() {
            c.apply(crate::model::Incoming { agent: "codex".into(), event: None, body: serde_json::json!({"session": format!("s{i}"), "state": st, "text": "Rebuilding the hero with the new glass cards and a longer line"}) });
        }
        c.in_control = true;
        let faces = c.faces();
        let t0 = Instant::now();
        let mut bytes = 0;
        for n in 0..10 {
            for f in faces.iter() {
                let pm = render_key(f, n as f64 * 0.033, &font);
                bytes += jpeg(&pm).len();
            }
        }
        eprintln!("frame (9 keys): {:.1} ms, {} bytes/frame", t0.elapsed().as_secs_f64() * 100.0, bytes / 10);
    }
}

#[cfg(test)]
mod preview {
    use super::*;
    /// Writes the keypad's key images to AGENTPAD_KEYS_DIR to look at them.
    #[test]
    #[ignore]
    fn save_keys() {
        let Ok(dir) = std::env::var("AGENTPAD_KEYS_DIR") else { return };
        let font = load_font();
        let mut c = crate::model::Core::new(crate::model::Settings::default());
        for (i, st) in ["thinking", "streaming", "tool", "waiting_permission", "idle", "done", "compacting", "error"].iter().enumerate() {
            c.apply(crate::model::Incoming { agent: "claude-code".into(), event: None, body: serde_json::json!({"session": format!("s{i}"), "state": st, "title": format!("api-server {i}"), "text": "Tracing the 502s back to the pool"}) });
        }
        c.in_control = true;
        for (i, f) in c.faces().iter().enumerate() {
            let pm = render_key(f, 2.3, &font);
            let _ = std::fs::write(format!("{dir}/key{i}.jpg"), jpeg(&pm));
        }
    }
}
