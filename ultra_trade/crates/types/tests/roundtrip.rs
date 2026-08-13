//! TEST-001 — round-trip is exact over each type's full backing range.
//! TEST-002 — same-type arithmetic matches integer arithmetic on scale units.
//!
//! Proves S1 / INV-001 / INV-010.

use proptest::prelude::*;
use types::{Notional, Px, Qty};

/// SC-003 requires at least 1,000 generated cases per property.
const CASES: u32 = 1_000;

// --- TEST-001 -------------------------------------------------------------

/// The boundaries are asserted directly, not left to sampling. A generator that
/// *usually* reaches `i64::MIN` is not evidence (SC-003).
#[test]
fn boundary_values_round_trip_exactly() {
    for scaled in [i64::MIN, i64::MIN + 1, -1, 0, 1, i64::MAX - 1, i64::MAX] {
        assert_eq!(Px::from_scaled(scaled).to_scaled(), scaled);
        assert_eq!(Qty::from_scaled(scaled).to_scaled(), scaled);
    }
    for scaled in [i128::MIN, i128::MIN + 1, -1, 0, 1, i128::MAX - 1, i128::MAX] {
        assert_eq!(Notional::from_scaled(scaled).to_scaled(), scaled);
    }
}

#[test]
fn one_scale_unit_is_not_zero() {
    assert_ne!(Px::from_scaled(1), Px::ZERO);
    assert_ne!(Qty::from_scaled(1), Qty::ZERO);
    assert_ne!(Notional::from_scaled(1), Notional::ZERO);
    assert_eq!(Px::from_scaled(1).to_scaled(), 1);
}

#[test]
fn declared_constants_match_the_backing_bounds() {
    assert_eq!(Px::MIN.to_scaled(), i64::MIN);
    assert_eq!(Px::MAX.to_scaled(), i64::MAX);
    assert_eq!(Px::ZERO.to_scaled(), 0);
    assert_eq!(Qty::MIN.to_scaled(), i64::MIN);
    assert_eq!(Qty::MAX.to_scaled(), i64::MAX);
    assert_eq!(Qty::ZERO.to_scaled(), 0);
    assert_eq!(Notional::MIN.to_scaled(), i128::MIN);
    assert_eq!(Notional::MAX.to_scaled(), i128::MAX);
    assert_eq!(Notional::ZERO.to_scaled(), 0);
}

// --- TEST-002 -------------------------------------------------------------

#[test]
fn zero_is_the_additive_identity() {
    for scaled in [i64::MIN, -7, 0, 1, i64::MAX] {
        assert_eq!(Px::from_scaled(scaled) + Px::ZERO, Px::from_scaled(scaled));
        assert_eq!(
            Qty::from_scaled(scaled) + Qty::ZERO,
            Qty::from_scaled(scaled)
        );
    }
    assert_eq!(Notional::MAX + Notional::ZERO, Notional::MAX);
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn px_round_trips(scaled in any::<i64>()) {
        prop_assert_eq!(Px::from_scaled(scaled).to_scaled(), scaled);
    }

    #[test]
    fn qty_round_trips(scaled in any::<i64>()) {
        prop_assert_eq!(Qty::from_scaled(scaled).to_scaled(), scaled);
    }

    #[test]
    fn notional_round_trips(scaled in any::<i128>()) {
        prop_assert_eq!(Notional::from_scaled(scaled).to_scaled(), scaled);
    }

    #[test]
    fn px_addition_matches_integer_addition(a in any::<i64>(), b in any::<i64>()) {
        prop_assume!(a.checked_add(b).is_some());
        prop_assert_eq!((Px::from_scaled(a) + Px::from_scaled(b)).to_scaled(), a + b);
    }

    #[test]
    fn px_subtraction_matches_integer_subtraction(a in any::<i64>(), b in any::<i64>()) {
        prop_assume!(a.checked_sub(b).is_some());
        prop_assert_eq!((Px::from_scaled(a) - Px::from_scaled(b)).to_scaled(), a - b);
    }

    #[test]
    fn px_negation_matches_integer_negation(a in any::<i64>()) {
        prop_assume!(a.checked_neg().is_some());
        prop_assert_eq!((-Px::from_scaled(a)).to_scaled(), -a);
    }

    #[test]
    fn qty_addition_matches_integer_addition(a in any::<i64>(), b in any::<i64>()) {
        prop_assume!(a.checked_add(b).is_some());
        prop_assert_eq!((Qty::from_scaled(a) + Qty::from_scaled(b)).to_scaled(), a + b);
    }

    #[test]
    fn qty_subtraction_matches_integer_subtraction(a in any::<i64>(), b in any::<i64>()) {
        prop_assume!(a.checked_sub(b).is_some());
        prop_assert_eq!((Qty::from_scaled(a) - Qty::from_scaled(b)).to_scaled(), a - b);
    }

    #[test]
    fn notional_addition_matches_integer_addition(a in any::<i128>(), b in any::<i128>()) {
        prop_assume!(a.checked_add(b).is_some());
        prop_assert_eq!((Notional::from_scaled(a) + Notional::from_scaled(b)).to_scaled(), a + b);
    }

    #[test]
    fn notional_subtraction_matches_integer_subtraction(a in any::<i128>(), b in any::<i128>()) {
        prop_assume!(a.checked_sub(b).is_some());
        prop_assert_eq!((Notional::from_scaled(a) - Notional::from_scaled(b)).to_scaled(), a - b);
    }

    /// INV-010: comparison is integer comparison of scale units. No tolerance
    /// is involved, and no API accepts one.
    #[test]
    fn px_ordering_is_exact(a in any::<i64>(), b in any::<i64>()) {
        prop_assert_eq!(Px::from_scaled(a).cmp(&Px::from_scaled(b)), a.cmp(&b));
        prop_assert_eq!(Px::from_scaled(a) == Px::from_scaled(b), a == b);
    }

    #[test]
    fn notional_ordering_is_exact(a in any::<i128>(), b in any::<i128>()) {
        prop_assert_eq!(Notional::from_scaled(a).cmp(&Notional::from_scaled(b)), a.cmp(&b));
    }
}
