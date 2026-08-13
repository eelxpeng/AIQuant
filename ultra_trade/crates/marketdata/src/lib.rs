//! Book building and bar aggregation.
//!
//! Two jobs, and the second one is why this crate exists as a boundary rather
//! than as a helper module:
//!
//! 1. **Book state.** [`Books`] holds top-of-book and last-trade per
//!    instrument, indexed densely so a read is a bounds check.
//! 2. **Aggregation machinery.** [`Aggregator`] owns the windowing and the rule
//!    for when a bar is complete. Bar *definitions* are configuration; the
//!    completion rule is not (`docs/ARCHITECTURE.md` seam 6).
//!
//! A strategy subscribes to the aggregates it wants and never implements
//! aggregation, because "using a bar's close before the bar is complete" is
//! look-ahead, is silently profitable in backtest, and must not be
//! re-implementable per strategy.
//!
//! This crate knows nothing about orders or positions.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod bar;
mod book;

pub use bar::{AggregateError, Aggregator, Aggregators, Bar, BarSpec, BarSubscription};
pub use book::{Books, LastTrade, MarkRule, TopOfBook};

use types::RoundDir;

/// Halves a widened value in the named direction.
///
/// Exists because a midpoint is the one place this crate divides, and division
/// needs a stated rounding direction like every other boundary
/// (Constitution VII). `div_euclid` floors rather than truncating, so a
/// negative sum rounds consistently with a positive one.
pub(crate) fn halve(sum: i128, dir: RoundDir) -> i128 {
    let floor = sum.div_euclid(2);
    if floor * 2 == sum {
        return floor;
    }
    match dir {
        RoundDir::Down => floor,
        RoundDir::Up => floor + 1,
        RoundDir::TowardZero => {
            if sum > 0 {
                floor
            } else {
                floor + 1
            }
        }
        RoundDir::AwayFromZero => {
            if sum > 0 {
                floor + 1
            } else {
                floor
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_exact_half_is_returned_unchanged_whatever_the_direction() {
        for dir in [
            RoundDir::Up,
            RoundDir::Down,
            RoundDir::TowardZero,
            RoundDir::AwayFromZero,
        ] {
            assert_eq!(halve(8, dir), 4);
            assert_eq!(halve(-8, dir), -4);
        }
    }

    #[test]
    fn an_inexact_half_moves_the_way_the_caller_named() {
        assert_eq!(halve(3, RoundDir::Down), 1);
        assert_eq!(halve(3, RoundDir::Up), 2);
        assert_eq!(halve(3, RoundDir::TowardZero), 1);
        assert_eq!(halve(3, RoundDir::AwayFromZero), 2);
    }

    #[test]
    fn a_negative_inexact_half_floors_rather_than_truncating() {
        assert_eq!(halve(-3, RoundDir::Down), -2);
        assert_eq!(halve(-3, RoundDir::Up), -1);
        assert_eq!(halve(-3, RoundDir::TowardZero), -1);
        assert_eq!(halve(-3, RoundDir::AwayFromZero), -2);
    }
}
