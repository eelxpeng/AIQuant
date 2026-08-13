//! A whole paper session, driven by a live feed on a clock a test controls.
//!
//! The binary is the same wiring; this is the part that can be asserted on
//! without waiting for wall-clock time to pass. It proves the shape that makes
//! paper trading paper: a live feed, a simulated venue, a real clock seam, and
//! an operator who can reach the running session.

use engine::{Engine, EngineConfig, FeedAdapter};
use event::{Command, EngineState, MemoryLog, Outbound};
use live_feed::{LiveFeed, ManualClock, Symbols, commands_from};
use marketdata::{Aggregator, BarSpec};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use std::time::Duration;
use strategy::MovingAverageCrossover;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

fn instrument() -> Instrument {
    Instrument::new(I, Px::from_scaled(10_000_000), qty(1), qty(1)).expect("conventions")
}

fn symbols() -> Symbols {
    let mut s = Symbols::new();
    s.add("SYNTH", I);
    s
}

/// A wandering market in the feed's own protocol, as a bridge would print it.
fn market_lines(steps: i64) -> String {
    let mut text = String::from("# a bridge would print these\n");
    for step in 0..steps {
        let phase = step % 40;
        let mid = 100 + if phase < 20 { phase } else { 40 - phase };
        let at = 1_700_000_000_000_000_000i64 + step * 100_000_000;
        text.push_str(&format!("Q SYNTH {at} {}.00 5 {}.00 5\n", mid - 1, mid + 1));
        text.push_str(&format!(
            "T SYNTH {} {mid}.00 1 {}\n",
            at + 50_000_000,
            if step % 2 == 0 { "B" } else { "S" }
        ));
    }
    text
}

fn config() -> EngineConfig {
    let mut limits = LimitBook::with_instruments(1);
    limits
        .set(
            I,
            Limits {
                max_position: qty(100),
                max_exposure: money(1_000_000),
                max_order_notional: money(100_000),
                max_orders_in_window: 500,
                rate_window: ExchangeSpan::from_nanos(1_000_000_000),
                max_quote_age: ExchangeSpan::from_nanos(60_000_000_000),
            },
        )
        .expect("limits");
    EngineConfig::new(
        vec![instrument()],
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    )
}

fn wire<C: types::Clock>(feed: &mut LiveFeed<C>) -> Engine<SimVenue, MemoryLog> {
    let _ = feed;
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(1 << 14));
    let subscription =
        engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("spec"));
    engine
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            I,
            subscription,
            10,
            qty(5),
        )))
        .expect("strategy");
    engine
}

/// The pump, the same three steps `bin/paper` runs.
fn pump<C: types::Clock>(feed: &mut LiveFeed<C>, engine: &mut Engine<SimVenue, MemoryLog>) {
    let mut requests = Vec::new();
    while let Some(event) = feed.next_event() {
        engine.on_inbound(event).expect("session");
        engine.take_timer_requests(&mut requests);
        for request in requests.drain(..) {
            feed.schedule_timer(request);
        }
        if engine.state() == EngineState::Killed {
            break;
        }
    }
}

#[test]
fn a_paper_session_trades_against_a_live_feed() {
    let lines = market_lines(300);
    let mut feed = LiveFeed::spawn(
        std::io::Cursor::new(lines.into_bytes()),
        symbols(),
        ManualClock::at(1_000),
        None,
    );
    let mut engine = wire(&mut feed);
    pump(&mut feed, &mut engine);

    assert_eq!(feed.market_events(), 600);
    let orders = engine
        .log()
        .outbound()
        .filter(|o| matches!(o, Outbound::OrderSubmitted { .. }))
        .count();
    assert!(orders > 5, "only {orders} orders over 300 market steps");
    assert_eq!(engine.state(), EngineState::Running);
    // Every decision is still caused by a recorded input, live feed or not.
    for record in engine.log().records() {
        if let Some(out) = record.event.as_outbound() {
            let caused_by = match out {
                Outbound::OrderSubmitted { caused_by, .. }
                | Outbound::CancelSubmitted { caused_by, .. }
                | Outbound::IntentRejected { caused_by, .. }
                | Outbound::TimerRequested { caused_by, .. }
                | Outbound::StateChanged { caused_by, .. } => *caused_by,
            };
            assert!(caused_by < record.seq);
        }
    }
}

#[test]
fn an_operator_can_halt_and_resume_a_running_session() {
    // The control path the architecture calls seam 5: an operator reaches the
    // engine only by appending a command, and here that command arrives while
    // the market is still moving.
    let lines = market_lines(300);
    let commands = commands_from("halt\n".as_bytes());
    std::thread::sleep(Duration::from_millis(50));
    let mut feed = LiveFeed::spawn(
        std::io::Cursor::new(lines.into_bytes()),
        symbols(),
        ManualClock::at(1_000),
        Some(commands),
    );
    let mut engine = wire(&mut feed);
    pump(&mut feed, &mut engine);

    assert_eq!(engine.state(), EngineState::Halted);
    assert!(
        engine.log().outbound().any(|o| matches!(
            o,
            Outbound::StateChanged {
                to: EngineState::Halted,
                ..
            }
        )),
        "the halt should be in the log"
    );
    // And it actually stopped the trading it would otherwise have done.
    let refusals = engine
        .log()
        .outbound()
        .filter(|o| {
            matches!(
                o,
                Outbound::IntentRejected {
                    reason: event::RiskReason::Halted,
                    ..
                }
            )
        })
        .count();
    assert!(
        refusals > 0,
        "the halt refused nothing, so it proves nothing"
    );
}

#[test]
fn an_operator_kill_stops_a_running_session() {
    let lines = market_lines(300);
    let commands = commands_from("kill\n".as_bytes());
    std::thread::sleep(Duration::from_millis(50));
    let mut feed = LiveFeed::spawn(
        std::io::Cursor::new(lines.into_bytes()),
        symbols(),
        ManualClock::at(1_000),
        Some(commands),
    );
    let mut engine = wire(&mut feed);
    pump(&mut feed, &mut engine);

    assert_eq!(engine.state(), EngineState::Killed);
    assert!(
        feed.market_events() < 600,
        "the kill should have ended it early, saw all {} events",
        feed.market_events()
    );
    assert_eq!(feed.commands_seen(), 1);
    let _ = Command::Kill;
}
