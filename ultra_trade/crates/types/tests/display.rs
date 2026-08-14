//! How the value types print.
//!
//! The scale is nine places and the output says so, always. These numbers get
//! compared between a live session and a backtest of it, and a trimming rule
//! that made −4.6 and −4.600000000 look different would manufacture a
//! disagreement where there is none.

use types::{Notional, Px, Qty, SCALE};

#[test]
fn a_whole_number_still_shows_every_place() {
    assert_eq!(
        Px::from_scaled(63_310 * SCALE).to_string(),
        "63310.000000000"
    );
}

#[test]
fn the_fraction_is_zero_padded_not_trimmed() {
    // 0.1 is 100_000_000 scale units, and its leading zeros are load-bearing.
    assert_eq!(Px::from_scaled(100_000_000).to_string(), "0.100000000");
    assert_eq!(Qty::from_scaled(10).to_string(), "0.000000010");
    assert_eq!(Qty::from_scaled(1).to_string(), "0.000000001");
}

#[test]
fn a_negative_value_signs_the_whole_number_not_the_integer_part() {
    // -0.5 must not print as "-0.500000000" via a "-0" integer part that
    // happens to work; check the sub-unit case where "0" carries no sign.
    assert_eq!(Qty::from_scaled(-500_000_000).to_string(), "-0.500000000");
    assert_eq!(
        Notional::from_scaled(-4_600_000_000).to_string(),
        "-4.600000000"
    );
}

#[test]
fn zero_has_no_sign() {
    assert_eq!(Px::ZERO.to_string(), "0.000000000");
    assert_eq!(Notional::ZERO.to_string(), "0.000000000");
}

#[test]
fn the_extremes_render_without_panicking() {
    // The buffer is sized for the widest i128; prove it rather than trust it.
    assert_eq!(Px::MIN.to_string(), "-9223372036.854775808");
    assert_eq!(Px::MAX.to_string(), "9223372036.854775807");
    let wide = Notional::from_scaled(i128::MIN);
    assert_eq!(wide.to_string().len(), 1 + 30 + 1 + 9);
}

#[test]
fn a_width_in_the_format_string_is_honoured() {
    // Reports print these in columns. `pad` is what makes that work.
    assert_eq!(
        format!("{:>16}", Qty::from_scaled(SCALE)),
        "     1.000000000"
    );
    assert_eq!(
        format!("{:<16}|", Qty::from_scaled(SCALE)),
        "1.000000000     |"
    );
}
