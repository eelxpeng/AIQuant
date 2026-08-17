//! The one place the exchange clock and the receive clock are compared.
//!
//! Everywhere else in this workspace that comparison does not compile, and
//! that is the point: mixing the clocks in market-state logic reads the market
//! as of a moment it was never in. Latency measurement is the single exception
//! `CONTEXT.md` names, so it gets one function, tested, rather than a
//! subtraction somebody talks themselves into at a call site.

use types::{ExchangeTime, ReceiveTime, Timestamp, feed_lag_nanos};

fn exchange(n: i64) -> ExchangeTime {
    Timestamp::from_nanos(n)
}

fn receive(n: i64) -> ReceiveTime {
    Timestamp::from_nanos(n)
}

#[test]
fn data_that_arrived_after_the_venue_stamped_it_lags_by_the_difference() {
    // The ordinary case: six seconds behind, which is what polling costs.
    assert_eq!(
        feed_lag_nanos(exchange(1_000_000_000), receive(7_000_000_000)),
        6_000_000_000
    );
}

#[test]
fn data_that_arrived_the_instant_it_was_stamped_has_no_lag() {
    assert_eq!(feed_lag_nanos(exchange(42), receive(42)), 0);
}

#[test]
fn a_local_clock_behind_the_venues_reads_negative_rather_than_zero() {
    // Clock skew is real and this is how it shows up. Clamping it to zero
    // would hide the one condition that makes every other lag figure a lie.
    assert_eq!(feed_lag_nanos(exchange(5_000), receive(4_000)), -1_000);
}

#[test]
fn the_extremes_saturate_rather_than_wrap() {
    // A wrapped lag would report a fast feed as an impossibly slow one, or the
    // reverse. Neither is a number anyone should act on.
    assert_eq!(
        feed_lag_nanos(exchange(i64::MIN), receive(i64::MAX)),
        i64::MAX
    );
    assert_eq!(
        feed_lag_nanos(exchange(i64::MAX), receive(i64::MIN)),
        i64::MIN
    );
}
