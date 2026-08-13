// TEST-019 — INV-007: conversion across clock kinds, which arithmetic and
// comparison do not cover. Relating two clocks is reserved for a named skew
// model (ADR #6), not a `From` impl.
// Positive control: TEST-015, where a timestamp round trips its own count.

use types::{ExchangeTime, MonotonicTime};

fn main() {
    let monotonic = MonotonicTime::from_nanos(42_500_000_000);

    let _ = ExchangeTime::from(monotonic);
    let _: ExchangeTime = monotonic.into();
}
