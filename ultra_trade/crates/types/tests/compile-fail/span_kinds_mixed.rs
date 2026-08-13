// TEST-020 — FR-020: a span carries the kind it came from and cannot cross.
// Without this, a receive-derived latency could be added to an exchange
// timestamp — the skew assumption Constitution VII forbids, smuggled in through
// an untyped duration.
// Positive control: TEST-015 and TEST-016, where `Timestamp<K> + Span<K>` works
// and same-kind subtraction is total.

use types::{ExchangeTime, MonotonicTime, ReceiveTime};

fn main() {
    let exchange = ExchangeTime::from_nanos(1_786_542_330_123_456_789);
    let receive_span = ReceiveTime::from_nanos(2) - ReceiveTime::from_nanos(1);
    let exchange_span = ExchangeTime::from_nanos(2) - ExchangeTime::from_nanos(1);
    let monotonic_span = MonotonicTime::from_nanos(2) - MonotonicTime::from_nanos(1);

    let _ = exchange.checked_add(receive_span);
    let _ = receive_span + exchange_span;
    let _ = receive_span < monotonic_span;
}
