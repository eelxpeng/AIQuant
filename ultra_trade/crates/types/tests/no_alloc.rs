//! TEST-021 — no operation allocates.
//!
//! Proves INV-009. `#[global_allocator]` is per-binary and each integration
//! test file is its own binary, so this counter sees only this target. That
//! isolation is the point: `proptest` allocates freely, so putting this
//! assertion beside a property test would measure the framework.
//!
//! This is the one place `unsafe` appears. It is a test target, so the library's
//! `#![forbid(unsafe_code)]` still holds.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use types::{
    ExchangeSpan, ExchangeTime, MonotonicSpan, MonotonicTime, Notional, Px, Qty, ReceiveTime,
    RoundDir, SCALE,
};

static ARMED: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::SeqCst) {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ARMED.load(Ordering::SeqCst) {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        if ARMED.load(Ordering::SeqCst) {
            ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
        }
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

const DIRS: [RoundDir; 4] = [
    RoundDir::Up,
    RoundDir::Down,
    RoundDir::TowardZero,
    RoundDir::AwayFromZero,
];

/// Every public operation, including the error paths — an error that carried a
/// `String` would show up here.
fn exercise_the_whole_surface() {
    let px = black_box(Px::from_scaled(10_000_100_000));
    let qty = black_box(Qty::from_scaled(1_000));
    let tick = black_box(Px::from_scaled(10_000_000));
    let lot = black_box(Qty::from_scaled(SCALE));

    let _ = black_box(px.to_scaled());
    let _ = black_box(qty.to_scaled());
    let _ = black_box(Notional::from_scaled(1).to_scaled());

    let _ = black_box(Px::try_from_units(700_000));
    let _ = black_box(Px::try_from_units(i64::MAX)); // error path
    let _ = black_box(Qty::try_from_units(1));
    let _ = black_box(Notional::try_from_units(i128::MAX)); // error path

    let _ = black_box(px + px);
    let _ = black_box(px - Px::from_scaled(1));
    let _ = black_box(-px);
    let _ = black_box(qty + qty);
    let _ = black_box(Notional::from_scaled(2) + Notional::from_scaled(3));

    let _ = black_box(px.checked_add(px));
    let _ = black_box(Px::MAX.checked_add(px)); // error path
    let _ = black_box(px.checked_sub(px));
    let _ = black_box(px.checked_neg());
    let _ = black_box(Px::MIN.checked_neg()); // error path
    let _ = black_box(Notional::MAX.checked_add(Notional::from_scaled(1))); // error path

    for dir in DIRS {
        let _ = black_box(px.notional(qty, dir));
        let _ = black_box(px.round_to_tick(tick, dir));
        let _ = black_box(qty.round_to_lot(lot, dir));
        let _ = black_box(px.round_to_tick(Px::ZERO, dir)); // error path
        let _ = black_box(Px::MAX.round_to_tick(tick, dir)); // error path
    }

    let exchange = black_box(ExchangeTime::from_nanos(1_786_542_330_123_456_789));
    let receive = black_box(ReceiveTime::from_nanos(1_786_542_330_124_011_300));
    let mono = black_box(MonotonicTime::from_nanos(42_500_000_000));
    let span = black_box(MonotonicSpan::from_nanos(5_000_000_000));

    let _ = black_box(exchange.to_nanos());
    let _ = black_box(receive.to_nanos());
    let _ = black_box(exchange - ExchangeTime::from_nanos(1));
    let _ = black_box(ExchangeTime::MAX - ExchangeTime::MIN);
    let _ = black_box(mono.checked_add(span));
    let _ = black_box(mono.checked_sub(span));
    let _ = black_box(ExchangeTime::MAX.checked_add(ExchangeSpan::from_nanos(1))); // error path
    let _ = black_box(span + span);
    let _ = black_box(span - MonotonicSpan::from_nanos(1));
    let _ = black_box(-span);
    let _ = black_box(span.checked_add(span));
    let _ = black_box(span.checked_neg());
    let _ = black_box(MonotonicSpan::from_nanos(i128::MIN).checked_neg()); // error path
    let _ = black_box(exchange < ExchangeTime::MAX);
    let _ = black_box(span.cmp(&MonotonicSpan::ZERO));
}

#[test]
fn no_operation_allocates() {
    // Warm up first: whatever the harness or lazy statics allocate happens
    // before the counter is armed.
    exercise_the_whole_surface();

    ARMED.store(true, Ordering::SeqCst);
    exercise_the_whole_surface();
    let count = ALLOCATIONS.load(Ordering::SeqCst);
    ARMED.store(false, Ordering::SeqCst);

    // Disarmed before asserting: a failing `assert` formats, and formatting
    // allocates.
    assert_eq!(count, 0, "value-type operations allocated {count} times");
}
