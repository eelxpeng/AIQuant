//! Proof that a warmed engine does not allocate while processing an event.
//!
//! The hot path is market-data event in → order out, and on it there is no
//! allocation (Constitution VI). Reserving capacity is not the same as proving
//! nothing grows, so this counts.
//!
//! It lives in its own test binary because it installs a global allocator, and
//! it contains exactly one test so that no other test's allocations land in the
//! window being measured.

use engine::{Engine, EngineConfig, FeedAdapter};
use event::{MemoryLog, OrderKind};
use marketdata::{Aggregator, BarSpec};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, Queue, SimVenue};
use simkit::{Script, qty};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};
use strategy::{Context, Strategy, StrategyEvent};
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Side, StrategyId, Timestamp,
};

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

struct Counting;

// SAFETY: every method forwards to the system allocator unchanged; the only
// addition is a counter increment.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc_zeroed(layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

const I: InstrumentId = InstrumentId::new(0);

/// Orders on every quote, so the measured event runs the whole path: book
/// update, dispatch, risk gate, venue submit, fill, position update, and the
/// log appends for each.
#[derive(Debug)]
struct AlwaysQuote;

impl Strategy for AlwaysQuote {
    fn id(&self) -> StrategyId {
        StrategyId::new(0)
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
        if let StrategyEvent::Quote { instrument, .. } = event {
            ctx.order(*instrument, Side::Buy, qty(1), OrderKind::Market);
        }
    }
}

#[test]
fn a_warmed_engine_does_not_allocate_while_processing_an_event() {
    let instrument =
        Instrument::new(I, Px::from_scaled(10_000_000), qty(1), qty(1)).expect("conventions");
    let mut book = LimitBook::with_instruments(1);
    book.set(
        I,
        Limits {
            max_position: qty(1_000_000),
            max_exposure: Notional::from_scaled(i128::MAX / 4),
            max_order_notional: Notional::from_scaled(i128::MAX / 4),
            max_orders_in_window: 1_000_000,
            rate_window: ExchangeSpan::from_nanos(1),
            max_quote_age: ExchangeSpan::from_nanos(1_000_000_000),
        },
    )
    .expect("limits");

    let config = EngineConfig::new(
        vec![instrument],
        book,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config, venue, MemoryLog::with_capacity(1 << 16));
    let _ =
        engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 4 }).expect("spec"));
    engine
        .add_strategy(Box::new(AlwaysQuote))
        .expect("strategy");

    // Warm every buffer: whatever grows, grows now.
    let mut warmup = Script::new(I)
        .quote(1_000, 99, 1_000, 101, 1_000)
        .trade(1_100, 100, 1, Side::Buy)
        .quote(2_000, 99, 1_000, 101, 1_000)
        .trade(2_100, 100, 1, Side::Buy)
        .quote(3_000, 99, 1_000, 101, 1_000)
        .build();
    while let Some(event) = warmup.next_event() {
        engine.on_inbound(event).expect("warmup");
    }
    assert!(
        !engine.would_allocate(),
        "the warmup should have left spare capacity everywhere"
    );

    // One more quote, measured. It buys, fills, and books the position.
    let measured = Script::new(I).quote(4_000, 99, 1_000, 101, 1_000).build();
    let event = measured.remaining();
    assert_eq!(event, 1);
    let mut measured = measured;
    let event = measured.next_event().expect("one quote");

    let before_orders = engine.orders().len();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    engine.on_inbound(event).expect("measured event");
    let after = ALLOCATIONS.load(Ordering::Relaxed);

    assert_eq!(
        after - before,
        0,
        "processing one market-data event allocated {} times",
        after - before
    );
    // The measured event really did run the whole path.
    assert_eq!(engine.orders().len(), before_orders + 1);
    assert!(!engine.log().would_grow());
}
