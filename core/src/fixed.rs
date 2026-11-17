//! Fixed-point arithmetic and the hinting graphics state.
//!
//! TrueType bytecode works in three number spaces:
//! - FUnits: integer font design coordinates from `glyf`
//! - F26Dot6: 26.6 fixed point pixel coordinates used by the interpreter
//! - F2Dot14: signed normalized values (default freedom/delta vectors)
//!
//! This module provides the conversions and the round-state machinery shared
//! by ROUND/NROUND/SROUND/JROT/JROF and the MDRP/MIRP families.

use crate::glyph::GlyphPoint;

pub const ONE: i32 = 64; // 1.0 in 26.6
const HALF: i32 = 32;

/// Rounding phase/period engine derived from SROUND/S45ROUND parameters.
#[derive(Clone, Copy, Default)]
pub struct RoundState {
    /// Rounding period (F26Dot6), 64 for grid rounding.
    pub period: i32,
    /// Phase offset (F26Dot6).
    pub phase: i32,
    /// Threshold as a fraction of the period (0..1 encoded in 2.14).
    pub threshold: i32,
}

impl RoundState {
    pub fn grid() -> Self {
        RoundState {
            period: ONE,
            phase: 0,
            threshold: 0x2000, // 0.5 in F2Dot14
        }
    }

    pub fn half_grid() -> Self {
        RoundState {
            period: ONE,
            phase: HALF,
            threshold: 0x2000,
        }
    }

    pub fn no_round() -> Self {
        RoundState {
            period: 0,
            phase: 0,
            threshold: 0,
        }
    }

    /// Decode the packed argument of SROUND (opcode 0x76).
    pub fn from_sround(value: i32) -> Self {
        let mode = ((value >> 8) & 0x3) as u8; // bits 9-8
        let period_type = ((value >> 4) & 0x3) as u8; // bits 5-4
        let phase_type = (value & 0x3) as u8; // bits 1-0
        let period = round_period(period_type);
        let phase = round_phase(phase_type, period);
        let threshold = match mode {
            0 => 0x2000, // half grid
            1 => 0x2000, // half period
            2 => ((value >> 16) & 0xFFFF) as i32, // explicit F2Dot14
            _ => 0x1333, // 3/8 period (0.375)
        };
        RoundState {
            period,
            phase,
            threshold,
        }
    }

    /// Round `value` (F26Dot6) according to this state. A zero period means
    /// rounding is disabled and the value passes through unchanged.
    pub fn round(&self, value: i32) -> i32 {
        if self.period == 0 {
            return value;
        }
        let p = self.period as i64;
        let shifted = value as i64 - self.phase as i64;
        let k = shifted.div_euclid(p);
        let rem = shifted.rem_euclid(p);
        let limit = (p * self.threshold as i64) / 0x4000;
        let rounded = if rem < limit {
            k * p
        } else if rem > p - limit {
            (k + 1) * p
        } else {
            // Inside the dead zone: round to nearest of the two half-periods.
            (2 * k + 1) * p / 2
        };
        (rounded + self.phase as i64) as i32
    }

    /// "No round" variant (NROUND family): same grid math but the result is
    /// returned without applying the final rounding — used when an instruction
    /// wants the rounding distance rather than the rounded coordinate.
    pub fn nround_distance(&self, value: i32) -> i32 {
        self.round(value) - value
    }
}

fn round_period(pt: u8) -> i32 {
    match pt {
        0 => ONE,           // 1 px
        1 => ONE,           // 1 px (same)
        2 => HALF,          // 0.5 px
        _ => HALF / 2,      // 0.25 px
    }
}

fn round_phase(pt: u8, period: i32) -> i32 {
    match pt {
        0 => 0,
        1 => HALF,          // half pixel
        2 => period / 2,    // half period
        _ => period / 4,    // quarter period
    }
}

/// Scale an integer FUnit distance into F26Dot6 pixels.
///
/// `scale_factor` is the pixels-per-FUnit ratio in 16.16 fixed point. The
/// F26Dot6 result needs an extra factor of 64, i.e. a left shift of 6 on top
/// of the 16.16 multiply, for a net right shift of 10.
#[inline]
pub fn scale(funit: i32, scale_factor: i32) -> i32 {
    (((funit as i64) * (scale_factor as i64)) >> 10) as i32
}

/// Multiply two values where `b` is interpreted as 16.16.
#[inline]
pub fn mul_fixed(a: i32, b: i32) -> i32 {
    (((a as i64) * (b as i64)) >> 16) as i32
}

/// Multiply an F26Dot6 value by an F2Dot14 normalized value.
#[inline]
pub fn mul_f2dot14(a: i32, b: i16) -> i32 {
    (((a as i64) * (b as i64)) >> 14) as i32
}

#[inline]
pub fn f26dot6(value: i32) -> i32 {
    value << 6
}

#[inline]
pub fn to_pixels(value: i32) -> f64 {
    value as f64 / 64.0
}

/// A 2D vector in F26Dot6 (unit vectors are stored as F2Dot14 components).
#[derive(Clone, Copy, Default)]
pub struct F2Dot14Vec {
    pub x: i16,
    pub y: i16,
}

impl F2Dot14Vec {
    pub const X: F2Dot14Vec = F2Dot14Vec { x: 0x4000, y: 0 };
    pub const Y: F2Dot14Vec = F2Dot14Vec { x: 0, y: 0x4000 };

    #[inline]
    pub fn dot(&self, dx: i32, dy: i32) -> i32 {
        mul_f2dot14(dx, self.x) + mul_f2dot14(dy, self.y)
    }
}

/// Hint-time point: glyph points plus the twilight zone, in F26Dot6 pixels.
#[derive(Clone, Copy, Default)]
pub struct HintPoint {
    pub x: i32,
    pub y: i32,
    pub flags: u8,
}

impl HintPoint {
    #[inline]
    pub fn touched_x(&self) -> bool {
        self.flags & crate::glyph::FLAG_TOUCH_X != 0
    }
    #[inline]
    pub fn touched_y(&self) -> bool {
        self.flags & crate::glyph::FLAG_TOUCH_Y != 0
    }
}

/// Scale an outline's points from integer FUnits to F26Dot6 pixels, keeping
/// the on-curve/touch flags.
pub fn scale_points(points: &[GlyphPoint], scale_x: i32, scale_y: i32) -> Vec<HintPoint> {
    points
        .iter()
        .map(|p| HintPoint {
            x: scale(p.x, scale_x),
            y: scale(p.y, scale_y),
            flags: p.flags,
        })
        .collect()
}
