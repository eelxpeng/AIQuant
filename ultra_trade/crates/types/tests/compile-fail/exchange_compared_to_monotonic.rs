// TEST-018 — INV-007: comparison across clock kinds, which subtraction does not
// cover. A monotonic reading has an opaque origin, so ordering it against an
// epoch timestamp is meaningless.
// Positive control: TEST-015, where same-kind ordering works.

use types::{ExchangeTime, MonotonicTime};

fn main() {
    let exchange = ExchangeTime::from_nanos(1_786_542_330_123_456_789);
    let monotonic = MonotonicTime::from_nanos(42_500_000_000);

    let _ = exchange < monotonic;
    let _ = exchange == monotonic;
}
