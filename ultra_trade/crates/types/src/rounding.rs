/// The direction a value is moved when it is rounded.
///
/// There is no default and no `Default` impl, by design: a call site that does
/// not state a direction does not compile (Constitution VII). `Up` and `Down`
/// are toward the infinities, so on a negative value `Down` and `TowardZero`
/// disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundDir {
    /// Toward +∞.
    Up,
    /// Toward −∞.
    Down,
    /// Toward zero — truncation.
    TowardZero,
    /// Away from zero.
    AwayFromZero,
}

/// Rounds `value` to a multiple of `step` in the named direction.
///
/// Shared by tick rounding, lot rounding, and the notional product, so the
/// negative-value behaviour is defined in exactly one place.
///
/// Callers must pass `step > 0` and must guarantee that `value + step` does not
/// overflow `i128`. Both hold for every caller in this crate: money rounding
/// widens an `i64` before calling, and the notional product's largest input is
/// `2^126`, which leaves a factor of two of headroom.
pub(crate) fn round_to_step(value: i128, step: i128, dir: RoundDir) -> i128 {
    debug_assert!(step > 0, "round_to_step requires a positive step");

    // `div_euclid` floors for a positive step, so `lower` is the multiple at or
    // below `value` with no sign special-casing. That is where hand-rolled
    // rounding usually goes wrong on negatives.
    let lower = value.div_euclid(step) * step;
    if lower == value {
        return value;
    }
    let upper = lower + step;

    // `value` cannot be zero here: zero is an exact multiple of every step and
    // returned above. So its sign is well defined.
    match dir {
        RoundDir::Down => lower,
        RoundDir::Up => upper,
        RoundDir::TowardZero => {
            if value > 0 {
                lower
            } else {
                upper
            }
        }
        RoundDir::AwayFromZero => {
            if value > 0 {
                upper
            } else {
                lower
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{RoundDir, round_to_step};

    const DIRS: [RoundDir; 4] = [
        RoundDir::Up,
        RoundDir::Down,
        RoundDir::TowardZero,
        RoundDir::AwayFromZero,
    ];

    #[test]
    fn an_exact_multiple_is_returned_unchanged() {
        for dir in DIRS {
            assert_eq!(round_to_step(100, 10, dir), 100);
            assert_eq!(round_to_step(-100, 10, dir), -100);
            assert_eq!(round_to_step(0, 10, dir), 0);
        }
    }

    #[test]
    fn a_positive_value_rounds_to_its_neighbours() {
        assert_eq!(round_to_step(104, 10, RoundDir::Down), 100);
        assert_eq!(round_to_step(104, 10, RoundDir::Up), 110);
        assert_eq!(round_to_step(104, 10, RoundDir::TowardZero), 100);
        assert_eq!(round_to_step(104, 10, RoundDir::AwayFromZero), 110);
    }

    /// The case that separates `Down` from `TowardZero`.
    #[test]
    fn a_negative_value_rounds_toward_the_named_infinity() {
        assert_eq!(round_to_step(-104, 10, RoundDir::Down), -110);
        assert_eq!(round_to_step(-104, 10, RoundDir::Up), -100);
        assert_eq!(round_to_step(-104, 10, RoundDir::TowardZero), -100);
        assert_eq!(round_to_step(-104, 10, RoundDir::AwayFromZero), -110);
    }

    #[test]
    fn a_step_wider_than_the_value_still_rounds() {
        assert_eq!(round_to_step(5, 10, RoundDir::Down), 0);
        assert_eq!(round_to_step(5, 10, RoundDir::Up), 10);
        assert_eq!(round_to_step(-5, 10, RoundDir::TowardZero), 0);
        assert_eq!(round_to_step(-5, 10, RoundDir::Down), -10);
    }
}
