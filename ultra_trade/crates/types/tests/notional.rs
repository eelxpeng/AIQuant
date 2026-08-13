//! TEST-003 — the notional product is exact when the product is representable,
//!             and then identical for every direction.
//! TEST-004 — otherwise it lands on the neighbour the call site named.
//! TEST-005 — it never overflows, for any pair of representable operands.
//!
//! Proves S2 / INV-004 / INV-005.

use proptest::prelude::*;
use types::{Notional, Px, Qty, RoundDir, SCALE};

const CASES: u32 = 1_000;
const SCALE_I128: i128 = SCALE as i128;
const DIRS: [RoundDir; 4] = [
    RoundDir::Up,
    RoundDir::Down,
    RoundDir::TowardZero,
    RoundDir::AwayFromZero,
];

// --- TEST-003 -------------------------------------------------------------

/// A whole-share quantity makes the product an exact multiple of one scale
/// unit, so every direction must agree and none may move the value.
#[test]
fn exact_products_are_direction_independent() {
    let px = Px::from_scaled(123_450_000_000); // 123.45
    let qty = Qty::from_scaled(100 * SCALE); // 100 shares
    for dir in DIRS {
        assert_eq!(px.notional(qty, dir).to_scaled(), 12_345_000_000_000);
    }

    let px = Px::from_scaled(700_000_000_000_000); // 700_000.00
    let qty = Qty::from_scaled(10 * SCALE); // 10 shares
    for dir in DIRS {
        assert_eq!(px.notional(qty, dir).to_scaled(), 7_000_000_000_000_000);
    }
}

#[test]
fn a_zero_operand_gives_zero_in_every_direction() {
    for dir in DIRS {
        assert_eq!(Px::ZERO.notional(Qty::MAX, dir), Notional::ZERO);
        assert_eq!(Px::MAX.notional(Qty::ZERO, dir), Notional::ZERO);
    }
}

// --- TEST-004 -------------------------------------------------------------

/// The spec's notional product vectors. `10.0001 × 0.000001 = 0.0000100001`
/// needs ten decimal places, and the scale holds nine.
#[test]
fn a_product_finer_than_the_scale_lands_where_the_call_site_says() {
    let px = Px::from_scaled(10_000_100_000); // 10.0001
    let qty = Qty::from_scaled(1_000); // 0.000001

    assert_eq!(px.notional(qty, RoundDir::TowardZero).to_scaled(), 10_000);
    assert_eq!(px.notional(qty, RoundDir::AwayFromZero).to_scaled(), 10_001);
    assert_eq!(px.notional(qty, RoundDir::Down).to_scaled(), 10_000);
    assert_eq!(px.notional(qty, RoundDir::Up).to_scaled(), 10_001);
}

/// On a negative product, `Down` and `TowardZero` disagree. This is the case
/// that a hand-rolled truncation gets wrong.
#[test]
fn a_negative_product_rounds_toward_the_named_infinity() {
    let px = Px::from_scaled(-10_000_100_000); // -10.0001
    let qty = Qty::from_scaled(1_000); // 0.000001

    assert_eq!(px.notional(qty, RoundDir::Down).to_scaled(), -10_001);
    assert_eq!(px.notional(qty, RoundDir::TowardZero).to_scaled(), -10_000);
    assert_eq!(
        px.notional(qty, RoundDir::AwayFromZero).to_scaled(),
        -10_001
    );
    assert_eq!(px.notional(qty, RoundDir::Up).to_scaled(), -10_000);
}

// --- TEST-005 -------------------------------------------------------------

/// INV-004: the largest product two `i64` scale-unit counts can make is
/// `2^126 ≈ 8.507e37`, against `i128::MAX ≈ 1.701e38`.
#[test]
fn the_extreme_product_does_not_overflow() {
    let bound = (i64::MIN as i128) * (i64::MIN as i128) / SCALE_I128;
    for dir in DIRS {
        let n = Px::MIN.notional(Qty::MIN, dir);
        assert!(n.to_scaled() > 0);
        assert!(n.to_scaled().abs() <= bound + 1);

        let n = Px::MIN.notional(Qty::MAX, dir);
        assert!(n.to_scaled() < 0);
        assert!(n.to_scaled().abs() <= bound + 1);

        let n = Px::MAX.notional(Qty::MAX, dir);
        assert!(n.to_scaled() > 0);
        assert!(n.to_scaled().abs() <= bound + 1);
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// Total: no input pair panics, and every result is inside `Notional`.
    #[test]
    fn the_product_never_overflows(a in any::<i64>(), b in any::<i64>()) {
        let bound = (i64::MIN as i128) * (i64::MIN as i128) / SCALE_I128 + 1;
        for dir in DIRS {
            let n = Px::from_scaled(a).notional(Qty::from_scaled(b), dir);
            prop_assert!(n.to_scaled().abs() <= bound);
        }
    }

    /// The result is always within one scale unit of the exact product, on the
    /// side the direction names. This is TEST-003 and TEST-004 as one property.
    #[test]
    fn the_product_is_the_named_neighbour_of_the_exact_value(a in any::<i64>(), b in any::<i64>()) {
        // The exact product, in units of 1e-18.
        let exact = (a as i128) * (b as i128);

        for dir in DIRS {
            let got = Px::from_scaled(a).notional(Qty::from_scaled(b), dir).to_scaled();
            let got_scaled = got * SCALE_I128;
            let error = exact - got_scaled;

            prop_assert!(error.abs() < SCALE_I128, "result moved by a whole scale unit or more");

            match dir {
                RoundDir::Down => prop_assert!(error >= 0),
                RoundDir::Up => prop_assert!(error <= 0),
                RoundDir::TowardZero => {
                    prop_assert!(got.abs() * SCALE_I128 <= exact.abs());
                }
                RoundDir::AwayFromZero => {
                    prop_assert!(got.abs() * SCALE_I128 >= exact.abs());
                }
            }

            // Exactly representable products ignore the direction entirely.
            if exact % SCALE_I128 == 0 {
                prop_assert_eq!(got_scaled, exact);
            }
        }
    }
}
