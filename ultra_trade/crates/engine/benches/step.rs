//! How long one market event takes to get through the engine.
//!
//! The hot path is "market-data event in → order out", and this measures
//! exactly that: `Engine::on_inbound` with a real strategy, a real risk gate,
//! and a simulated venue behind it, over a market that actually trades.
//!
//! # Why it is hand-rolled
//!
//! No third-party dependency. The trading path has none, and a benchmark that
//! dragged one in would make `cargo test --workspace` on a fresh clone depend
//! on a registry. `Instant` and arithmetic are enough to answer the question
//! this exists to answer.
//!
//! `Instant` is a monotonic clock read *around* the engine, never inside it.
//! Nothing here is on the engine path, so Constitution VII's rule about
//! injected clocks is not in play.
//!
//! # How to read the numbers
//!
//! Per-event timestamps would cost as much as the work being measured, so this
//! times whole rounds and divides. Several rounds run and the **minimum** is
//! reported alongside the median: the minimum is the least contaminated by
//! whatever else the machine was doing, and for comparing two builds it is the
//! number that moves for real reasons.
//!
//! This is a throughput figure on one machine, not a latency budget. It exists
//! to answer "did this change cost anything", which is a comparison, and the
//! absolute value should not be quoted as if it were a guarantee.
//!
//! ```text
//! cargo bench -p engine
//! ```

use engine::{Engine, EngineConfig};
use event::{EventLog, Inbound, MarketEvent, MarketKind, MemoryLog};
use marketdata::{Aggregator, BarSpec};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use std::time::Instant;
use strategy::MovingAverageCrossover;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);
/// Events per round. Large enough that one `Instant` pair is noise beside it.
const EVENTS: usize = 200_000;
/// Rounds. The minimum of these is the headline.
const ROUNDS: usize = 7;

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

fn instrument() -> Instrument {
    Instrument::new(I, Px::from_scaled(SCALE / 100), qty(1), qty(1)).expect("conventions")
}

fn limits() -> LimitBook {
    let mut book = LimitBook::with_instruments(1);
    book.set(
        I,
        Limits {
            max_position: qty(1_000_000),
            max_exposure: money(1_000_000_000),
            max_order_notional: money(100_000_000),
            // Wide on purpose. A rate limit that bound would turn most events
            // into refusals and measure the refusal path instead of the one
            // that reaches the venue.
            max_orders_in_window: u32::MAX,
            rate_window: ExchangeSpan::from_nanos(60_000_000_000),
            max_quote_age: ExchangeSpan::from_nanos(600_000_000_000),
        },
    )
    .expect("limits");
    book
}

fn build<L: EventLog>(log: L) -> Engine<SimVenue, L> {
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let config = EngineConfig::new(
        vec![instrument()],
        limits(),
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let mut engine = Engine::new(config, venue, log);
    let subscription = engine
        .add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("aggregator"));
    engine
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            I,
            subscription,
            8,
            qty(1),
        )))
        .expect("strategy");
    engine
}

/// A saw-tooth market that makes the crossover cross, so orders really are
/// placed, filled, and accounted for rather than the strategy sitting idle.
fn market(count: usize) -> Vec<Inbound> {
    let mut events = Vec::with_capacity(count);
    for n in 0..count as i64 {
        let at = 1_000_000 * (n + 1);
        let level = 100 + (n % 16) * 2;
        if n % 2 == 0 {
            events.push(Inbound::Market(MarketEvent {
                instrument: I,
                exchange_time: Timestamp::from_nanos(at),
                receive_time: Timestamp::from_nanos(at),
                kind: MarketKind::Quote {
                    bid_px: px(level),
                    bid_qty: qty(1_000),
                    ask_px: px(level + 1),
                    ask_qty: qty(1_000),
                },
            }));
        } else {
            events.push(Inbound::Market(MarketEvent {
                instrument: I,
                exchange_time: Timestamp::from_nanos(at),
                receive_time: Timestamp::from_nanos(at),
                kind: MarketKind::Trade {
                    px: px(level),
                    qty: qty(1),
                    aggressor: Side::Buy,
                },
            }));
        }
    }
    events
}

/// Runs one round and returns nanoseconds per event.
///
/// A fresh engine each round: a warmed one accumulates orders and positions,
/// and measuring round seven of a growing order book is not measuring the same
/// thing as round one.
fn round(events: &[Inbound], replaying: bool) -> (f64, usize) {
    // Capacity for every record the round can produce, so the log never grows
    // mid-measurement. Growing it would be measuring `Vec`, not the engine.
    let mut engine = build(MemoryLog::with_capacity(events.len() * 4));
    if replaying {
        engine.begin_replay();
    }

    // Warm the strategy's window and the venue's book before the clock starts.
    for event in events.iter().take(64) {
        engine.on_inbound(*event).expect("warmup");
    }

    let started = Instant::now();
    for event in events {
        engine.on_inbound(*event).expect("event");
    }
    let elapsed = started.elapsed();

    // Proof the measurement is of the no-allocation path, not of a Vec growing
    // underneath it. A round that would have allocated is not the hot path.
    assert!(
        !engine.would_allocate(),
        "this round outgrew its capacity, so it did not measure the hot path"
    );

    let orders = engine.orders().len();
    (elapsed.as_nanos() as f64 / events.len() as f64, orders)
}

fn report(label: &str, events: &[Inbound], replaying: bool) {
    let mut per_event = Vec::with_capacity(ROUNDS);
    let mut orders = 0;
    for _ in 0..ROUNDS {
        let (ns, placed) = round(events, replaying);
        per_event.push(ns);
        orders = placed;
    }
    per_event.sort_by(|a, b| a.partial_cmp(b).expect("no NaN from a duration"));

    let min = per_event[0];
    let median = per_event[ROUNDS / 2];
    let max = per_event[ROUNDS - 1];
    println!(
        "{label:<28} min {min:7.1} ns/event   median {median:7.1}   max {max:7.1}   \
         ({:.2} M events/s at the minimum, {orders} orders placed)",
        1_000.0 / min
    );
}

fn main() {
    let events = market(EVENTS);
    println!(
        "engine step: {EVENTS} events per round, {ROUNDS} rounds, one instrument, one strategy"
    );
    println!("{}", "-".repeat(78));
    report("live (recording)", &events, false);
    report("replaying (recovery)", &events, true);
    println!();
    println!(
        "The first line is the hot path. The second is what a recovery costs while it\n\
         rebuilds state, which happens once at startup and never during a session."
    );
}
