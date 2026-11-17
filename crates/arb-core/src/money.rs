//! Integer money. The đồng has no minor unit in practice, so one `i64` unit = 1 VND.
//!
//! Rates are basis points (1 bps = 0.01%). Every multiplication by a rate names its
//! rounding direction; there is no default. The engine's convention is *conservative*:
//! discounts round `Down`, costs and haircuts round `Up`, so rounding error can only
//! understate profit.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ArbError;

/// ±10^15 VND. Far above any real basket, far below `i64::MAX`, so sums of thousands
/// of amounts cannot overflow before the range check fires.
pub const MAX_ABS_DONG: i64 = 1_000_000_000_000_000;

/// 100% in basis points.
pub const BPS_SCALE: i64 = 10_000;

/// Upper bound for [`Bps`]: 1000%. Anything above is a config typo, not a rate.
pub const MAX_BPS: u32 = 100_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rounding {
    /// Toward negative infinity (floor).
    Down,
    /// Toward positive infinity (ceiling).
    Up,
    /// Nearest, ties toward positive infinity.
    HalfUp,
}

/// A signed amount of Vietnamese đồng. Serializes as a bare integer.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "i64", into = "i64")]
pub struct VndAmount(i64);

impl VndAmount {
    pub const ZERO: Self = Self(0);

    pub fn try_new(dong: i64) -> Result<Self, ArbError> {
        if (-MAX_ABS_DONG..=MAX_ABS_DONG).contains(&dong) {
            Ok(Self(dong))
        } else {
            Err(ArbError::AmountOutOfRange { dong })
        }
    }

    pub const fn dong(self) -> i64 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub const fn is_negative(self) -> bool {
        self.0 < 0
    }

    pub const fn is_positive(self) -> bool {
        self.0 > 0
    }

    pub fn checked_add(self, rhs: Self) -> Result<Self, ArbError> {
        self.0
            .checked_add(rhs.0)
            .ok_or(ArbError::Arithmetic { op: "vnd_add" })
            .and_then(Self::try_new)
    }

    pub fn checked_sub(self, rhs: Self) -> Result<Self, ArbError> {
        self.0
            .checked_sub(rhs.0)
            .ok_or(ArbError::Arithmetic { op: "vnd_sub" })
            .and_then(Self::try_new)
    }

    pub fn checked_neg(self) -> Result<Self, ArbError> {
        self.0
            .checked_neg()
            .ok_or(ArbError::Arithmetic { op: "vnd_neg" })
            .and_then(Self::try_new)
    }

    pub fn checked_mul_qty(self, qty: u32) -> Result<Self, ArbError> {
        self.0
            .checked_mul(i64::from(qty))
            .ok_or(ArbError::Arithmetic { op: "vnd_mul_qty" })
            .and_then(Self::try_new)
    }

    /// `max(self - rhs, 0)`. For "price after a discount can't go below zero".
    pub fn sub_floor_zero(self, rhs: Self) -> Result<Self, ArbError> {
        self.checked_sub(rhs).map(|v| v.max(Self::ZERO))
    }

    /// `self * rate`, rounded as requested. Exact: widens to `i128` before dividing.
    pub fn mul_bps(self, rate: Bps, rounding: Rounding) -> Result<Self, ArbError> {
        let scale = i128::from(BPS_SCALE);
        let product = i128::from(self.0) * i128::from(rate.get());
        let quotient = match rounding {
            Rounding::Down => product.div_euclid(scale),
            Rounding::Up => -((-product).div_euclid(scale)),
            Rounding::HalfUp => (product + scale / 2).div_euclid(scale),
        };
        i64::try_from(quotient)
            .map_err(|_| ArbError::Arithmetic { op: "vnd_mul_bps" })
            .and_then(Self::try_new)
    }

    /// `floor(self / denom)` expressed in basis points. `denom` must be positive.
    /// Floors toward negative infinity, so a reported ROI never overstates.
    pub fn ratio_bps(self, denom: Self) -> Result<SignedBps, ArbError> {
        if denom.0 <= 0 {
            return Err(ArbError::Arithmetic {
                op: "vnd_ratio_bps_nonpositive_denominator",
            });
        }
        let q = (i128::from(self.0) * i128::from(BPS_SCALE)).div_euclid(i128::from(denom.0));
        i64::try_from(q)
            .map(SignedBps::new)
            .map_err(|_| ArbError::Arithmetic {
                op: "vnd_ratio_bps",
            })
    }

    /// Floor to a multiple of `width` (> 0). Used for dedup price buckets.
    pub fn bucket_floor(self, width: Self) -> Result<Self, ArbError> {
        if width.0 <= 0 {
            return Err(ArbError::Arithmetic {
                op: "vnd_bucket_nonpositive_width",
            });
        }
        Self::try_new(self.0.div_euclid(width.0) * width.0)
    }

    /// Checked sum; fails on the first out-of-range partial sum.
    pub fn sum_checked<I: IntoIterator<Item = Self>>(iter: I) -> Result<Self, ArbError> {
        iter.into_iter().try_fold(Self::ZERO, Self::checked_add)
    }
}

impl TryFrom<i64> for VndAmount {
    type Error = ArbError;
    fn try_from(dong: i64) -> Result<Self, ArbError> {
        Self::try_new(dong)
    }
}

impl From<VndAmount> for i64 {
    fn from(v: VndAmount) -> Self {
        v.0
    }
}

/// Vietnamese formatting: dot thousands separator, trailing ₫. `-1.500.000 ₫`.
impl fmt::Display for VndAmount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let digits = self.0.unsigned_abs().to_string();
        let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 4);
        if self.0 < 0 {
            out.push('-');
        }
        let lead = digits.len() % 3;
        for (i, ch) in digits.chars().enumerate() {
            if i != 0 && (i + 3 - lead).is_multiple_of(3) {
                out.push('.');
            }
            out.push(ch);
        }
        out.push_str(" ₫");
        f.write_str(&out)
    }
}

