//! Runs a backtest.
//!
//! A thin binary that binds a feed, a venue, and a strategy, and calls the
//! engine. It holds no domain logic of any kind: the loop, the risk gate, the
//! order machine, and the accounting all live in crates below it
//! (`docs/ARCHITECTURE.md`).
//!
//! What makes this a *backtest* rather than a live session is exactly two
//! lines: the feed it binds and the venue it binds. Nothing downstream knows
//! which, and there is no mode flag anywhere in the program
//! (Constitution III).
//!
//! **The feed here is synthetic.** Replaying recorded market data is
//! `adapters/historical`, which is not built yet — that needs a recorded-data
//! format, which is a decision of its own (#1). Until it exists, this binary
//! proves the wiring rather than a strategy.

use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::{Inbound, MarketEvent, MarketKind, MemoryLog, Outbound};
use marketdata::{Aggregator, BarSpec, BarSubscription};
use report::summarize;
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use strategy::MovingAverageCrossover;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const INSTRUMENT: InstrumentId = InstrumentId::new(0);
const SUBSCRIPTION: BarSubscription = BarSubscription::from_index(0);
const STEPS: i64 = 400;
/// One step of simulated market time.
const STEP_NANOS: i64 = 1_000_000_000;

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// A deterministic price path: a triangular wave between 90 and 110.
///
/// Deterministic by construction rather than by seeding a generator. A random
/// path would need a random source, and an ambient one on the input side makes
/// two runs of the same backtest disagree (Constitution II).
fn price_at(step: i64) -> i64 {
    let phase = step.rem_euclid(40);
    if phase < 20 {
        90 + phase
    } else {
        110 - (phase - 20)
    }
}

/// A feed built from the price path, one quote and one trade per step.
struct SyntheticFeed {
    events: Vec<Inbound>,
    next: usize,
}

impl SyntheticFeed {
    fn new(steps: i64) -> SyntheticFeed {
        let mut events = Vec::with_capacity(steps as usize * 2);
        for step in 0..steps {
            let at = Timestamp::from_nanos(step * STEP_NANOS);
            let mid = price_at(step);
            events.push(Inbound::Market(MarketEvent {
                instrument: INSTRUMENT,
                exchange_time: at,
                receive_time: Timestamp::from_nanos(step * STEP_NANOS),
                kind: MarketKind::Quote {
                    bid_px: px(mid - 1),
                    bid_qty: qty(500),
                    ask_px: px(mid + 1),
                    ask_qty: qty(500),
                },
            }));
            let trade_at = Timestamp::from_nanos(step * STEP_NANOS + STEP_NANOS / 2);
            events.push(Inbound::Market(MarketEvent {
                instrument: INSTRUMENT,
                exchange_time: trade_at,
                receive_time: Timestamp::from_nanos(step * STEP_NANOS + STEP_NANOS / 2),
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

/// Renders a fixed-point value as a decimal.
///
/// Formatting, and therefore off the hot path by construction — this runs once,
/// after the session (Constitution VI).
fn decimal(scaled: i128) -> String {
    let unit = SCALE as i128;
    let sign = if scaled < 0 { "-" } else { "" };
    let magnitude = scaled.unsigned_abs();
    let whole = magnitude / unit as u128;
    let frac = magnitude % unit as u128;
    format!("{sign}{whole}.{frac:09}")
}

fn main() {
    // Tick 0.01, lot 1, minimum 1.
    let instrument = Instrument::new(INSTRUMENT, Px::from_scaled(10_000_000), qty(1), qty(1))
        .expect("instrument conventions");

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

    let config = EngineConfig::new(
        vec![instrument],
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );

    // The two lines that make this a backtest. Swap them and the same engine,
    // the same strategy, and the same risk gate run a live session.
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees {
            maker: Px::from_scaled(0),
            taker: Px::from_scaled(SCALE / 100), // 0.01 per unit
        },
        ExchangeSpan::from_nanos(0),
    );
    let mut feed = SyntheticFeed::new(STEPS);

    let mut engine = Engine::new(config, venue, MemoryLog::with_capacity(1 << 14));
    let _ = engine.add_aggregator(
        Aggregator::new(INSTRUMENT, BarSpec::Tick { threshold: 1 }).expect("bar spec"),
    );
    engine
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            INSTRUMENT,
            SUBSCRIPTION,
            10,
            qty(5),
        )))
        .expect("strategy");

    if let Err(e) = run(&mut feed, &mut engine) {
        eprintln!("session stopped: {e:?}");
        std::process::exit(1);
    }

    let summary = summarize(engine.log().records(), 1).expect("summary");
    let position = engine.positions().get(INSTRUMENT).expect("position");

    println!("backtest — synthetic triangular path, {STEPS} steps");
    println!("  records            {}", summary.records);
    println!(
        "  inputs / decisions {} / {}",
        summary.inputs, summary.decisions
    );
    println!("  orders submitted   {}", summary.orders_submitted);
    println!("  fills              {}", summary.fills);
    println!("  intents refused    {}", summary.intents_rejected);
    for (reason, count) in &summary.rejections {
        println!("      {reason:?}: {count}");
    }
    println!(
        "  realized           {}",
        decimal(summary.realized.to_scaled())
    );
    println!("  fees               {}", decimal(summary.fees.to_scaled()));
    println!(
        "  max drawdown       {}",
        decimal(summary.max_drawdown.to_scaled())
    );
    println!(
        "  final position     {}",
        decimal(position.qty().to_scaled() as i128)
    );
    println!("  final state        {:?}", summary.final_state);

    // The log is the whole story: every order in it names the record that
    // caused it, so this line is a pointer rather than a second account.
    let first_order = engine
        .log()
        .records()
        .iter()
        .find_map(|r| match r.event.as_outbound() {
            Some(Outbound::OrderSubmitted { caused_by, .. }) => Some((r.seq, *caused_by)),
            _ => None,
        });
    if let Some((seq, caused_by)) = first_order {
        println!("  first order at {seq}, caused by {caused_by}");
    }
}
