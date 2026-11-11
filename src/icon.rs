//! Draws the AgentPad app icon: a dark glass slab with a 3×3 keypad of glowing orbs.

use crate::colour::{self, Rgb};
use tiny_skia::{Color, FillRule, GradientStop, LinearGradient, Paint, PathBuilder, Pixmap, Point, RadialGradient, SpreadMode, Transform};

fn rounded(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<tiny_skia::Path> {
    let mut pb = PathBuilder::new();
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.quad_to(x + w, y, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.quad_to(x + w, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.quad_to(x, y + h, x, y + h - r);
    pb.line_to(x, y + r);
    pb.quad_to(x, y, x + r, y);
    pb.close();
    pb.finish()
}

fn rgba(c: Rgb, a: f32) -> Color {
    Color::from_rgba8(c.0, c.1, c.2, (a.clamp(0.0, 1.0) * 255.0) as u8)
}

fn vertical(top: Color, bottom: Color, y0: f32, y1: f32) -> Option<tiny_skia::Shader<'static>> {
    LinearGradient::new(
        Point::from_xy(0.0, y0),
        Point::from_xy(0.0, y1),
        vec![GradientStop::new(0.0, top), GradientStop::new(1.0, bottom)],
        SpreadMode::Pad,
        Transform::identity(),
    )
}

pub fn render(size: u32) -> Pixmap {
    let s = size as f32;
    let mut pm = Pixmap::new(size, size).expect("pixmap");
    // macOS icon grid: the body leaves a 10% margin.
    let m = s * 0.1;
    let body = s - 2.0 * m;
    if let (Some(path), Some(shader)) = (
        rounded(m, m, body, body, body * 0.225),
        vertical(Color::from_rgba8(46, 48, 62, 255), Color::from_rgba8(14, 15, 22, 255), m, m + body),
    ) {
        let paint = Paint { shader, anti_alias: true, ..Paint::default() };
        pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
    }
    let orbs = [
        colour::hex_u32(0xd9693e), // waiting
        colour::hex_u32(0xd9a441), // Claude
        colour::oklch(0.75, 0.13, 270.0), // Codex
        colour::oklch(0.75, 0.13, 225.0),
        colour::hex_u32(0xc9b38e), // done
        colour::oklch(0.75, 0.13, 324.0),
        colour::oklch(0.75, 0.13, 165.0),
        colour::hex_u32(0xe0b35a),
        Rgb(240, 240, 245), // hub
    ];
    let pad = body * 0.12;
    let gap = body * 0.045;
    let key = (body - 2.0 * pad - 2.0 * gap) / 3.0;
    for (i, c) in orbs.iter().enumerate() {
        let x = m + pad + (i % 3) as f32 * (key + gap);
        let y = m + pad + (i / 3) as f32 * (key + gap);
        if let (Some(path), Some(shader)) = (
            rounded(x, y, key, key, key * 0.22),
            vertical(Color::from_rgba8(255, 255, 255, 40), Color::from_rgba8(255, 255, 255, 12), y, y + key),
        ) {
            let paint = Paint { shader, anti_alias: true, ..Paint::default() };
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
        let (cx, cy) = (x + key / 2.0, y + key / 2.0);
        let stops = vec![
            GradientStop::new(0.0, rgba(*c, 0.85)),
            GradientStop::new(0.4, rgba(*c, 0.35)),
            GradientStop::new(1.0, rgba(*c, 0.0)),
        ];
        if let (Some(shader), Some(path)) = (
            RadialGradient::new(Point::from_xy(cx, cy), Point::from_xy(cx, cy), key * 0.5, stops, SpreadMode::Pad, Transform::identity()),
            PathBuilder::from_circle(cx, cy, key * 0.5),
        ) {
            let paint = Paint { shader, anti_alias: true, ..Paint::default() };
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
        if let Some(path) = PathBuilder::from_circle(cx, cy, key * 0.19) {
            let mut paint = Paint::default();
            paint.set_color(rgba(*c, 1.0));
            paint.anti_alias = true;
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
        if let Some(path) = PathBuilder::from_circle(cx - key * 0.06, cy - key * 0.07, key * 0.055) {
            let mut paint = Paint::default();
            paint.set_color(Color::from_rgba8(255, 255, 255, 150));
            paint.anti_alias = true;
            pm.fill_path(&path, &paint, FillRule::Winding, Transform::identity(), None);
        }
    }
    pm
}

pub fn save(path: &str, size: u32) -> Result<(), String> {
    let pm = render(size);
    // Pixmap data is premultiplied; PNG wants straight alpha.
    let rgba: Vec<u8> = pm
        .data()
        .chunks(4)
        .flat_map(|p| {
            let a = p[3] as f32 / 255.0;
            let un = |c: u8| if a > 0.0 { (c as f32 / a).round().min(255.0) as u8 } else { 0 };
            [un(p[0]), un(p[1]), un(p[2]), p[3]]
        })
        .collect();
    image::save_buffer(path, &rgba, size, size, image::ExtendedColorType::Rgba8).map_err(|e| e.to_string())
}
