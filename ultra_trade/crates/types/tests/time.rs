//! TEST-015 — same-kind ordering, subtraction, and checked span application.
//! TEST-016 — timestamp subtraction is total.
//!
//! Proves S3 / INV-007 / INV-013. These are also the positive control for the
//! four clock compile-fail cases: without them, `exchange - receive` failing to
//! build would prove nothing, because subtraction might not exist at all.

use proptest::prelude::*;
use types::{
    ExchangeSpan, ExchangeTime, MonotonicSpan, MonotonicTime, ReceiveSpan, ReceiveTime, ValueError,
};

const CASES: u32 = 1_000;

/// The spec's timestamp vectors.
const EXCHANGE_NANOS: i64 = 1_786_542_330_123_456_789;
const RECEIVE_NANOS: i64 = 1_786_542_330_124_011_300;

// --- TEST-015 -------------------------------------------------------------

#[test]
fn a_timestamp_round_trips_its_nanosecond_count() {
    assert_eq!(
        ExchangeTime::from_nanos(EXCHANGE_NANOS).to_nanos(),
        EXCHANGE_NANOS
    );
    assert_eq!(
        ReceiveTime::from_nanos(RECEIVE_NANOS).to_nanos(),
        RECEIVE_NANOS
    );
    assert_eq!(
        MonotonicTime::from_nanos(42_500_000_000).to_nanos(),
        42_500_000_000
    );
    assert_eq!(ExchangeTime::MIN.to_nanos(), i64::MIN);
    assert_eq!(ExchangeTime::MAX.to_nanos(), i64::MAX);
}

#[test]
fn timestamps_of_one_kind_are_ordered_by_their_count() {
    let early = ExchangeTime::from_nanos(EXCHANGE_NANOS);
    let late = ExchangeTime::from_nanos(EXCHANGE_NANOS + 1);

    assert!(early < late);
    assert!(late > early);
    assert_eq!(early, ExchangeTime::from_nanos(EXCHANGE_NANOS));
    assert_ne!(early, late);
}

/// Out-of-order and duplicate timestamps compare as given. Ordering policy
/// belongs to the event layer, not to a value type.
#[test]
fn equal_and_out_of_order_timestamps_compare_as_given() {
    let a = ReceiveTime::from_nanos(10);
    let b = ReceiveTime::from_nanos(10);
    let earlier = ReceiveTime::from_nanos(9);

    assert_eq!(a, b);
    assert_eq!((a - b).to_nanos(), 0);
    assert_eq!((earlier - a).to_nanos(), -1);
}

#[test]
fn subtracting_two_timestamps_of_one_kind_gives_a_span_of_that_kind() {
    let a = ReceiveTime::from_nanos(RECEIVE_NANOS);
    let b = ReceiveTime::from_nanos(EXCHANGE_NANOS);
    let span: ReceiveSpan = a - b;

    assert_eq!(span.to_nanos(), 554_511);
    assert_eq!(span, ReceiveSpan::from_nanos(554_511));
    assert!(span > ReceiveSpan::ZERO);
}

#[test]
fn a_span_can_be_built_directly_with_its_kind_named() {
    let five_seconds = MonotonicSpan::from_nanos(5_000_000_000);
    assert_eq!(five_seconds.to_nanos(), 5_000_000_000);
    assert_eq!(MonotonicSpan::ZERO.to_nanos(), 0);
}

#[test]
fn a_span_applied_to_a_timestamp_of_its_own_kind_moves_it() {
    let now = MonotonicTime::from_nanos(42_500_000_000);
    let five_seconds = MonotonicSpan::from_nanos(5_000_000_000);

    let later = now.checked_add(five_seconds).unwrap();
    assert_eq!(later.to_nanos(), 47_500_000_000);
    assert_eq!(later.checked_sub(five_seconds).unwrap(), now);
    assert_eq!(later - now, five_seconds);
}

