//! Runs a session and records it.
//!
//! ```text
//! record <session.conf> <output.log> [steps] [session-id]
//! ```
//!
//! A thin binary. It binds a feed, a venue, a strategy, and a log, and calls
//! the engine — no domain logic of any kind (`docs/ARCHITECTURE.md`).
//!
//! It trades whatever the config declares, against a synthetic market per
//! instrument.
//!
//! **The feed is synthetic**, because no real feed adapter exists yet. That is
//! the only thing separating this from `bin/paper`: swap the synthetic feed for
//! a live one and this *is* paper trading, with the same engine, the same
//! strategy, and the same risk gate. Nothing below the binding knows the
//! difference (Constitution III).
//!
//! What it produces is a recorded session that `bin/backtest` can run against.

use config::SessionConfig;
use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::codec::{InstrumentEntry, LogHeader};
use event::{Inbound, LogWriter, MarketEvent, MarketKind};
use marketdata::Aggregator;
use sim_venue::{Fees, FillModel, SimVenue};
use strategy::MovingAverageCrossover;
use types::{ExchangeSpan, OrderId, Px, Qty, SCALE, Side, StrategyId, Timestamp};

const STEP_NANOS: i64 = 1_000_000_000;
const DEFAULT_STEPS: i64 = 2_000;

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
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
    fn new(steps: i64, instruments: &[types::Instrument]) -> SyntheticFeed {
        let mut events = Vec::with_capacity(steps as usize * 2 * instruments.len());
        for step in 0..steps {
            for (offset, instrument) in instruments.iter().enumerate() {
                // Each instrument walks the same shape out of phase, so a
                // multi-instrument session is not one market copied.
                let mid = price_at(step + offset as i64 * 7);
                let id = instrument.id();
                let at = step * STEP_NANOS + offset as i64;
                events.push(Inbound::Market(MarketEvent {
                    instrument: id,
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
                    instrument: id,
                    exchange_time: Timestamp::from_nanos(trade_at),
                    receive_time: Timestamp::from_nanos(trade_at),
                    kind: MarketKind::Trade {
                        px: px(mid),
                        qty: qty(1),
                        aggressor: if step % 2 == 0 { Side::Buy } else { Side::Sell },
                    },
                }));
            }
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
    eprintln!("usage: record <session.conf> <output.log> [steps] [session-id]");
    eprintln!();
    eprintln!("  session.conf what to trade, under what limits, with which strategies");
    eprintln!("  output.log   where to write the recording; must not exist");
    eprintln!("  steps        market steps to generate (default {DEFAULT_STEPS})");
    eprintln!("  session-id   stamped into the header (default 1)");
    std::process::exit(2);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 2 || args.len() > 4 {
        usage();
    }
    let (config_path, path) = (&args[0], &args[1]);
    let steps = match args.get(2).map(|s| s.parse::<i64>()) {
        None => DEFAULT_STEPS,
        Some(Ok(n)) if n > 0 => n,
        Some(_) => usage(),
    };
    let session_id = match args.get(3).map(|s| s.parse::<u64>()) {
        None => 1,
        Some(Ok(n)) => n,
        Some(Err(_)) => usage(),
    };

    let session = SessionConfig::load(config_path).unwrap_or_else(|e| {
        eprintln!("record: cannot read {config_path}: {e}");
        std::process::exit(1);
    });
    let instruments = session.instrument_list();

    // The header carries the instrument table, so the recording is
    // interpretable on its own.
    let header = LogHeader::new(
        session_id,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        session
            .instruments
            .iter()
            .map(|i| InstrumentEntry::new(i.instrument, &i.symbol).expect("symbol fits"))
            .collect(),
    );

    let writer = match LogWriter::create(path, header) {
        Ok(writer) => writer,
        Err(e) => {
            eprintln!("record: cannot create {path}: {e}");
            std::process::exit(1);
        }
    };

    let config = EngineConfig::new(
        instruments.clone(),
        session.limits.clone(),
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );

    // The two lines that decide what kind of run this is.
    let venue = SimVenue::new(
        instruments.len(),
        FillModel::TouchDisplayed,
        Fees {
            maker: Px::ZERO,
            taker: Px::from_scaled(SCALE / 100), // 0.01 per unit
        },
        ExchangeSpan::from_nanos(0),
    );
    let mut feed = SyntheticFeed::new(steps, &instruments);

    let mut engine = Engine::new(config, venue, writer);
    for (index, spec) in session.strategies.iter().enumerate() {
        let subscription =
            engine.add_aggregator(Aggregator::new(spec.instrument, spec.bars).expect("validated"));
        engine
            .add_strategy(Box::new(MovingAverageCrossover::new(
                StrategyId::new(index as u16),
                spec.instrument,
                subscription,
                spec.window,
                spec.size,
            )))
            .expect("strategy");
    }

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
    for i in &session.instruments {
        println!("  instrument     {} (id {})", i.symbol, i.id.raw());
    }
    println!("  market steps   {steps}");
    println!("  orders         {orders}");
    println!("  final state    {state:?}");
    println!();
    println!("run a backtest over it with:");
    println!("  cargo run -p backtest -- {path} {config_path}");
}
