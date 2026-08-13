//! Runs a session and records it.
//!
//! ```text
//! record <output.log> [steps] [session-id]
//! ```
//!
//! A thin binary. It binds a feed, a venue, a strategy, and a log, and calls
//! the engine — no domain logic of any kind (`docs/ARCHITECTURE.md`).
//!
//! **The feed is synthetic**, because no real feed adapter exists yet. That is
//! the only thing separating this from `bin/paper`: swap the synthetic feed for
//! a live one and this *is* paper trading, with the same engine, the same
//! strategy, and the same risk gate. Nothing below the binding knows the
//! difference (Constitution III).
//!
//! What it produces is a recorded session that `bin/backtest` can run against.

use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::codec::{InstrumentEntry, LogHeader};
use event::{Inbound, LogWriter, MarketEvent, MarketKind};
use marketdata::{Aggregator, BarSpec};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use strategy::MovingAverageCrossover;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const INSTRUMENT: InstrumentId = InstrumentId::new(0);
const SYMBOL: &str = "SYNTH";
const STEP_NANOS: i64 = 1_000_000_000;
const DEFAULT_STEPS: i64 = 2_000;

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// Tick 0.01, lot 1, minimum 1.
fn instrument() -> Instrument {
    Instrument::new(INSTRUMENT, Px::from_scaled(10_000_000), qty(1), qty(1))
        .expect("instrument conventions")
}

/// A deterministic price path: a triangular wave between 90 and 110.
///
/// Deterministic by construction rather than by seeding a generator. An ambient
/// random source on the input side makes two recordings of "the same" session
/// disagree, and a recording that cannot be reproduced is not evidence of
/// anything (Constitution II).
fn price_at(step: i64) -> i64 {
    let phase = step.rem_euclid(40);
    if phase < 20 {
        90 + phase
    } else {
        110 - (phase - 20)
    }
}

/// One quote and one trade per step.
struct SyntheticFeed {
    events: Vec<Inbound>,
    next: usize,
}

impl SyntheticFeed {
    fn new(steps: i64) -> SyntheticFeed {
        let mut events = Vec::with_capacity(steps as usize * 2);
        for step in 0..steps {
            let mid = price_at(step);
            let at = step * STEP_NANOS;
            events.push(Inbound::Market(MarketEvent {
                instrument: INSTRUMENT,
                exchange_time: Timestamp::from_nanos(at),
                receive_time: Timestamp::from_nanos(at),
                kind: MarketKind::Quote {
                    bid_px: px(mid - 1),
                    bid_qty: qty(500),
                    ask_px: px(mid + 1),
                    ask_qty: qty(500),
                },
            }));
            let trade_at = at + STEP_NANOS / 2;
            events.push(Inbound::Market(MarketEvent {
                instrument: INSTRUMENT,
                exchange_time: Timestamp::from_nanos(trade_at),
                receive_time: Timestamp::from_nanos(trade_at),
                kind: MarketKind::Trade {
                    px: px(mid),
                    qty: qty(1),
                    aggressor: if step % 2 == 0 { Side::Buy } else { Side::Sell },
                },
            }));
        }
        SyntheticFeed { events, next: 0 }
    }
}

impl FeedAdapter for SyntheticFeed {
    fn next_event(&mut self) -> Option<Inbound> {
        let event = self.events.get(self.next).copied()?;
        self.next += 1;
        Some(event)
    }
}

fn usage() -> ! {
    eprintln!("usage: record <output.log> [steps] [session-id]");
    eprintln!();
    eprintln!("  output.log   where to write the recording; must not exist");
    eprintln!("  steps        market steps to generate (default {DEFAULT_STEPS})");
    eprintln!("  session-id   stamped into the header (default 1)");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else { usage() };
    let steps = match args.get(1).map(|s| s.parse::<i64>()) {
        None => DEFAULT_STEPS,
        Some(Ok(n)) if n > 0 => n,
        Some(_) => usage(),
    };
    let session_id = match args.get(2).map(|s| s.parse::<u64>()) {
        None => 1,
        Some(Ok(n)) => n,
        Some(Err(_)) => usage(),
    };

    let instrument = instrument();
    let mut limits = LimitBook::with_instruments(1);
    limits
        .set(
            INSTRUMENT,
            Limits {
                max_position: qty(50),
                max_exposure: money(100_000),
                max_order_notional: money(50_000),
                max_orders_in_window: 20,
                rate_window: ExchangeSpan::from_nanos(STEP_NANOS as i128 * 10),
                max_quote_age: ExchangeSpan::from_nanos(STEP_NANOS as i128 * 5),
            },
        )
        .expect("limits");

    // The header carries the instrument table, so the recording is
    // interpretable on its own — without it, `InstrumentId(0)` means nothing to
    // anyone who no longer has this binary's configuration.
    let header = LogHeader::new(
        session_id,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument, SYMBOL).expect("symbol fits")],
    );

    let writer = match LogWriter::create(path, header) {
        Ok(writer) => writer,
        Err(e) => {
            eprintln!("record: cannot create {path}: {e}");
            std::process::exit(1);
        }
    };

    let config = EngineConfig::new(
        vec![instrument],
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );

    // The two lines that decide what kind of run this is.
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees {
            maker: Px::ZERO,
            taker: Px::from_scaled(SCALE / 100), // 0.01 per unit
        },
        ExchangeSpan::from_nanos(0),
    );
    let mut feed = SyntheticFeed::new(steps);

    let mut engine = Engine::new(config, venue, writer);
    let subscription = engine
        .add_aggregator(Aggregator::new(INSTRUMENT, BarSpec::Tick { threshold: 1 }).expect("spec"));
    engine
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            INSTRUMENT,
            subscription,
            10,
            qty(5),
        )))
        .expect("strategy");

    if let Err(e) = run(&mut feed, &mut engine) {
        eprintln!("record: session stopped: {e:?}");
        std::process::exit(1);
    }

    // Capture the summary before taking the engine apart.
    let records = engine.log().records();
    let orders = engine.orders().len();
    let state = engine.state();

    // Persist before reporting, and report a failure rather than dropping it:
    // a summary describing records still sitting in a buffer would be
    // describing a file that does not say that yet.
    let mut log = engine.into_log();
    if let Err(e) = log.sync() {
        eprintln!("record: could not persist {path}: {e}");
        std::process::exit(1);
    }

    println!("recorded {records} records to {path}");
    println!("  session id     {session_id}");
    println!("  instrument     {SYMBOL} (id {})", INSTRUMENT.raw());
    println!("  market steps   {steps}");
    println!("  orders         {orders}");
    println!("  final state    {state:?}");
    println!();
    println!("run a backtest over it with:");
    println!("  cargo run -p backtest -- {path}");
}