/// A non-negative rate in basis points, `0..=MAX_BPS`. Serializes as a bare integer.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(try_from = "u32", into = "u32")]
pub struct Bps(u32);

impl Bps {
    pub const ZERO: Self = Self(0);
    pub const ONE_HUNDRED_PERCENT: Self = Self(10_000);

    pub fn try_new(bps: u32) -> Result<Self, ArbError> {
        if bps <= MAX_BPS {
            Ok(Self(bps))
        } else {
            Err(ArbError::RateOutOfRange {
                bps: i64::from(bps),
            })
        }
    }

    pub const fn get(self) -> u32 {
        self.0
    }

    pub fn checked_add(self, rhs: Self) -> Result<Self, ArbError> {
        self.0
            .checked_add(rhs.0)
            .ok_or(ArbError::Arithmetic { op: "bps_add" })
            .and_then(Self::try_new)
    }

    pub const fn as_signed(self) -> SignedBps {
        SignedBps(self.0 as i64)
    }
}

impl TryFrom<u32> for Bps {
    type Error = ArbError;
    fn try_from(bps: u32) -> Result<Self, ArbError> {
        Self::try_new(bps)
    }
}

impl From<Bps> for u32 {
    fn from(b: Bps) -> Self {
        b.0
    }
}

impl fmt::Display for Bps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{:02}%", self.0 / 100, self.0 % 100)
    }
}

/// A signed ratio in basis points (ROI, price change). Unbounded by design: a glitch
/// listing can legitimately compute to a five-digit ROI before the sanity gate rejects it.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct SignedBps(i64);

impl SignedBps {
    pub const ZERO: Self = Self(0);

    pub const fn new(bps: i64) -> Self {
        Self(bps)
    }

    pub const fn get(self) -> i64 {
        self.0
    }
}

impl fmt::Display for SignedBps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let abs = self.0.unsigned_abs();
        let sign = if self.0 < 0 { "-" } else { "" };
        write!(f, "{sign}{}.{:02}%", abs / 100, abs % 100)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vnd(d: i64) -> VndAmount {
        VndAmount::try_new(d).unwrap()
    }

    fn bps(b: u32) -> Bps {
        Bps::try_new(b).unwrap()
    }

    #[test]
    fn range_is_enforced() {
        assert!(VndAmount::try_new(MAX_ABS_DONG).is_ok());
        assert!(VndAmount::try_new(MAX_ABS_DONG + 1).is_err());
        assert!(vnd(MAX_ABS_DONG).checked_add(vnd(1)).is_err());
        assert!(Bps::try_new(MAX_BPS + 1).is_err());
    }

    #[test]
    fn mul_bps_rounding_directions() {
        // 34_990_001 * 8% = 2_799_200.08
        let p = vnd(34_990_001);
        assert_eq!(p.mul_bps(bps(800), Rounding::Down).unwrap(), vnd(2_799_200));
        assert_eq!(p.mul_bps(bps(800), Rounding::Up).unwrap(), vnd(2_799_201));
        assert_eq!(
            p.mul_bps(bps(800), Rounding::HalfUp).unwrap(),
            vnd(2_799_200)
        );
        // exact products are unaffected by the mode
        let q = vnd(30_000_000);
        for r in [Rounding::Down, Rounding::Up, Rounding::HalfUp] {
            assert_eq!(q.mul_bps(bps(250), r).unwrap(), vnd(750_000));
        }
        // negative: Down is floor (more negative), Up is ceiling
        assert_eq!(
            vnd(-15).mul_bps(bps(1_000), Rounding::Down).unwrap(),
            vnd(-2)
        );
        assert_eq!(vnd(-15).mul_bps(bps(1_000), Rounding::Up).unwrap(), vnd(-1));
        // half-way case
        assert_eq!(
            vnd(5).mul_bps(bps(1_000), Rounding::HalfUp).unwrap(),
            vnd(1)
        );
    }

    #[test]
    fn ratio_floors_and_rejects_bad_denominator() {
        assert_eq!(
            vnd(1_500_000).ratio_bps(vnd(30_000_000)).unwrap(),
            SignedBps::new(500)
        );
        assert_eq!(vnd(1).ratio_bps(vnd(3)).unwrap(), SignedBps::new(3_333));
        assert_eq!(vnd(-1).ratio_bps(vnd(3)).unwrap(), SignedBps::new(-3_334));
        assert!(vnd(1).ratio_bps(VndAmount::ZERO).is_err());
        assert!(vnd(1).ratio_bps(vnd(-5)).is_err());
    }

    #[test]
    fn bucket_and_floor_zero() {
        assert_eq!(
            vnd(29_949_000).bucket_floor(vnd(100_000)).unwrap(),
            vnd(29_900_000)
        );
        assert_eq!(vnd(100).sub_floor_zero(vnd(250)).unwrap(), VndAmount::ZERO);
        assert!(vnd(1).bucket_floor(VndAmount::ZERO).is_err());
    }

    #[test]
    fn display_formats() {
        assert_eq!(vnd(32_990_000).to_string(), "32.990.000 ₫");
        assert_eq!(vnd(-1_500_000).to_string(), "-1.500.000 ₫");
        assert_eq!(vnd(999).to_string(), "999 ₫");
        assert_eq!(vnd(1_000).to_string(), "1.000 ₫");
        assert_eq!(VndAmount::ZERO.to_string(), "0 ₫");
        assert_eq!(bps(725).to_string(), "7.25%");
        assert_eq!(SignedBps::new(-5).to_string(), "-0.05%");
    }
}
