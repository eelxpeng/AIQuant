//! Reading a decimal without going through a float.
//!
//! A feed writes `"100.25"` and a fixed-point type has to end up holding
//! exactly `100_250_000_000` scale units. Parsing through `f64` would be the
//! obvious way and is forbidden: a float cannot hold most decimal fractions
//! exactly, and Principle VII does not admit "close enough" on a price.
//!
//! So the integer part and the fraction are parsed as integers and combined.
//! Nothing here touches floating point.
//!
//! Everything it refuses, it refuses loudly. In particular a value with more
//! precision than the scale can hold is an **error**, not a silent rounding —
//! a feed sending more digits than the representation has is a fact worth
//! knowing about, and quietly dropping them is how a price becomes wrong by a
//! little bit forever.

use crate::{Px, Qty, SCALE};
use core::fmt;

/// Fraction digits the scale can hold: `1e-9`.
const SCALE_DIGITS: u32 = 9;

/// Why a decimal could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DecimalError {
    /// There was nothing to parse.
    Empty,
    /// A character that is not a digit, a sign, or the one decimal point.
    Invalid,
    /// More fraction digits than the scale can hold.
    ///
    /// Refused rather than rounded: see the module note.
    TooPrecise {
        /// How many the scale holds.
        limit: u32,
    },
    /// The value does not fit the representation.
    OutOfRange,
}

impl fmt::Display for DecimalError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecimalError::Empty => f.write_str("no digits to read"),
            DecimalError::Invalid => f.write_str("not a decimal number"),
            DecimalError::TooPrecise { limit } => {
                write!(f, "more precision than {limit} decimal places")
            }
            DecimalError::OutOfRange => f.write_str("value does not fit"),
        }
    }
}

impl core::error::Error for DecimalError {}

/// Reads a decimal into scale units.
///
/// Accepts an optional sign, an integer part, and an optional fraction of up to
/// nine digits. No exponent, no separators, no whitespace — a feed that sends
/// `1e5` is sending something this does not claim to understand, and guessing
/// would be worse than refusing.
pub const fn parse_scaled(text: &str) -> Result<i64, DecimalError> {
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return Err(DecimalError::Empty);
    }

    let mut index = 0;
    let negative = match bytes[0] {
        b'-' => {
            index = 1;
            true
        }
        b'+' => {
            index = 1;
            false
        }
        _ => false,
    };

    let mut whole: i64 = 0;
    let mut saw_digit = false;
    while index < bytes.len() && bytes[index] != b'.' {
        let digit = match bytes[index] {
            d @ b'0'..=b'9' => (d - b'0') as i64,
            _ => return Err(DecimalError::Invalid),
        };
        whole = match whole.checked_mul(10) {
            Some(scaled) => match scaled.checked_add(digit) {
                Some(total) => total,
                None => return Err(DecimalError::OutOfRange),
            },
            None => return Err(DecimalError::OutOfRange),
        };
        saw_digit = true;
        index += 1;
    }

    let mut fraction: i64 = 0;
    let mut places: u32 = 0;
    if index < bytes.len() {
        // Skip the point. A trailing point with no digits after it is allowed
        // only if the integer part had some.
        index += 1;
        while index < bytes.len() {
            let digit = match bytes[index] {
                d @ b'0'..=b'9' => (d - b'0') as i64,
                _ => return Err(DecimalError::Invalid),
            };
            if places == SCALE_DIGITS {
                return Err(DecimalError::TooPrecise {
                    limit: SCALE_DIGITS,
                });
            }
            fraction = fraction * 10 + digit;
            places += 1;
            saw_digit = true;
            index += 1;
        }
    }

    if !saw_digit {
        return Err(DecimalError::Empty);
    }

    // Left-align the fraction against the scale: "0.25" is 250_000_000 units.
    let mut padding = SCALE_DIGITS - places;
    while padding > 0 {
        fraction *= 10;
        padding -= 1;
    }

    let scaled = match whole.checked_mul(SCALE) {
        Some(units) => match units.checked_add(fraction) {
            Some(total) => total,
            None => return Err(DecimalError::OutOfRange),
        },
        None => return Err(DecimalError::OutOfRange),
    };

    if negative {
        match scaled.checked_neg() {
            Some(negated) => Ok(negated),
            None => Err(DecimalError::OutOfRange),
        }
    } else {
        Ok(scaled)
    }
}

