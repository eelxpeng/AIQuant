//! TEST-011 — construction out of range is rejected.
//! TEST-012 — aggregation overflow and negating the minimum are rejected.
//! TEST-013 — a zero or negative step is rejected (fail-closed).
//! TEST-014 — rounding lands where the direction says, and is idempotent.
//!
//! Proves S4 / S5 / INV-005 / INV-006, and reaches every `ValueError` variant.

use proptest::prelude::*;
use types::{Notional, Px, Qty, RoundDir, SCALE, ValueError};

const CASES: u32 = 1_000;
const DIRS: [RoundDir; 4] = [
    RoundDir::Up,
    RoundDir::Down,
    RoundDir::TowardZero,
    RoundDir::AwayFromZero,
];

/// 0.01 — the ordinary equity tick.
const TICK: i64 = 10_000_000;

// --- TEST-011: OutOfRange from construction --------------------------------

#[test]
fn whole_units_past_the_ceiling_are_rejected() {
    // 9_223_372_036 whole units is the last one whose scaled form fits.
    assert!(Px::try_from_units(9_223_372_036).is_ok());
    assert_eq!(
        Px::try_from_units(9_223_372_037),
        Err(ValueError::OutOfRange)
    );
    assert_eq!(Px::try_from_units(i64::MAX), Err(ValueError::OutOfRange));
    assert_eq!(Px::try_from_units(i64::MIN), Err(ValueError::OutOfRange));

    assert!(Qty::try_from_units(9_223_372_036).is_ok());
    assert_eq!(Qty::try_from_units(i64::MAX), Err(ValueError::OutOfRange));

    // `Notional` takes i128 units, or this arm would be unreachable (R-006).
    assert!(Notional::try_from_units(170_141_183_460_469_231_731_687_303_715).is_ok());
    assert_eq!(
        Notional::try_from_units(170_141_183_460_469_231_731_687_303_716),
        Err(ValueError::OutOfRange)
    );
    assert_eq!(
        Notional::try_from_units(i128::MAX),
        Err(ValueError::OutOfRange)
    );
}

#[test]
fn whole_units_inside_the_range_scale_exactly() {
    assert_eq!(
        Px::try_from_units(700_000).unwrap().to_scaled(),
        700_000 * SCALE
    );
    assert_eq!(Px::try_from_units(0).unwrap(), Px::ZERO);
    assert_eq!(Px::try_from_units(-5).unwrap().to_scaled(), -5 * SCALE);
    assert_eq!(Qty::try_from_units(1).unwrap().to_scaled(), SCALE);
    assert_eq!(
        Notional::try_from_units(12_345).unwrap().to_scaled(),
        12_345 * SCALE as i128
    );
}

// --- TEST-012: Overflow and OutOfRange from arithmetic ---------------------

#[test]
fn aggregation_past_the_notional_ceiling_is_rejected() {
    let near = Notional::from_scaled(i128::MAX - 2);
    let one = Notional::from_scaled(1);

    let mut acc = near;
    acc = acc.checked_add(one).expect("first add fits");
    acc = acc.checked_add(one).expect("second add fits");
    assert_eq!(acc.to_scaled(), i128::MAX);
    assert_eq!(acc.checked_add(one), Err(ValueError::Overflow));

    // The accumulator is unchanged by the failed add.
    assert_eq!(acc.to_scaled(), i128::MAX);

    assert_eq!(Notional::MIN.checked_sub(one), Err(ValueError::Overflow));
}

#[test]
fn boundary_arithmetic_on_px_and_qty_is_checked() {
    let one = Px::from_scaled(1);
    assert_eq!(Px::MAX.checked_add(one), Err(ValueError::Overflow));
    assert_eq!(Px::MIN.checked_sub(one), Err(ValueError::Overflow));
    assert_eq!(Px::MAX.checked_add(Px::ZERO), Ok(Px::MAX));

    let one = Qty::from_scaled(1);
    assert_eq!(Qty::MAX.checked_add(one), Err(ValueError::Overflow));
    assert_eq!(Qty::MIN.checked_sub(one), Err(ValueError::Overflow));
}

#[test]
fn negating_the_minimum_is_rejected_rather_than_wrapping() {
    assert_eq!(Px::MIN.checked_neg(), Err(ValueError::OutOfRange));
    assert_eq!(Qty::MIN.checked_neg(), Err(ValueError::OutOfRange));
    assert_eq!(Notional::MIN.checked_neg(), Err(ValueError::OutOfRange));

    assert_eq!(Px::MAX.checked_neg(), Ok(Px::from_scaled(-i64::MAX)));
    assert_eq!(Px::ZERO.checked_neg(), Ok(Px::ZERO));
}

// --- TEST-013: InvalidStep -------------------------------------------------

#[test]
fn a_non_positive_step_is_rejected() {
    let px = Px::from_scaled(10_004_000_000);
    let qty = Qty::from_scaled(1_500_000_000);

    for dir in DIRS {
        assert_eq!(
            px.round_to_tick(Px::ZERO, dir),
            Err(ValueError::InvalidStep)
        );
        assert_eq!(
            px.round_to_tick(Px::from_scaled(-TICK), dir),
            Err(ValueError::InvalidStep)
        );
        assert_eq!(px.round_to_tick(Px::MIN, dir), Err(ValueError::InvalidStep));

        assert_eq!(
            qty.round_to_lot(Qty::ZERO, dir),
            Err(ValueError::InvalidStep)
        );
        assert_eq!(
            qty.round_to_lot(Qty::from_scaled(-SCALE), dir),
            Err(ValueError::InvalidStep)
        );
    }
}

// --- TEST-014: the direction table ----------------------------------------

