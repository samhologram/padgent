//! Colour maths: agent hues are degrees on the OKLCH wheel (see MECHANICS.md).

#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    pub fn parse(hex: &str) -> Option<Rgb> {
        let h = hex.trim().trim_start_matches('#');
        if h.len() != 6 || !h.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let v = u32::from_str_radix(h, 16).ok()?;
        Some(Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
    }

    pub fn mix(self, other: Rgb, t: f32) -> Rgb {
        let t = t.clamp(0.0, 1.0);
        let m = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
        Rgb(m(self.0, other.0), m(self.1, other.1), m(self.2, other.2))
    }

    pub fn scale(self, k: f32) -> Rgb {
        let s = |a: u8| (a as f32 * k).clamp(0.0, 255.0) as u8;
        Rgb(s(self.0), s(self.1), s(self.2))
    }
}

fn oklab_to_linear(l: f32, a: f32, b: f32) -> [f32; 3] {
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
}

fn encode(c: f32) -> u8 {
    let c = c.clamp(0.0, 1.0);
    let v = if c <= 0.003_130_8 { 12.92 * c } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    (v * 255.0).round() as u8
}

/// OKLCH to sRGB; chroma is reduced until the colour fits the sRGB gamut.
pub fn oklch(l: f32, c: f32, hue_deg: f32) -> Rgb {
    let h = hue_deg.to_radians();
    let mut chroma = c;
    for _ in 0..24 {
        let rgb = oklab_to_linear(l, chroma * h.cos(), chroma * h.sin());
        if rgb.iter().all(|v| (-0.0005..=1.0005).contains(v)) {
            return Rgb(encode(rgb[0]), encode(rgb[1]), encode(rgb[2]));
        }
        chroma *= 0.88;
    }
    let rgb = oklab_to_linear(l, 0.0, 0.0);
    Rgb(encode(rgb[0]), encode(rgb[1]), encode(rgb[2]))
}

/// Hues 18.9..55 are reserved for the waiting glow (AdapterPaletteSpec.attentionExclusion).
pub const ATTENTION_BAND: (f32, f32) = (18.9, 55.0);

pub fn keep_out_of_attention_band(hue: f32) -> (f32, bool) {
    let h = hue.rem_euclid(360.0);
    let (lo, hi) = ATTENTION_BAND;
    if h > lo && h < hi {
        let mid = (lo + hi) / 2.0;
        (if h < mid { lo } else { hi }, true)
    } else {
        (h, false)
    }
}

/// Hues 80..280 are the calm arc used for working states (AdapterPaletteSpec.calmArc).
pub const CALM_ARC: (f32, f32) = (80.0, 280.0);

/// Default hue for a new or imported agent without a usable colour: the middle of the calm arc.
pub fn calm_default() -> f32 {
    (CALM_ARC.0 + CALM_ARC.1) / 2.0
}

/// Minimum contrast for marks drawn on a key colour (OrbPalette.generate 0x100438d50 checks 4.5:1
/// with the WCAG luminance weights 0.2126 / 0.7152 / 0.0722).
pub const MIN_CONTRAST: f32 = 4.5;

pub fn luminance(c: Rgb) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(c.0) + 0.7152 * lin(c.1) + 0.0722 * lin(c.2)
}

pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Dark ink on a light colour, white on a dark one; dark wins when it reaches 4.5:1.
pub fn ink_on(bg: Rgb) -> Rgb {
    let dark = Rgb(10, 12, 18);
    if contrast(bg, dark) >= MIN_CONTRAST || contrast(bg, dark) >= contrast(bg, Rgb(255, 255, 255)) { dark } else { Rgb(255, 255, 255) }
}

fn lin(v: u8) -> f32 {
    let v = v as f32 / 255.0;
    if v <= 0.040_45 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
}

/// sRGB to OKLCH (L, C, hue in degrees).
pub fn to_oklch(c: Rgb) -> (f32, f32, f32) {
    let (r, g, b) = (lin(c.0), lin(c.1), lin(c.2));
    let l = (0.412_221_47 * r + 0.536_332_54 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_84 * g + 0.629_978_7 * b).cbrt();
    let ll = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let bb = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    (ll, a.hypot(bb), bb.atan2(a).to_degrees().rem_euclid(360.0))
}

/// Base hue of the reference palette (OrbPalette.reference): its "thinking" orb #d9a441.
pub const REFERENCE_HUE: f32 = 79.85;
/// Hue of the reference waiting orb #d9693e.
pub const REFERENCE_WAITING_HUE: f32 = 40.8;
/// Hue of the reference error orb #9e2a22 (OKLCH L 0.467, C 0.154): error always stays red.
pub const ERROR_HUE: f32 = 28.5;

/// Reference ramps, dark to light (static arrays read by OrbPalette.reference, 0x100431480,
/// at 0x100b933f8..0x100b93670). Stop 2 is the lit orb, stop 3 its glow.
pub fn reference_ramp(state: &str) -> [u32; 5] {
    match state {
        "starting" => [0x17110a, 0x3d2a16, 0x8a6435, 0xc9a466, 0xead6a8],
        "idle" => [0x1e170c, 0x5a4620, 0xb89660, 0xefd9a8, 0xfff6e4],
        "thinking" => [0x221408, 0x6b4416, 0xd9a441, 0xf0c46a, 0xfff1d0],
        "tool" => [0x221408, 0x6e4816, 0xdfa946, 0xf4cb72, 0xfff4d6],
        "streaming" => [0x241a0c, 0x7c5a22, 0xe0b35a, 0xf5d78f, 0xfff8e6],
        "waitingPermission" => [0x240c05, 0x6a2612, 0xd9693e, 0xf08a5a, 0xffd6b8],
        "waitingQuestion" => [0x241005, 0x6e3016, 0xe0703f, 0xf29a5e, 0xffdcb8],
        "done" => [0x1c1610, 0x5a4c38, 0xc9b38e, 0xefe3cf, 0xfffaf0],
        "error" => [0x1e0605, 0x4e100d, 0x9e2a22, 0xc8352b, 0xf07a60],
        "compacting" => [0x18140f, 0x4a4030, 0xa8966e, 0xdccaa0, 0xf4ecd8],
        _ => [0x14110f, 0x2c2723, 0x4e463d, 0x6a6157, 0x6a6157],
    }
}

pub fn hex_u32(v: u32) -> Rgb {
    Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// A reference colour turned by `delta` degrees on the OKLCH wheel, keeping its lightness
/// and chroma (how OrbPalette.generate derives an agent's palette from the reference).
pub fn rotate(c: Rgb, delta: f32) -> Rgb {
    if delta.abs() < 0.01 {
        return c;
    }
    let (l, ch, h) = to_oklch(c);
    oklch(l, ch, h + delta)
}
