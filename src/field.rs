//! Animated light field behind each session key: core, halo, rings, moving lights, ripple
//! and grain. Coordinates use key units with y measured up from the bottom.

use crate::model::{Activity, ToolKind};
use std::f32::consts::TAU;

#[derive(Clone, Copy, Debug, Default)]
pub struct Field {
    pub cx: f32,
    pub cy: f32,
    pub cs: f32,
    pub ci: f32,
    pub hs: f32,
    pub hi: f32,
    pub floor: f32,
    pub rr: f32,
    pub rw: f32,
    pub ri: f32,
    pub r2r: f32,
    pub r2w: f32,
    pub r2i: f32,
    pub l1: (f32, f32, f32, f32),
    pub l2: (f32, f32, f32, f32),
    pub ripple: f32,
    pub grain: f32,
    pub grain_rate: f32,
}

/// Defaults of every state (the switch's starting registers, 0x10052f440..0x10052f4c4).
fn base() -> Field {
    Field {
        cx: 0.5,
        cy: 0.84,
        cs: 0.12,
        ci: 0.0,
        hs: 0.36,
        hi: 0.0,
        floor: 0.0,
        rr: 0.0,
        rw: 0.05,
        ri: 0.0,
        r2r: 0.0,
        r2w: 0.05,
        r2i: 0.0,
        l1: (0.5, 0.84, 0.1, 0.0),
        l2: (0.5, 0.84, 0.1, 0.0),
        ripple: 0.0,
        grain: 0.0,
        grain_rate: 0.2,
    }
}

fn wave(t: f32, period: f32) -> f32 {
    (t * TAU / period).sin()
}

fn frac(x: f32) -> f32 {
    x - x.floor()
}