#[test]
fn tick_rounding_matches_the_spec_vectors() {
    let tick = Px::from_scaled(TICK); // 0.01
    let value = Px::from_scaled(10_004_000_000); // 10.004

    assert_eq!(
        value
            .round_to_tick(tick, RoundDir::Down)
            .unwrap()
            .to_scaled(),
        10_000_000_000
    );
    assert_eq!(
        value.round_to_tick(tick, RoundDir::Up).unwrap().to_scaled(),
        10_010_000_000
    );
    assert_eq!(
        value
            .round_to_tick(tick, RoundDir::TowardZero)
            .unwrap()
            .to_scaled(),
        10_000_000_000
    );
    assert_eq!(
        value
            .round_to_tick(tick, RoundDir::AwayFromZero)
            .unwrap()
            .to_scaled(),
        10_010_000_000
    );

    let negative = Px::from_scaled(-10_004_000_000); // -10.004
    assert_eq!(
        negative
            .round_to_tick(tick, RoundDir::Down)
            .unwrap()
            .to_scaled(),
        -10_010_000_000
    );
    assert_eq!(
        negative
            .round_to_tick(tick, RoundDir::TowardZero)
            .unwrap()
            .to_scaled(),
        -10_000_000_000
    );
    assert_eq!(
        negative
            .round_to_tick(tick, RoundDir::AwayFromZero)
            .unwrap()
            .to_scaled(),
        -10_010_000_000
    );
    assert_eq!(
        negative
            .round_to_tick(tick, RoundDir::Up)
            .unwrap()
            .to_scaled(),
        -10_000_000_000
    );
}

#[test]
fn a_value_already_on_the_step_is_unchanged_in_every_direction() {
    let tick = Px::from_scaled(TICK);
    let on_tick = Px::from_scaled(10_010_000_000); // 10.01
    for dir in DIRS {
        assert_eq!(on_tick.round_to_tick(tick, dir), Ok(on_tick));
    }

    let lot = Qty::from_scaled(SCALE);
    let whole = Qty::from_scaled(3 * SCALE);
    for dir in DIRS {
        assert_eq!(whole.round_to_lot(lot, dir), Ok(whole));
    }

    for dir in DIRS {
        assert_eq!(Px::ZERO.round_to_tick(tick, dir), Ok(Px::ZERO));
    }
}

#[test]
fn lot_rounding_matches_the_direction() {
    let lot = Qty::from_scaled(SCALE); // one whole share
    let one_and_a_half = Qty::from_scaled(1_500_000_000);

    assert_eq!(
        one_and_a_half.round_to_lot(lot, RoundDir::Down).unwrap(),
        Qty::from_scaled(SCALE)
    );
    assert_eq!(
        one_and_a_half.round_to_lot(lot, RoundDir::Up).unwrap(),
        Qty::from_scaled(2 * SCALE)
    );
}

#[test]
fn a_step_larger_than_the_value_still_rounds_rather_than_failing() {
    let tick = Px::from_scaled(SCALE); // 1.00
    let half = Px::from_scaled(SCALE / 2); // 0.50

    assert_eq!(half.round_to_tick(tick, RoundDir::Down).unwrap(), Px::ZERO);
    assert_eq!(
        half.round_to_tick(tick, RoundDir::Up).unwrap().to_scaled(),
        SCALE
    );
}

#[test]
fn rounding_up_within_one_step_of_the_ceiling_overflows() {
    let tick = Px::from_scaled(TICK);

    assert_eq!(
        Px::MAX.round_to_tick(tick, RoundDir::Up),
        Err(ValueError::Overflow)
    );
    assert_eq!(
        Px::MAX.round_to_tick(tick, RoundDir::AwayFromZero),
        Err(ValueError::Overflow)
    );

    // Downward from the same value stays in range.
    assert_eq!(
        Px::MAX
            .round_to_tick(tick, RoundDir::Down)
            .unwrap()
            .to_scaled(),
        9_223_372_036_850_000_000
    );

    assert_eq!(
        Px::MIN.round_to_tick(tick, RoundDir::Down),
        Err(ValueError::Overflow)
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    /// INV-005: rounding an exact multiple is the identity, in every direction.
    #[test]
    fn rounding_an_exact_multiple_is_the_identity(
        step in 1i64..=1_000_000_000,
        multiplier in -1_000_000_000i64..=1_000_000_000,
    ) {
        let scaled = step.checked_mul(multiplier);
        prop_assume!(scaled.is_some());
        let value = Px::from_scaled(scaled.unwrap());
        let tick = Px::from_scaled(step);
        for dir in DIRS {
            prop_assert_eq!(value.round_to_tick(tick, dir), Ok(value));
        }
    }

    /// The rounded value is always a multiple of the step, on the side named.
    #[test]
    fn rounding_lands_on_a_multiple_of_the_step(
        value in any::<i64>(),
        step in 1i64..=1_000_000_000,
    ) {
        for dir in DIRS {
            if let Ok(rounded) = Px::from_scaled(value).round_to_tick(Px::from_scaled(step), dir) {
                let r = rounded.to_scaled();
                prop_assert_eq!(r % step, 0);
                prop_assert!((r as i128 - value as i128).abs() < step as i128);
                // Widened before `abs`: `i64::MIN.abs()` panics, and `i64::MIN`
                // is a legal rounded value whenever the step divides it.
                match dir {
                    RoundDir::Down => prop_assert!(r <= value),
                    RoundDir::Up => prop_assert!(r >= value),
                    RoundDir::TowardZero => {
                        prop_assert!((r as i128).abs() <= (value as i128).abs())
                    }
                    RoundDir::AwayFromZero => {
                        prop_assert!((r as i128).abs() >= (value as i128).abs())
                    }
                }
            }
        }
    }
}
