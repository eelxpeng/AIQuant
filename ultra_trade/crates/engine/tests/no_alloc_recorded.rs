//! The point of the background writer: a session that is being **recorded to a
//! file** still does not allocate while processing an event.
//!
//! `no_alloc.rs` proves it for a session logging to memory. That is the easy
//! case — nothing there goes near a syscall. This is the one that matters for
//! live trading, because a durable log is not optional and writing on the
//! engine thread is not allowed (Constitution VI).
//!
//! The counter is global, so it sees the writer thread too. That is
//! deliberate: if the writer allocated per record, the design would be wrong in
//! a way a producer-only measurement would hide.
//!
//! Its own test binary, with one test, so no other test's allocations land in
//! the measured window.

use engine::{Engine, EngineConfig, FeedAdapter};
use event::codec::{InstrumentEntry, LogHeader};
use event::{BackgroundLog, OrderKind};
use marketdata::{Aggregator, BarSpec};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use simkit::{Script, qty};
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
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

/// Orders on every quote, so the measured event runs the whole path.
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
fn a_session_being_recorded_to_a_file_does_not_allocate_while_processing_an_event() {
    let mut path: PathBuf = std::env::temp_dir();
    path.push(format!("ultra_trade-noalloc-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&path);

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

    let header = LogHeader::new(
        1,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument, "NOALLOC").expect("entry")],
    );
    // A ring big enough that the session cannot fill it, so the measured event
    // is a plain hand-off rather than a refusal.
    let log = BackgroundLog::create(&path, header, 1 << 16).expect("create");

    let config = EngineConfig::new(
        vec![instrument],
        book,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config, venue, log);
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
        "the warmup left no spare capacity"
    );

    // Let the writer thread reach its steady state, so nothing it does lazily
    // on first use lands inside the window.
    std::thread::sleep(std::time::Duration::from_millis(50));

    let mut measured = Script::new(I).quote(4_000, 99, 1_000, 101, 1_000).build();
    let event = measured.next_event().expect("one quote");

    let before_orders = engine.orders().len();
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    engine.on_inbound(event).expect("measured event");
    let after = ALLOCATIONS.load(Ordering::Relaxed);

    assert_eq!(
        after - before,
        0,
        "recording one market-data event allocated {} times",
        after - before
    );
    // The measured event really did run the whole path.
    assert_eq!(engine.orders().len(), before_orders + 1);

    // And what it recorded is on disk when the session ends.
    let log = engine.into_log();
    let report = log.shutdown().expect("shutdown");
    assert_eq!(report.handed_off, report.written);
    assert!(report.written > 0);
    let mut reader = event::LogReader::open(&path).expect("open");
    let records = reader.read_all_intact().expect("intact");
    assert_eq!(records.len() as u64, report.written);
    let _ = std::fs::remove_file(&path);
}
