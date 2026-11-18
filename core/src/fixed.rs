//! TrueType 定点运算。
//!
//! 微调解释器与像素坐标统一使用 26.6 定点(i32,低 6 位为小数);
//! 变换矩阵的系数使用 2.14(i16)。

use core::ops::{Add, AddAssign, Mul, Neg, Sub, SubAssign};

/// 26.6 定点数:1.0 像素 = 64。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct F26Dot6(pub i32);

/// 2.14 定点数:1.0 = 0x4000,用于复合字形变换。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default)]
pub struct F2Dot14(pub i16);

impl F26Dot6 {
    pub const ZERO: F26Dot6 = F26Dot6(0);
    pub const ONE: F26Dot6 = F26Dot6(64);
    pub const HALF: F26Dot6 = F26Dot6(32);

    #[inline]
    pub const fn from_i32(v: i32) -> Self {
        F26Dot6(v * 64)
    }

    /// 字体单位(font units)按缩放系数 `scale = ppem * 64 / units_per_em`
    /// 转成 26.6 像素值。
    #[inline]
    pub const fn scale_font_units(units: i32, scale: i32) -> Self {
        // units * scale / 64,带四舍五入
        let prod = units as i64 * scale as i64;
        let q = if prod >= 0 {
            (prod + 32) >> 6
        } else {
            -(((-prod) + 32) >> 6)
        };
        F26Dot6(q as i32)
    }

    #[inline]
    pub const fn raw(self) -> i32 {
        self.0
    }

    #[inline]
    pub const fn floor(self) -> Self {
        F26Dot6(self.0 & !63)
    }

    #[inline]
    pub const fn round(self) -> Self {
        // 银行家无关的普通四舍五入(向远离零方向,正数即 +0.5 取整)
        let v = self.0;
        if v >= 0 {
            F26Dot6((v + 32) & !63)
        } else {
            F26Dot6(-(((-v) + 32) & !63))
        }
    }

    #[inline]
    pub const fn to_f32(self) -> f32 {
        self.0 as f32 / 64.0
    }

    /// 截成整数像素(向零取整)。
    #[inline]
    pub const fn trunc(self) -> i32 {
        self.0 >> 6
    }
}

impl Add for F26Dot6 {
    type Output = F26Dot6;
    #[inline]
    fn add(self, rhs: F26Dot6) -> F26Dot6 {
        F26Dot6(self.0 + rhs.0)
    }
}
impl Sub for F26Dot6 {
    type Output = F26Dot6;
    #[inline]
    fn sub(self, rhs: F26Dot6) -> F26Dot6 {
        F26Dot6(self.0 - rhs.0)
    }
}
impl AddAssign for F26Dot6 {
    #[inline]
    fn add_assign(&mut self, rhs: F26Dot6) {
        self.0 += rhs.0;
    }
}
impl SubAssign for F26Dot6 {
    #[inline]
    fn sub_assign(&mut self, rhs: F26Dot6) {
        self.0 -= rhs.0;
    }
}
impl Neg for F26Dot6 {
    type Output = F26Dot6;
    #[inline]
    fn neg(self) -> F26Dot6 {
        F26Dot6(-self.0)
    }
}
impl Mul<i32> for F26Dot6 {
    type Output = F26Dot6;
    #[inline]
    fn mul(self, rhs: i32) -> F26Dot6 {
        F26Dot6(self.0 * rhs)
    }
}

impl F2Dot14 {
    #[inline]
    pub const fn to_f32(self) -> f32 {
        self.0 as f32 / 16384.0
    }

    /// 2.14 × 26.6 -> 26.6(中间用 i64 防溢出)。
    #[inline]
    pub const fn apply(self, v: F26Dot6) -> F26Dot6 {
        let prod = self.0 as i64 * v.0 as i64;
        // 带四舍五入的 /16384
        let q = if prod >= 0 {
            (prod + 8192) >> 14
        } else {
            -(((-prod) + 8192) >> 14)
        };
        F26Dot6(q as i32)
    }
}

/// 通用 TrueType 舍入:依据周期 / 相位 / 阈值(均为 26.6)。
///
/// 对应规范里 round() 的定义:
/// `n = floor((distance - phase) / period + threshold) * period + phase`
/// 负数分支对称处理。
#[inline]
pub fn tt_round(distance: F26Dot6, period: F26Dot6, phase: F26Dot6, threshold: F26Dot6) -> F26Dot6 {
    if period.0 <= 0 {
        return distance;
    }
    let d = distance.0 - phase.0;
    let p = period.0 as i64;
    let t = threshold.0 as i64;
    let n = if d >= 0 {
        ((d as i64 * 16384) / p + (t * 16384) / p) / 16384
    } else {
        -((((-d) as i64 * 16384) / p + (t * 16384) / p) / 16384)
    };
    F26Dot6((n as i32) * period.0 + phase.0)
}

/// 无舍入表时的简单半像素网格舍入(round to half grid)。
#[inline]
pub fn round_half_grid(distance: F26Dot6) -> F26Dot6 {
    tt_round(distance, F26Dot6::ONE, F26Dot6::ZERO, F26Dot6::HALF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_basics() {
        assert_eq!(F26Dot6(40).round(), F26Dot6(64));
        assert_eq!(F26Dot6(31).round(), F26Dot6(0));
        assert_eq!(F26Dot6(-40).round(), F26Dot6(-64));
        assert_eq!(F26Dot6(100).floor(), F26Dot6(64));
    }

    #[test]
    fn scaling() {
        // 2048 单位,upem=2048,16px => 该单位 = 16px = 1024 (26.6)
        let scale = crate::tables::pixel_scale(2048, 16); // 每单位的 26.6 值
        let v = F26Dot6::scale_font_units(2048, scale);
        assert_eq!(v, F26Dot6::from_i32(16));
        // 半个 em -> 8px
        let half = F26Dot6::scale_font_units(1024, scale);
        assert_eq!(half, F26Dot6::from_i32(8));
    }

    #[test]
    fn f2dot14_apply() {
        let one = F2Dot14(0x4000);
        assert_eq!(one.apply(F26Dot6(128)), F26Dot6(128));
        let half = F2Dot14(0x2000);
        assert_eq!(half.apply(F26Dot6(128)), F26Dot6(64));
    }
}
