//! Backtests a strategy over a recorded session.
//!
//! ```text
//! backtest <recorded.log> [session.conf]
//! ```
//!
//! A thin binary. It binds a feed, a venue, and a strategy, and calls the
//! engine — no domain logic of any kind (`docs/ARCHITECTURE.md`).
//!
//! What makes this a *backtest* rather than a live session is exactly two
//! bindings: [`HistoricalFeed`] instead of a live feed, and [`SimVenue`]
//! instead of a real one. Nothing downstream of them knows, and there is no
//! mode flag anywhere in the program (Constitution III).
//!
//! # Where the instruments come from
//!
//! The recording's header, not this file. A backtest that configured its own
//! tick and lot sizes could disagree with what was recorded, and then
//! `InstrumentId(0)` would silently mean a different contract. Taking them from
//! the header makes that impossible rather than merely checked.
//!
//! Risk limits and strategies are *not* taken from the header. They are what
//! this run chooses, not a property of the market that was recorded, so they
//! come from a config — the same config a paper session would use. Given one,
//! a backtest over a paper recording reproduces it; given none, it falls back
//! to limits wide enough not to bind by accident and one crossover on the first
//! instrument.

use config::{SessionConfig, StrategyConfig};
use engine::{Engine, EngineConfig, run};
use event::{LogReader, Outbound};
use historical::{HistoricalFeed, Replaying};
use marketdata::{Aggregator, BarSpec};
use report::summarize;
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use strategy::MovingAverageCrossover;
use types::{ExchangeSpan, Instrument, Notional, OrderId, Px, Qty, SCALE, StrategyId};

const STEP_NANOS: i128 = 1_000_000_000;

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// Renders a fixed-point value as a decimal.
///
/// Formatting, and off the hot path by construction: this runs once, after the
/// session (Constitution VI).
fn decimal(scaled: i128) -> String {
    let unit = SCALE as i128;
    let sign = if scaled < 0 { "-" } else { "" };
    let magnitude = scaled.unsigned_abs();
    format!(
        "{sign}{}.{:09}",
        magnitude / unit as u128,
        magnitude % unit as u128
    )
}

fn usage() -> ! {
    eprintln!("usage: backtest <recorded.log> [session.conf]");
    eprintln!();
    eprintln!("  recorded.log   a session written by `record` or `paper`");
    eprintln!("  session.conf   limits and strategies for this run; without it,");
    eprintln!("                 wide limits and one crossover on the first instrument");
    eprintln!();
    eprintln!("give the config the recording was made under and the backtest");
    eprintln!("reproduces it; give a different one to ask what would have happened.");
    std::process::exit(2);
}

