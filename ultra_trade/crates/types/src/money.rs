use core::ops::{Add, Neg, Sub};

use crate::error::ValueError;
use crate::rounding::{RoundDir, round_to_step};
use crate::{SCALE, SCALE_I128};

/// A fixed-point price in the instrument's quoting convention.
///
/// `i64` at a `1e-9` scale, spanning ±9_223_372_036.854775807 (ADR #4). Never a
/// float, never compared with a tolerance. `Px + Qty` does not compile, and
/// neither does `Px * Qty` — the product is [`Px::notional`], which names its
/// rounding direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Px(i64);

/// A fixed-point quantity in the instrument's lot convention.
///
/// `i64` at a `1e-9` scale, which admits fractional shares (ADR #4). Signed,
/// per that record's table; the preference for an unsigned quantity plus an
/// explicit side is a usage convention enforced where a side type exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Qty(i64);

/// The money value of a price times a quantity.
///
/// `i128` at a `1e-9` scale (ADR #4): wider than [`Px`] and [`Qty`] because
/// cumulative traded value overflows 64 bits on an ordinary day, and because
/// the multiply already forces an `i128` intermediate. The only type produced
/// by multiplying the other two.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Notional(i128);

impl Px {
    /// The additive identity, and the result of aggregating nothing.
    pub const ZERO: Px = Px(0);
    /// The most negative representable price.
    pub const MIN: Px = Px(i64::MIN);
    /// The largest representable price.
    pub const MAX: Px = Px(i64::MAX);

    /// Builds a price from its count of scale units.
    ///
    /// Total: every `i64` is a valid count, so there is nothing to check. This
    /// and [`Px::to_scaled`] are the only window onto the representation
    /// (ADR #6); all arithmetic goes through the type.
    #[inline]
    #[must_use]
    pub const fn from_scaled(scaled: i64) -> Px {
        Px(scaled)
    }

    /// The price's count of scale units. Exact inverse of [`Px::from_scaled`].
    #[inline]
    #[must_use]
    pub const fn to_scaled(self) -> i64 {
        self.0
    }

    /// Builds a price from whole units.
    ///
    /// # Errors
    ///
    /// [`ValueError::OutOfRange`] when `units * SCALE` leaves the range — past
    /// roughly ±9.22e9.
    pub const fn try_from_units(units: i64) -> Result<Px, ValueError> {
        match units.checked_mul(SCALE) {
            Some(scaled) => Ok(Px(scaled)),
            None => Err(ValueError::OutOfRange),
        }
    }

