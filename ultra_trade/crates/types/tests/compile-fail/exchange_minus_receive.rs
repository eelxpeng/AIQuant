// TEST-017 — INV-007: the skew subtraction, and the whole point of typed
// clocks. These two instants are 554_511 ns apart and the difference means
// nothing.
// Positive control: TEST-015, where same-kind subtraction works and yields a
// span of that kind.

use types::{ExchangeTime, ReceiveTime};

fn main() {
    let exchange = ExchangeTime::from_nanos(1_786_542_330_123_456_789);
    let receive = ReceiveTime::from_nanos(1_786_542_330_124_011_300);

    let _ = exchange - receive;
    let _ = receive - exchange;
}