fn fail(context: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("backtest: {context}: {e}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else { usage() };
    if args.len() > 2 {
        usage();
    }

    // The instruments come from the recording, so this run cannot disagree with
    // it about what `InstrumentId(0)` means.
    let reader = match LogReader::open(path) {
        Ok(reader) => reader,
        Err(e) => fail(&format!("cannot read {path}"), e),
    };
    let header = reader.header().clone();
    let instruments: Vec<Instrument> = header
        .instruments
        .iter()
        .map(|entry| {
            Instrument::new(entry.id, entry.tick, entry.lot, entry.min_qty)
                .unwrap_or_else(|e| fail("the recording's instrument table is not usable", e))
        })
        .collect();
    if instruments.is_empty() {
        fail("the recording has no instruments", "nothing to trade");
    }
    drop(reader);

    // Limits and strategies are this run's policy.
    let session = args.get(1).map(|conf| {
        SessionConfig::load(conf).unwrap_or_else(|e| fail(&format!("cannot read {conf}"), e))
    });
    let (limits, strategies) = match &session {
        Some(session) => {
            if session.instruments.len() != instruments.len() {
                fail(
                    "the config and the recording disagree",
                    format!(
                        "the recording holds {} instruments, the config declares {}",
                        instruments.len(),
                        session.instruments.len()
                    ),
                );
            }
            (session.limits.clone(), session.strategies.clone())
        }
        None => {
            let mut limits = LimitBook::with_instruments(instruments.len());
            for instrument in &instruments {
                limits
                    .set(
                        instrument.id(),
                        Limits {
                            max_position: qty(1_000),
                            max_exposure: money(10_000_000),
                            max_order_notional: money(1_000_000),
                            max_orders_in_window: 60,
                            rate_window: ExchangeSpan::from_nanos(STEP_NANOS * 60),
                            max_quote_age: ExchangeSpan::from_nanos(STEP_NANOS * 30),
                        },
                    )
                    .unwrap_or_else(|e| fail("limits", format!("{e:?}")));
            }
            let first = &header.instruments[0];
            (
                limits,
                vec![StrategyConfig {
                    instrument: first.id,
                    symbol: first.symbol_str().unwrap_or("?").to_string(),
                    window: 20,
                    size: qty(1),
                    bars: BarSpec::Tick { threshold: 1 },
                }],
            )
        }
    };

    // The two bindings that make this a backtest.
    let mut feed = match HistoricalFeed::open(path, &instruments, Replaying::MarketDataOnly) {
        Ok(feed) => feed,
        Err(e) => fail(&format!("cannot replay {path}"), e),
    };
    let venue = SimVenue::new(
        instruments.len(),
        FillModel::TouchDisplayed,
        Fees {
            maker: Px::ZERO,
            taker: Px::from_scaled(SCALE / 100), // 0.01 per unit
        },
        ExchangeSpan::from_nanos(0),
    );

    let recorded_by_the_session = feed.recorded_decisions().to_vec();
    let recorded_decisions = recorded_by_the_session.len();
    let recorded_orders = recorded_by_the_session
        .iter()
        .filter(|o| matches!(o, Outbound::OrderSubmitted { .. }))
        .count();
    let market_events = feed.len();

    let config = EngineConfig::new(
        instruments.clone(),
        limits,
        OrderId::new(0),
        header.session_start,
    );
    let mut engine = Engine::new(config, venue, event::MemoryLog::with_capacity(1 << 16));
    for (index, spec) in strategies.iter().enumerate() {
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
            .unwrap_or_else(|e| fail("strategy", format!("{e:?}")));
    }

    if let Err(e) = run(&mut feed, &mut engine) {
        eprintln!("backtest: session stopped: {e:?}");
        std::process::exit(1);
    }

    let summary = match summarize(engine.log().records(), instruments.len()) {
        Ok(summary) => summary,
        Err(e) => fail("cannot summarize the run", format!("{e:?}")),
    };

    println!("backtest over {path}");
    println!("  session id         {}", header.session_id);
    for entry in &header.instruments {
        println!(
            "  instrument         {} (id {})",
            entry.symbol_str().unwrap_or("<not utf-8>"),
            entry.id.raw()
        );
    }
    println!("  market events      {market_events}");
    println!();
    println!("this run");
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
    println!("  final state        {:?}", summary.final_state);
    for entry in &header.instruments {
        let position = engine.positions().get(entry.id).expect("configured");
        println!(
            "  {:<14}     realized {}  position {}",
            entry.symbol_str().unwrap_or("?"),
            decimal(position.realized().to_scaled()),
            decimal(position.qty().to_scaled() as i128)
        );
    }

    // What the recording did, for comparison. The point of a backtest is the
    // difference between the two columns.
    println!();
    println!("the recorded session, for comparison");
    println!("  decisions          {recorded_decisions}");
    println!("  orders submitted   {recorded_orders}");
    // What the recorded session refused, and why. Without this a difference in
    // order counts is a mystery — and the usual cause is that the two runs are
    // under different limits, which is a fact about the comparison rather than
    // about the strategy.
    let mut recorded_refusals: Vec<(event::RiskReason, usize)> = Vec::new();
    for outbound in recorded_by_the_session.iter() {
        if let Outbound::IntentRejected { reason, .. } = outbound {
            match recorded_refusals.iter_mut().find(|(r, _)| r == reason) {
                Some(entry) => entry.1 += 1,
                None => recorded_refusals.push((*reason, 1)),
            }
        }
    }
    println!(
        "  intents refused    {}",
        recorded_refusals.iter().map(|(_, n)| n).sum::<usize>()
    );
    for (reason, count) in &recorded_refusals {
        println!("      {reason:?}: {count}");
    }
    if recorded_orders == summary.orders_submitted {
        println!("  -> same order count as this run");
    } else {
        println!(
            "  -> this run submitted {} order(s) {}",
            summary.orders_submitted.abs_diff(recorded_orders),
            if summary.orders_submitted > recorded_orders {
                "more"
            } else {
                "fewer"
            }
        );
    }
    println!();
    println!("note: the recorded session's operator commands are not replayed,");
    println!("      so a recording that was halted will not match order for order.");
}