    /// Adds two prices at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the sum leaves the range.
    pub const fn checked_add(self, rhs: Px) -> Result<Px, ValueError> {
        match self.0.checked_add(rhs.0) {
            Some(scaled) => Ok(Px(scaled)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Subtracts two prices at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the difference leaves the range.
    pub const fn checked_sub(self, rhs: Px) -> Result<Px, ValueError> {
        match self.0.checked_sub(rhs.0) {
            Some(scaled) => Ok(Px(scaled)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Negates a price at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::OutOfRange`] on [`Px::MIN`], which has no positive
    /// counterpart.
    pub const fn checked_neg(self) -> Result<Px, ValueError> {
        match self.0.checked_neg() {
            Some(scaled) => Ok(Px(scaled)),
            None => Err(ValueError::OutOfRange),
        }
    }

    /// Rounds this price to a multiple of `tick`, in the named direction.
    ///
    /// Every price crossing a venue boundary goes through here (Constitution
    /// VII). A price already on an exact tick is returned unchanged, whatever
    /// the direction.
    ///
    /// # Errors
    ///
    /// [`ValueError::InvalidStep`] when `tick` is zero or negative;
    /// [`ValueError::Overflow`] when rounding would leave the range.
    pub fn round_to_tick(self, tick: Px, dir: RoundDir) -> Result<Px, ValueError> {
        if tick.0 <= 0 {
            return Err(ValueError::InvalidStep);
        }
        // Widened first: `lower + step` can leave `i64` near either bound, and
        // the narrowing below is where that becomes a reported error.
        let rounded = round_to_step(self.0 as i128, tick.0 as i128, dir);
        match i64::try_from(rounded) {
            Ok(scaled) => Ok(Px(scaled)),
            Err(_) => Err(ValueError::Overflow),
        }
    }

    /// The money value of this price times `qty`.
    ///
    /// The one operation relating two value types. It takes a direction because
    /// an exact product can need up to eighteen decimal places and [`Notional`]
    /// holds nine — `10.0001 × 0.000001` is a sub-dollar tick times a
    /// fractional share and already needs ten (ADR #6).
    ///
    /// No `Result`, because this cannot overflow (INV-004): both operands are
    /// `i64` scale-unit counts, so the product is at most `2^126 ≈ 8.507e37`
    /// against `i128::MAX ≈ 1.701e38` — a factor of two of headroom — and
    /// rounding adds at most one further `SCALE` before the divide shrinks it.
    #[must_use]
    pub fn notional(self, qty: Qty, dir: RoundDir) -> Notional {
        // In units of 1e-18: two 1e-9 counts multiplied.
        let product = (self.0 as i128) * (qty.0 as i128);
        // Rounded to a multiple of SCALE, so the divide is exact.
        Notional(round_to_step(product, SCALE_I128, dir) / SCALE_I128)
    }
}

impl Qty {
    /// The additive identity, and the result of aggregating nothing.
    pub const ZERO: Qty = Qty(0);
    /// The most negative representable quantity.
    pub const MIN: Qty = Qty(i64::MIN);
    /// The largest representable quantity.
    pub const MAX: Qty = Qty(i64::MAX);

    /// Builds a quantity from its count of scale units. Total.
    #[inline]
    #[must_use]
    pub const fn from_scaled(scaled: i64) -> Qty {
        Qty(scaled)
    }

    /// The quantity's count of scale units. Exact inverse of
    /// [`Qty::from_scaled`].
    #[inline]
    #[must_use]
    pub const fn to_scaled(self) -> i64 {
        self.0
    }

    /// Builds a quantity from whole units.
    ///
    /// # Errors
    ///
    /// [`ValueError::OutOfRange`] when `units * SCALE` leaves the range.
    pub const fn try_from_units(units: i64) -> Result<Qty, ValueError> {
        match units.checked_mul(SCALE) {
            Some(scaled) => Ok(Qty(scaled)),
            None => Err(ValueError::OutOfRange),
        }
    }

    /// Adds two quantities at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the sum leaves the range.
    pub const fn checked_add(self, rhs: Qty) -> Result<Qty, ValueError> {
        match self.0.checked_add(rhs.0) {
            Some(scaled) => Ok(Qty(scaled)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Subtracts two quantities at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the difference leaves the range.
    pub const fn checked_sub(self, rhs: Qty) -> Result<Qty, ValueError> {
        match self.0.checked_sub(rhs.0) {
            Some(scaled) => Ok(Qty(scaled)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Negates a quantity at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::OutOfRange`] on [`Qty::MIN`].
    pub const fn checked_neg(self) -> Result<Qty, ValueError> {
        match self.0.checked_neg() {
            Some(scaled) => Ok(Qty(scaled)),
            None => Err(ValueError::OutOfRange),
        }
    }

    /// Rounds this quantity to a multiple of `lot`, in the named direction.
    ///
    /// # Errors
    ///
    /// [`ValueError::InvalidStep`] when `lot` is zero or negative;
    /// [`ValueError::Overflow`] when rounding would leave the range.
    pub fn round_to_lot(self, lot: Qty, dir: RoundDir) -> Result<Qty, ValueError> {
        if lot.0 <= 0 {
            return Err(ValueError::InvalidStep);
        }
        let rounded = round_to_step(self.0 as i128, lot.0 as i128, dir);
        match i64::try_from(rounded) {
            Ok(scaled) => Ok(Qty(scaled)),
            Err(_) => Err(ValueError::Overflow),
        }
    }
}

impl Notional {
    /// The additive identity, and the result of aggregating nothing.
    pub const ZERO: Notional = Notional(0);
    /// The most negative representable money value.
    pub const MIN: Notional = Notional(i128::MIN);
    /// The largest representable money value.
    pub const MAX: Notional = Notional(i128::MAX);

    /// Builds a money value from its count of scale units. Total.
    #[inline]
    #[must_use]
    pub const fn from_scaled(scaled: i128) -> Notional {
        Notional(scaled)
    }

    /// The value's count of scale units. Exact inverse of
    /// [`Notional::from_scaled`].
    #[inline]
    #[must_use]
    pub const fn to_scaled(self) -> i128 {
        self.0
    }

    /// Builds a money value from whole units.
    ///
    /// Takes `i128` rather than `i64`: an `i64` count of whole units always
    /// fits, which would leave this error arm unreachable (research.md R-006).
    ///
    /// # Errors
    ///
    /// [`ValueError::OutOfRange`] when `units * SCALE` leaves the range.
    pub const fn try_from_units(units: i128) -> Result<Notional, ValueError> {
        match units.checked_mul(SCALE_I128) {
            Some(scaled) => Ok(Notional(scaled)),
            None => Err(ValueError::OutOfRange),
        }
    }

    /// Adds two money values at a boundary — the aggregation case.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the sum leaves the range.
    pub const fn checked_add(self, rhs: Notional) -> Result<Notional, ValueError> {
        match self.0.checked_add(rhs.0) {
            Some(scaled) => Ok(Notional(scaled)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Subtracts two money values at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the difference leaves the range.
    pub const fn checked_sub(self, rhs: Notional) -> Result<Notional, ValueError> {
        match self.0.checked_sub(rhs.0) {
            Some(scaled) => Ok(Notional(scaled)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Negates a money value at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::OutOfRange`] on [`Notional::MIN`].
    pub const fn checked_neg(self) -> Result<Notional, ValueError> {
        match self.0.checked_neg() {
            Some(scaled) => Ok(Notional(scaled)),
            None => Err(ValueError::OutOfRange),
        }
    }
}

// Interior arithmetic: debug-assert overflow, release wrap, exactly as ADR #4
// specifies. That is Rust's built-in integer behaviour, so these are the plain
// operators and deliberately not the checked forms. Use `checked_*` at a
// boundary — construction, venue I/O, aggregation — and these inside one, which
// is what keeps `?` off every hot-path expression.

impl Add for Px {
    type Output = Px;
    #[inline]
    fn add(self, rhs: Px) -> Px {
        Px(self.0 + rhs.0)
    }
}

impl Sub for Px {
    type Output = Px;
    #[inline]
    fn sub(self, rhs: Px) -> Px {
        Px(self.0 - rhs.0)
    }
}

impl Neg for Px {
    type Output = Px;
    #[inline]
    fn neg(self) -> Px {
        Px(-self.0)
    }
}

impl Add for Qty {
    type Output = Qty;
    #[inline]
    fn add(self, rhs: Qty) -> Qty {
        Qty(self.0 + rhs.0)
    }
}

impl Sub for Qty {
    type Output = Qty;
    #[inline]
    fn sub(self, rhs: Qty) -> Qty {
        Qty(self.0 - rhs.0)
    }
}

impl Neg for Qty {
    type Output = Qty;
    #[inline]
    fn neg(self) -> Qty {
        Qty(-self.0)
    }
}

impl Add for Notional {
    type Output = Notional;
    #[inline]
    fn add(self, rhs: Notional) -> Notional {
        Notional(self.0 + rhs.0)
    }
}

impl Sub for Notional {
    type Output = Notional;
    #[inline]
    fn sub(self, rhs: Notional) -> Notional {
        Notional(self.0 - rhs.0)
    }
}

impl Neg for Notional {
    type Output = Notional;
    #[inline]
    fn neg(self) -> Notional {
        Notional(-self.0)
    }
}