/// `t` is the clock in seconds, `age` the time since the state began.
pub fn params(activity: Activity, tool: Option<ToolKind>, t: f32, age: f32, reduced: bool) -> Field {
    let t = if reduced { 0.0 } else { t };
    let mut f = base();
    match activity {
        Activity::Empty => {}
        Activity::Offline => {
            f.rr = 0.13;
            f.rw = 0.032;
            f.ri = 0.55;
        }
        Activity::Idle => {
            // legacyParams idle case (0x10052f91c): phase = 0.13·t·2π, x = 0.5 − 0.5·sin,
            // k = 0.9 + 0.1·x²(3−2x); (ci, hi) = (0.62, 0.18)·k (vector at 0x1009f15f8);
            // the core drifts sideways by 0.015 of value noise (0x10052fc54).
            f.cs = 0.1;
            f.hs = 0.3;
            let x = 0.5 - 0.5 * (t * 0.13 * TAU).sin();
            let k = 0.9 + 0.1 * x * x * (3.0 - 2.0 * x);
            f.ci = 0.62 * k;
            f.hi = 0.18 * k;
            f.cx = 0.5 + 0.015 * (2.0 * smooth_noise(t * 0.05) - 1.0);
        }
        Activity::Starting => {
            // Fades in over 1.4 s while a ring opens every 1.6 s.
            let p = (age / 1.4).min(1.0);
            f.cs = 0.08 + 0.04 * p;
            f.ci = 0.85 * p;
            f.hs = 0.34;
            f.hi = 0.3 * p;
            let r = frac(t / 1.6);
            f.rr = 0.12 + 0.3 * r;
            f.rw = 0.05;
            f.ri = 0.2 * (1.0 - r);
        }
        Activity::Thinking => {
            // 3.2 s pulse; two lights circling at 5.3 s and 11 s.
            f.ci = 0.88 + 0.07 * wave(t, 3.2);
            f.hi = 0.3;
            let a1 = t * TAU / 5.3;
            let a2 = -t * TAU / 11.0 + 2.1;
            f.l1 = (0.5 + 0.15 * a1.cos(), 0.84 + 0.12 * a1.sin(), 0.1, 0.3);
            f.l2 = (0.5 + 0.17 * a2.cos(), 0.84 + 0.102 * a2.sin(), 0.11, 0.26);
            f.grain = 0.1;
            f.grain_rate = 0.22;
        }
        Activity::Tool => {
            // Tighter core; two small lights move by tool kind (ToolMotion.frame).
            f.cs = 0.095;
            f.ci = 0.95;
            f.hs = 0.32;
            f.hi = 0.36;
            let (p1, p2) = match tool.unwrap_or(ToolKind::Read) {
                ToolKind::Shell => {
                    let a = t * TAU / 0.57;
                    ((0.5 + 0.11 * a.cos(), 0.84 + 0.11 * a.sin()), (0.5 - 0.11 * a.cos(), 0.84 - 0.11 * a.sin()))
                }
                ToolKind::Edit => {
                    let s = wave(t, 1.2);
                    ((0.5 + 0.12 * s, 0.84), (0.5 - 0.12 * s, 0.84))
                }
                ToolKind::Read => {
                    let s = wave(t, 1.6);
                    ((0.5 + 0.14 * s, 0.84 + 0.0624), (0.5 + 0.14 * s, 0.84 - 0.0624))
                }
                ToolKind::Web => {
                    let r = 0.06 + 0.08 * frac(t / 1.2);
                    ((0.5 + r, 0.84), (0.5 - r, 0.84))
                }
                ToolKind::Delegate => {
                    let a = t * TAU / 1.4;
                    ((0.5 + 0.12 * a.cos(), 0.84 + 0.06 * a.sin()), (0.5 + 0.12 * (a + 3.14).cos(), 0.84 + 0.06 * (a + 3.14).sin()))
                }
            };
            f.l1 = (p1.0, p1.1, 0.052, 0.57);
            f.l2 = (p2.0, p2.1, 0.062, 0.57);
        }
        Activity::Streaming => {
            f.cs = 0.13;
            f.ci = 0.9;
            f.hs = 0.37;
            f.hi = 0.36;
            f.ripple = 0.2;
            f.grain = 0.05;
            f.grain_rate = 0.15;
        }
        Activity::WaitingPermission => {
            // Beacon: breathes every 1.6 s and sends out two rings half a period apart.
            let env = 0.5 + 0.5 * wave(t, 1.6);
            f.cs = 0.12 + 0.02 * env;
            f.ci = 0.72 + 0.26 * env;
            f.hs = 0.36 + 0.04 * env;
            f.hi = 0.45;
            f.floor = 0.06 * env;
            let r1 = frac(t / 1.6);
            let r2 = frac(t / 1.6 + 0.5);
            f.rr = 0.12 + 0.33 * r1;
            f.rw = 0.06 + 0.08 * r1;
            f.ri = 0.5 * (1.0 - r1);
            f.r2r = 0.12 + 0.33 * r2;
            f.r2w = 0.06 + 0.08 * r2;
            f.r2i = 0.5 * (1.0 - r2);
        }
        Activity::WaitingQuestion => {
            // Softer bob with one ring between 0.12 and 0.24.
            let env = 0.5 + 0.5 * wave(t, 2.0);
            f.cs = 0.115 + 0.01 * env;
            f.ci = 0.65 + 0.2 * env;
            f.hs = 0.34;
            f.hi = 0.35 + 0.1 * env;
            f.floor = 0.05 * env;
            f.rr = 0.12 + 0.12 * env;
            f.rw = 0.08;
            f.ri = 0.24 * (1.0 - env) + 0.1;
        }
        Activity::Done => {
            // A ring bursts out over 1.4 s, then the light settles.
            let burst = (-age / 0.4).exp();
            f.cs = 0.13;
            f.ci = 0.8 + 0.2 * burst;
            f.hs = 0.38;
            f.hi = 0.22 + 0.2 * burst;
            if age < 1.4 {
                f.rr = 0.13 + 0.25 * (1.0 - burst);
                f.rw = 0.04;
                f.ri = burst;
            }
            f.grain = 0.03;
            f.grain_rate = 0.04;
        }
        Activity::Error => {
            f.ci = 0.8;
            f.hs = 0.34;
            f.hi = 0.3;
            f.floor = 0.18;
            f.grain = 0.06;
            f.grain_rate = 0.5;
        }
        Activity::Compacting => {
            // A ring closes in every 2.4 s.
            let r = frac(t / 2.4);
            f.cs = 0.11;
            f.ci = 0.6 + 0.1 * wave(t, 2.4);
            f.hs = 0.36;
            f.hi = 0.26;
            f.rr = 0.45 - 0.33 * r;
            f.rw = 0.07;
            f.ri = 0.35 * r;
        }
    }
    f
}

/// 1-D value noise with a quintic fade (6t⁵−15t⁴+10t³) for idle drift.
fn smooth_noise(x: f32) -> f32 {
    let i = x.floor();
    let f = x - i;
    let u = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = hash(i as i32 as u32, 17);
    let b = hash((i as i32 + 1) as u32, 17);
    a + (b - a) * u
}

/// Scale for light sizes and their distance from the core.
pub const ANIM_SCALE: f32 = 1.5;

impl Field {
    pub fn scaled(mut self, s: f32) -> Field {
        self.cs *= s;
        self.hs *= s;
        self.rr *= s;
        self.rw *= s;
        self.r2r *= s;
        self.r2w *= s;
        for l in [&mut self.l1, &mut self.l2] {
            l.0 = self.cx + (l.0 - self.cx) * s;
            l.1 = self.cy + (l.1 - self.cy) * s;
            l.2 *= s;
        }
        self
    }
}

/// Cheap repeatable noise for the grain sparkle.
pub fn hash(i: u32, j: u32) -> f32 {
    let mut h = i.wrapping_mul(374_761_393) ^ j.wrapping_mul(668_265_263);
    h = (h ^ (h >> 13)).wrapping_mul(1_274_126_177);
    (h ^ (h >> 16)) as f32 / u32::MAX as f32
}
