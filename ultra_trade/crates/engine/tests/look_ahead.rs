//! D3.4: a strategy cannot observe data timestamped after the event it is
//! processing.
//!
//! The compile-level half lives in the `strategy` crate: a strategy is handed
//! no book, no venue, and no way to ask for the next event. This is the other
//! half — a strategy that inspects **every timestamp the engine gives it**
//! across a long session and fails the moment one lies in the future of its own
//! current-event time.
//!
//! The guard runs **inside the engine**, driven by the engine's own dispatch.
//! A test that re-implemented the dispatch could pass while the engine did
//! something else, which is the failure mode this file exists to avoid.
//!
//! Bars are the interesting case. A time bar completes because a *later* event
//! proved its window had closed, so its `close_time` is the window boundary and
//! not the timestamp of the event that revealed it. If that were ever stamped
//! forward, a strategy would be reading a price from a window that had not
//! finished — silently profitable in backtest, and a correctness bug.

use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::{Inbound, MarketEvent, MarketKind, MemoryLog, OrderKind};
use marketdata::{Aggregator, BarSpec, BarSubscription};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use std::cell::RefCell;
use std::rc::Rc;
use strategy::{Context, Strategy, StrategyEvent};
use types::{
    ExchangeSpan, ExchangeTime, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side,
    StrategyId, Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);
const STEPS: i64 = 600;
const STEP_NANOS: i64 = 1_000_000;

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// What the guard saw, shared with the test.
///
/// `Rc<RefCell<_>>` rather than `Arc<Mutex<_>>`: the loop is single-threaded by
/// design, and pretending otherwise in a test would misrepresent it.
#[derive(Debug, Default)]
struct Seen {
    quotes: usize,
    bars: usize,
    violations: Vec<String>,
}

/// Records any timestamp it is shown that sits after the event in hand.
#[derive(Debug)]
struct LookAheadGuard {
    seen: Rc<RefCell<Seen>>,
    dispatches: usize,
}

impl LookAheadGuard {
    fn check(&self, what: &str, observed: ExchangeTime, now: ExchangeTime) {
        if observed > now {
            self.seen.borrow_mut().violations.push(format!(
                "{what}: observed {} while processing an event at {}",
                observed.to_nanos(),
                now.to_nanos()
            ));
        }
    }
}

impl Strategy for LookAheadGuard {
    fn id(&self) -> StrategyId {
        StrategyId::new(0)
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
        let now = ctx.now();
        match event {
            StrategyEvent::Quote { top, .. } => {
                self.check("quote", top.exchange_time, now);
                self.seen.borrow_mut().quotes += 1;
            }
            StrategyEvent::Bar { bar, .. } => {
                self.check("bar close", bar.close_time, now);
                self.check("bar open", bar.open_time, now);
                self.seen.borrow_mut().bars += 1;
            }
            _ => {}
        }
        // Trade occasionally, so the session carries orders and fills rather
        // than being a pure read.
        self.dispatches += 1;
        if self.dispatches.is_multiple_of(97) {
            ctx.order(I, Side::Buy, qty(1), OrderKind::Market);
        }
    }
}

struct Feed {
    events: Vec<Inbound>,
    next: usize,
}

impl FeedAdapter for Feed {
    fn next_event(&mut self) -> Option<Inbound> {
        let event = self.events.get(self.next).copied()?;
        self.next += 1;
        Some(event)
    }
}

/// Quotes and trades on a deterministic zigzag, unevenly spaced so that window
/// boundaries do not line up with events — some windows close empty, others
/// straddle several trades.
fn stream() -> Vec<Inbound> {
    let mut events = Vec::with_capacity(STEPS as usize * 2);
    let mut clock = 0i64;
    for step in 0..STEPS {
        clock += STEP_NANOS + (step % 7) * 130_000;
        let mid = 100 + (step % 20) - 10;
        events.push(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(clock),
            receive_time: Timestamp::from_nanos(clock),
            kind: MarketKind::Quote {
                bid_px: px(mid - 1),
                bid_qty: qty(500),
                ask_px: px(mid + 1),
                ask_qty: qty(500),
            },
        }));
        clock += 250_000;
        events.push(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(clock),
            receive_time: Timestamp::from_nanos(clock),
            kind: MarketKind::Trade {
                px: px(mid),
                qty: qty(1),
                aggressor: Side::Buy,
            },
        }));
    }
    events
}

fn config() -> EngineConfig {
    let instrument =
        Instrument::new(I, Px::from_scaled(10_000_000), qty(1), qty(1)).expect("conventions");
    let mut limits = LimitBook::with_instruments(1);
    limits
        .set(
            I,
            Limits {
                max_position: qty(500),
                max_exposure: money(1_000_000),
                max_order_notional: money(100_000),
                max_orders_in_window: 50,
                rate_window: ExchangeSpan::from_nanos(10_000_000),
                max_quote_age: ExchangeSpan::from_nanos(50_000_000),
            },
        )
        .expect("limits");
    EngineConfig::new(
        vec![instrument],
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    )
}

#[test]
fn a_strategy_never_observes_a_timestamp_later_than_the_event_in_hand() {
    let seen = Rc::new(RefCell::new(Seen::default()));
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(1 << 14));

    // Both bar conventions. The time bar is the one that could look ahead: it
    // closes on the strength of a later event, so its close time is a window
    // boundary rather than that event's timestamp.
    let tick =
        engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 4 }).expect("spec"));
    let timed = engine.add_aggregator(
        Aggregator::new(
            I,
            BarSpec::Time {
                period: ExchangeSpan::from_nanos(3_000_000),
            },
        )
        .expect("spec"),
    );
    assert_eq!(tick, BarSubscription::from_index(0));
    assert_eq!(timed, BarSubscription::from_index(1));

    engine
        .add_strategy(Box::new(LookAheadGuard {
            seen: Rc::clone(&seen),
            dispatches: 0,
        }))
        .expect("strategy");

    let mut feed = Feed {
        events: stream(),
        next: 0,
    };
    run(&mut feed, &mut engine).expect("session");

    let seen = seen.borrow();
    assert!(
        seen.violations.is_empty(),
        "a strategy saw the future {} times:\n{}",
        seen.violations.len(),
        seen.violations.join("\n")
    );

    // The session has to have shown it enough for the absence to mean
    // something. Without these, a strategy that was never dispatched to would
    // pass.
    assert!(seen.quotes >= STEPS as usize, "only {} quotes", seen.quotes);
    assert!(seen.bars > 100, "only {} bars", seen.bars);
    assert!(
        !engine.orders().is_empty(),
        "the session never traded, so the order path went unexercised"
    );
}