#[test]
fn applying_a_span_past_the_window_is_rejected() {
    let one = ExchangeSpan::from_nanos(1);

    assert_eq!(
        ExchangeTime::MAX.checked_add(one),
        Err(ValueError::Overflow)
    );
    assert_eq!(
        ExchangeTime::MIN.checked_sub(one),
        Err(ValueError::Overflow)
    );
    assert_eq!(
        ExchangeTime::MAX.checked_add(ExchangeSpan::ZERO),
        Ok(ExchangeTime::MAX)
    );

    // A span far wider than the window fails rather than wrapping.
    let huge = ExchangeSpan::from_nanos(i128::MAX);
    assert_eq!(
        ExchangeTime::from_nanos(0).checked_add(huge),
        Err(ValueError::Overflow)
    );
}

#[test]
fn span_arithmetic_is_checked_at_the_boundary() {
    let max = MonotonicSpan::from_nanos(i128::MAX);
    let one = MonotonicSpan::from_nanos(1);

    assert_eq!(max.checked_add(one), Err(ValueError::Overflow));
    assert_eq!(
        MonotonicSpan::from_nanos(i128::MIN).checked_sub(one),
        Err(ValueError::Overflow)
    );
    assert_eq!(
        MonotonicSpan::from_nanos(i128::MIN).checked_neg(),
        Err(ValueError::OutOfRange)
    );
    assert_eq!(max.checked_neg(), Ok(MonotonicSpan::from_nanos(-i128::MAX)));
    assert_eq!(max.checked_add(MonotonicSpan::ZERO), Ok(max));
}

#[test]
fn interior_span_arithmetic_adds_and_negates() {
    let a = ExchangeSpan::from_nanos(300);
    let b = ExchangeSpan::from_nanos(200);

    assert_eq!((a + b).to_nanos(), 500);
    assert_eq!((a - b).to_nanos(), 100);
    assert_eq!((-a).to_nanos(), -300);
}

// --- TEST-016 -------------------------------------------------------------

/// INV-013: the widest difference two `i64` nanosecond counts can have is
/// `2^64 - 1 ≈ 1.845e19`, which an `i64` cannot hold. This is why a span is
/// `i128` and why the subtraction has no `Result`.
#[test]
fn the_widest_timestamp_difference_does_not_overflow() {
    let widest = ExchangeTime::MAX - ExchangeTime::MIN;
    assert_eq!(widest.to_nanos(), (i64::MAX as i128) - (i64::MIN as i128));
    assert_eq!(widest.to_nanos(), 18_446_744_073_709_551_615);
    assert!(widest.to_nanos() > i64::MAX as i128);

    let negated = ExchangeTime::MIN - ExchangeTime::MAX;
    assert_eq!(negated.to_nanos(), -18_446_744_073_709_551_615);

    // Every kind, not just one.
    assert_eq!(
        (ReceiveTime::MAX - ReceiveTime::MIN).to_nanos(),
        18_446_744_073_709_551_615
    );
    assert_eq!(
        (MonotonicTime::MAX - MonotonicTime::MIN).to_nanos(),
        18_446_744_073_709_551_615
    );
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(CASES))]

    #[test]
    fn timestamp_subtraction_is_total(a in any::<i64>(), b in any::<i64>()) {
        let span = ExchangeTime::from_nanos(a) - ExchangeTime::from_nanos(b);
        prop_assert_eq!(span.to_nanos(), (a as i128) - (b as i128));
    }

    #[test]
    fn timestamp_ordering_is_exact(a in any::<i64>(), b in any::<i64>()) {
        prop_assert_eq!(
            ExchangeTime::from_nanos(a).cmp(&ExchangeTime::from_nanos(b)),
            a.cmp(&b)
        );
    }

    /// Adding a span and taking it back is the identity whenever the forward
    /// step is representable.
    #[test]
    fn applying_and_removing_a_span_round_trips(start in any::<i64>(), delta in any::<i32>()) {
        let span = MonotonicSpan::from_nanos(delta as i128);
        let ts = MonotonicTime::from_nanos(start);
        if let Ok(moved) = ts.checked_add(span) {
            prop_assert_eq!(moved.checked_sub(span), Ok(ts));
            prop_assert_eq!(moved - ts, span);
        }
    }
}