impl Px {
    /// Reads a price from a decimal string, exactly.
    pub const fn from_decimal(text: &str) -> Result<Px, DecimalError> {
        match parse_scaled(text) {
            Ok(scaled) => Ok(Px::from_scaled(scaled)),
            Err(e) => Err(e),
        }
    }
}

impl Qty {
    /// Reads a quantity from a decimal string, exactly.
    pub const fn from_decimal(text: &str) -> Result<Qty, DecimalError> {
        match parse_scaled(text) {
            Ok(scaled) => Ok(Qty::from_scaled(scaled)),
            Err(e) => Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    // `types` is no_std; the test harness brings std in.
    use std::format;

    #[test]
    fn a_whole_number_scales_up() {
        assert_eq!(parse_scaled("0"), Ok(0));
        assert_eq!(parse_scaled("1"), Ok(SCALE));
        assert_eq!(parse_scaled("100"), Ok(100 * SCALE));
    }

    #[test]
    fn a_fraction_lands_on_the_right_scale_unit() {
        // The case a float gets wrong: 0.1 is not representable in binary.
        assert_eq!(parse_scaled("0.1"), Ok(100_000_000));
        assert_eq!(parse_scaled("100.25"), Ok(100_250_000_000));
        assert_eq!(parse_scaled("0.000000001"), Ok(1));
    }

    #[test]
    fn signs_are_read() {
        assert_eq!(parse_scaled("-1"), Ok(-SCALE));
        assert_eq!(parse_scaled("+1"), Ok(SCALE));
        assert_eq!(parse_scaled("-0.000000001"), Ok(-1));
        assert_eq!(parse_scaled("-0"), Ok(0));
    }

    #[test]
    fn a_short_fraction_is_padded_not_misread() {
        // "1.5" is fifteen hundred million units, not fifteen.
        assert_eq!(parse_scaled("1.5"), Ok(1_500_000_000));
        assert_eq!(parse_scaled("1.05"), Ok(1_050_000_000));
    }

    #[test]
    fn more_precision_than_the_scale_holds_is_refused_not_rounded() {
        // Quietly dropping the last digit is how a price becomes wrong by a
        // little bit forever.
        assert_eq!(
            parse_scaled("0.0000000001"),
            Err(DecimalError::TooPrecise { limit: 9 })
        );
        assert_eq!(
            parse_scaled("1.1234567891"),
            Err(DecimalError::TooPrecise { limit: 9 })
        );
    }

    #[test]
    fn nothing_that_is_not_a_plain_decimal_is_accepted() {
        for text in [
            "", "-", "+", ".", "abc", "1e5", "1_000", " 1", "1 ", "1.2.3", "1,5",
        ] {
            assert!(
                parse_scaled(text).is_err(),
                "{text:?} should not have parsed"
            );
        }
    }

    #[test]
    fn a_value_too_large_for_the_representation_is_refused() {
        assert_eq!(parse_scaled("100000000000"), Err(DecimalError::OutOfRange));
        assert_eq!(
            parse_scaled("99999999999999999999999"),
            Err(DecimalError::OutOfRange)
        );
    }

    #[test]
    fn the_widest_representable_value_still_parses() {
        // i64::MAX is 9_223_372_036.854775807 at this scale.
        assert_eq!(parse_scaled("9223372036.854775807"), Ok(i64::MAX));
    }

    #[test]
    fn it_reads_straight_into_the_fixed_point_types() {
        assert_eq!(
            Px::from_decimal("100.25"),
            Ok(Px::from_scaled(100_250_000_000))
        );
        assert_eq!(Qty::from_decimal("0.5"), Ok(Qty::from_scaled(500_000_000)));
        assert_eq!(Px::from_decimal("nope"), Err(DecimalError::Invalid));
    }

    #[test]
    fn parsing_round_trips_through_the_scale() {
        // Every scale unit from a decimal comes back as itself.
        for units in [0i64, 1, -1, 999_999_999, 1_000_000_000, -123_456_789_012] {
            let whole = units / SCALE;
            let frac = (units % SCALE).unsigned_abs();
            let sign = if units < 0 && whole == 0 { "-" } else { "" };
            let text = format!("{sign}{whole}.{frac:09}");
            assert_eq!(parse_scaled(&text), Ok(units), "{text}");
        }
    }
}
