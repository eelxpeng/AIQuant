//! End-to-end sessions through the whole loop.
//!
//! Everything here runs offline against a scripted feed and a simulated venue.
//! No socket, no credential, no wall clock (Constitution VIII).

use engine::{Engine, EngineConfig, EngineError, FeedAdapter, run};
use event::{
    Command, EngineState, Inbound, MemoryLog, OrderKind, Outbound, RiskReason, StateReason,
};
use marketdata::{Aggregator, BarSpec, BarSubscription};
use oms::OrderState;
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use simkit::{ReplayVenue, Script, qty};
use strategy::{Context, MovingAverageCrossover, Strategy, StrategyEvent};
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Side, StrategyId, Timestamp,
};

const SCALE: i64 = types::SCALE;
const I: InstrumentId = InstrumentId::new(0);
const SUB: BarSubscription = BarSubscription::from_index(0);

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

fn instrument() -> Instrument {
    // Tick 0.01, lot 1, minimum 1.
    Instrument::new(I, Px::from_scaled(10_000_000), qty(1), qty(1)).expect("conventions")
}

fn limits() -> Limits {
    Limits {
        max_position: qty(100),
        max_exposure: money(1_000_000),
        max_order_notional: money(100_000),
        max_orders_in_window: 100,
        rate_window: ExchangeSpan::from_nanos(1_000_000_000),
        max_quote_age: ExchangeSpan::from_nanos(1_000_000_000),
    }
}

fn config() -> EngineConfig {
    let mut book = LimitBook::with_instruments(1);
    book.set(I, limits()).expect("limits");
    EngineConfig::new(
        vec![instrument()],
        book,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    )
}

fn sim() -> SimVenue {
    SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    )
}

/// Buys once, the first time it sees a quote. Keeps the engine tests about the
/// engine rather than about a signal.
#[derive(Debug)]
struct BuyOnce {
    done: bool,
    size: i64,
    kind: OrderKind,
}

impl BuyOnce {
    /// Takes whatever the touch offers.
    fn market(size: i64) -> Box<BuyOnce> {
        Box::new(BuyOnce {
            done: false,
            size,
            kind: OrderKind::Market,
        })
    }

    /// Rests, if the price is not marketable.
    fn limit(size: i64, price: Px) -> Box<BuyOnce> {
        Box::new(BuyOnce {
            done: false,
            size,
            kind: OrderKind::Limit(price),
        })
    }
}

impl Strategy for BuyOnce {
    fn id(&self) -> StrategyId {
        StrategyId::new(0)
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
        if let StrategyEvent::Quote { instrument, .. } = event
            && !self.done
        {
            self.done = true;
            ctx.order(*instrument, Side::Buy, qty(self.size), self.kind);
        }
    }
}

fn crossover() -> Box<MovingAverageCrossover> {
    Box::new(MovingAverageCrossover::new(
        StrategyId::new(0),
        I,
        SUB,
        3,
        qty(10),
    ))
}

/// A script with enough trades to warm a three-bar average and reverse it.
fn signal_script() -> Script {
    Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .trade(2_000, 100, 1, Side::Buy)
        .trade(3_000, 100, 1, Side::Buy)
        .trade(4_000, 130, 1, Side::Buy)
        .quote(5_000, 99, 100, 101, 100)
        .trade(6_000, 1, 1, Side::Sell)
}

fn decisions(log: &MemoryLog) -> Vec<Outbound> {
    log.outbound().copied().collect()
}

fn inputs(log: &MemoryLog) -> Vec<Inbound> {
    log.inbound().copied().collect()
}

/// Runs the signal script through a live-shaped session.
fn run_signal_session() -> Engine<SimVenue, MemoryLog> {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(256));
    let _ =
        engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("spec"));
    engine.add_strategy(crossover()).expect("strategy");
    let mut feed = signal_script().build();
    run(&mut feed, &mut engine).expect("session");
    engine
}

#[test]
fn a_session_turns_a_signal_into_orders_and_fills() {
    let engine = run_signal_session();
    let log = engine.log();

    let submitted: Vec<&Outbound> = log
        .outbound()
        .filter(|o| matches!(o, Outbound::OrderSubmitted { .. }))
        .collect();
    assert!(
        !submitted.is_empty(),
        "the signal should have produced at least one order"
    );

    // The first order is the long the crossover opens once it is warm.
    match submitted[0] {
        Outbound::OrderSubmitted { side, qty: q, .. } => {
            assert_eq!(*side, Side::Buy);
            assert_eq!(*q, qty(10));
        }
        other => panic!("expected an order, got {other:?}"),
    }

    // It filled against the simulated book, so the books are no longer flat.
    let position = engine.positions().get(I).expect("position");
    assert!(!position.is_flat() || position.realized() != Notional::ZERO);
}

#[test]
fn replaying_the_recorded_inputs_reproduces_every_decision() {
    // Constitution II. The same input sequence must produce the same orders,
    // in the same order, with the same reasons.
    let live = run_signal_session();
    let recorded_inputs = inputs(live.log());
    let recorded_decisions = decisions(live.log());
    assert!(
        !recorded_decisions.is_empty(),
        "a session with no decisions would prove nothing"
    );

    // Replay binds a venue that says nothing, because the venue's reports are
    // already among the recorded inputs.
    let mut replayed = Engine::new(config(), ReplayVenue::new(), MemoryLog::with_capacity(256));
    let _ =
        replayed.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("spec"));
    replayed.add_strategy(crossover()).expect("strategy");
    for input in &recorded_inputs {
        replayed.on_inbound(*input).expect("replay");
    }

    assert_eq!(decisions(replayed.log()), recorded_decisions);
    // Byte-identical means the whole log, not just the decisions: the sequence
    // numbers and the interleaving have to match too.
    assert_eq!(replayed.log().records(), live.log().records());
}

#[test]
fn two_identical_runs_produce_identical_logs() {
    let first = run_signal_session();
    let second = run_signal_session();
    assert_eq!(first.log().records(), second.log().records());
    assert_eq!(
        first.positions().get(I).expect("position").qty(),
        second.positions().get(I).expect("position").qty()
    );
}

#[test]
fn a_halt_refuses_new_risk_and_records_the_reason() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    engine.add_strategy(BuyOnce::market(5)).expect("strategy");

    let mut feed = Script::new(I)
        .command(500, Command::Halt)
        .quote(1_000, 99, 100, 101, 100)
        .build();
    run(&mut feed, &mut engine).expect("session");

    assert_eq!(engine.state(), EngineState::Halted);
    let rejected: Vec<RiskReason> = engine
        .log()
        .outbound()
        .filter_map(|o| match o {
            Outbound::IntentRejected { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, vec![RiskReason::Halted]);
    assert!(engine.orders().is_empty(), "no order should exist");
}

#[test]
fn a_kill_cancels_resting_orders_and_refuses_everything_after() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    // A limit below the offer rests rather than filling. A market order would
    // never rest, so there would be nothing for the kill to cancel.
    engine
        .add_strategy(BuyOnce::limit(5, px(99)))
        .expect("strategy");

    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .command(2_000, Command::Kill)
        .build();
    run(&mut feed, &mut engine).expect("session");

    assert_eq!(engine.state(), EngineState::Killed);
    let cancels = engine
        .log()
        .outbound()
        .filter(|o| matches!(o, Outbound::CancelSubmitted { .. }))
        .count();
    assert_eq!(cancels, 1, "the resting order should have been cancelled");

    // Anything after a kill is refused, including a reduce-only order.
    let mut after = Script::new(I).quote(3_000, 99, 100, 101, 100).build();
    while let Some(event) = after.next_event() {
        engine.on_inbound(event).expect("post-kill");
    }
    assert_eq!(engine.state(), EngineState::Killed);
}

#[test]
fn a_flatten_brings_the_position_back_to_zero() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(128));
    engine.add_strategy(BuyOnce::market(7)).expect("strategy");

    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .command(2_000, Command::Flatten)
        .build();
    run(&mut feed, &mut engine).expect("session");

    let flatten_orders: Vec<&Outbound> = engine
        .log()
        .outbound()
        .filter(|o| {
            matches!(
                o,
                Outbound::OrderSubmitted {
                    reduce_only: true,
                    ..
                }
            )
        })
        .collect();
    assert_eq!(flatten_orders.len(), 1);
    match flatten_orders[0] {
        Outbound::OrderSubmitted {
            side,
            qty: q,
            strategy,
            ..
        } => {
            assert_eq!(*side, Side::Sell);
            assert_eq!(*q, qty(7));
            // Nobody's strategy asked for it, so it is attributed to the
            // operator rather than left ambiguous.
            assert_eq!(*strategy, Engine::<SimVenue, MemoryLog>::OPERATOR);
        }
        other => panic!("expected a reduce-only order, got {other:?}"),
    }
    assert!(engine.positions().get(I).expect("position").is_flat());
}

#[test]
fn a_flatten_against_a_flat_book_orders_nothing() {
    // The degenerate case: there is nothing to reduce, and "sell" would open a
    // short rather than close anything.
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .command(2_000, Command::Flatten)
        .build();
    run(&mut feed, &mut engine).expect("session");

    assert_eq!(
        engine
            .log()
            .outbound()
            .filter(|o| matches!(o, Outbound::OrderSubmitted { .. }))
            .count(),
        0
    );
}

#[test]
fn a_venue_position_that_disagrees_halts_trading() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .venue_position(2_000, 42)
        .build();
    run(&mut feed, &mut engine).expect("session");

    assert_eq!(engine.state(), EngineState::Halted);
    let halted_for = engine.log().outbound().find_map(|o| match o {
        Outbound::StateChanged { reason, to, .. } if *to == EngineState::Halted => Some(*reason),
        _ => None,
    });
    assert_eq!(halted_for, Some(StateReason::ReconciliationDivergence));
}

#[test]
fn resume_is_refused_while_the_books_still_disagree() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .venue_position(2_000, 42)
        .command(3_000, Command::Resume)
        .build();
    run(&mut feed, &mut engine).expect("session");

    assert_eq!(engine.state(), EngineState::Halted);
    // The refusal is in the log as a transition to the state it stayed in,
    // rather than being an absence of a record.
    let refusals = engine
        .log()
        .outbound()
        .filter(|o| {
            matches!(
                o,
                Outbound::StateChanged {
                    from: EngineState::Halted,
                    to: EngineState::Halted,
                    reason: StateReason::ReconciliationDivergence,
                    ..
                }
            )
        })
        .count();
    assert_eq!(refusals, 1);
}

#[test]
fn resume_after_the_books_agree_returns_to_running() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .venue_position(2_000, 42)
        .venue_position(3_000, 0)
        .command(4_000, Command::Resume)
        .build();
    run(&mut feed, &mut engine).expect("session");
    assert_eq!(engine.state(), EngineState::Running);
}

#[test]
fn a_venue_that_cannot_be_reached_produces_no_order_and_says_why() {
    let mut venue = sim();
    venue.disconnect();
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(64));
    engine.add_strategy(BuyOnce::market(5)).expect("strategy");

    let mut feed = Script::new(I).quote(1_000, 99, 100, 101, 100).build();
    run(&mut feed, &mut engine).expect("session");

    assert!(
        engine.orders().is_empty(),
        "an order that never left must not exist"
    );
    let rejected: Vec<RiskReason> = engine
        .log()
        .outbound()
        .filter_map(|o| match o {
            Outbound::IntentRejected { reason, .. } => Some(*reason),
            _ => None,
        })
        .collect();
    assert_eq!(rejected, vec![RiskReason::VenueUnreachable]);
}

#[test]
fn an_order_larger_than_the_touch_fills_what_is_there_and_cancels_the_rest() {
    // The TouchDisplayed model: five available against an order for twenty.
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    engine.add_strategy(BuyOnce::market(20)).expect("strategy");

    let mut feed = Script::new(I).quote(1_000, 99, 100, 101, 5).build();
    run(&mut feed, &mut engine).expect("session");

    let order = engine.orders().iter().next().expect("one order");
    assert_eq!(order.filled(), qty(5));
    assert_eq!(order.state(), OrderState::Cancelled);
    assert_eq!(engine.positions().get(I).expect("position").qty(), qty(5));
}

/// Counts what it is shown, so a test can assert what never arrived.
#[derive(Debug, Default)]
struct Counting {
    quotes: usize,
    trades: usize,
    bars: usize,
}

impl Strategy for Counting {
    fn id(&self) -> StrategyId {
        StrategyId::new(0)
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, _ctx: &mut Context<'_>) {
        match event {
            StrategyEvent::Quote { .. } => self.quotes += 1,
            StrategyEvent::Trade { .. } => self.trades += 1,
            StrategyEvent::Bar { .. } => self.bars += 1,
            _ => {}
        }
    }
}

#[test]
fn a_market_event_that_arrives_late_reaches_nothing_downstream() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    let _ =
        engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("spec"));
    engine
        .add_strategy(Box::new(Counting::default()))
        .expect("strategy");

    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .trade(1_100, 100, 1, Side::Buy)
        // Both of these are stamped before what the book already holds.
        .quote(500, 1, 100, 2, 100)
        .trade(600, 55, 1, Side::Buy)
        .build();
    run(&mut feed, &mut engine).expect("session");

    // The book kept the newer prices and counted the two refusals.
    let top = engine.books().top(I).expect("quote");
    assert_eq!(top.bid_px, px(99));
    assert_eq!(top.ask_px, px(101));
    assert_eq!(engine.books().total_out_of_order(), 2);
    assert_eq!(engine.books().last_trade(I).expect("trade").px, px(100));
}

#[test]
fn a_late_market_event_is_still_recorded_as_an_input() {
    // It is refused, not erased. Replay must see the same inputs this run saw,
    // including the ones it decided to ignore.
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(64));
    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .quote(500, 1, 100, 2, 100)
        .build();
    run(&mut feed, &mut engine).expect("session");

    assert_eq!(engine.log().inbound().count(), 2);
    assert_eq!(engine.books().total_out_of_order(), 1);
}

#[test]
fn refusing_a_late_event_is_reproduced_on_replay() {
    let mut live = Engine::new(config(), sim(), MemoryLog::with_capacity(128));
    let _ = live.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("spec"));
    live.add_strategy(crossover()).expect("strategy");
    let mut feed = Script::new(I)
        .quote(1_000, 99, 100, 101, 100)
        .trade(2_000, 100, 1, Side::Buy)
        .trade(1_500, 999, 1, Side::Buy)
        .trade(3_000, 100, 1, Side::Buy)
        .trade(4_000, 130, 1, Side::Buy)
        .build();
    run(&mut feed, &mut live).expect("session");
    assert_eq!(live.books().total_out_of_order(), 1);

    // The refusal is a function of records already in the log, which is why it
    // needs no record of its own: replay reaches the same decision unaided.
    let mut replayed = Engine::new(config(), ReplayVenue::new(), MemoryLog::with_capacity(128));
    let _ =
        replayed.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("spec"));
    replayed.add_strategy(crossover()).expect("strategy");
    for input in inputs(live.log()) {
        replayed.on_inbound(input).expect("replay");
    }

    assert_eq!(replayed.log().records(), live.log().records());
    assert_eq!(replayed.books().total_out_of_order(), 1);
}

#[test]
fn an_event_for_an_unconfigured_instrument_is_refused() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(16));
    let stray = Script::new(InstrumentId::new(9))
        .quote(1_000, 1, 1, 2, 1)
        .events()[0];
    assert_eq!(
        engine.on_inbound(stray),
        Err(EngineError::UnknownInstrument)
    );
}

#[test]
fn every_decision_names_the_record_that_caused_it() {
    let engine = run_signal_session();
    let records = engine.log().records();
    for record in records {
        let Some(out) = record.event.as_outbound() else {
            continue;
        };
        let caused_by = match out {
            Outbound::OrderSubmitted { caused_by, .. }
            | Outbound::CancelSubmitted { caused_by, .. }
            | Outbound::IntentRejected { caused_by, .. }
            | Outbound::TimerRequested { caused_by, .. }
            | Outbound::StateChanged { caused_by, .. } => *caused_by,
        };
        assert!(
            caused_by < record.seq,
            "{out:?} at {} claims to be caused by {caused_by}, which is not earlier",
            record.seq
        );
        assert!(
            records[caused_by.raw() as usize]
                .event
                .as_inbound()
                .is_some(),
            "a decision must be caused by an input, not by another decision"
        );
    }
}

#[test]
fn a_session_that_never_exceeds_its_reservations_never_needs_to_grow() {
    let mut engine = Engine::new(config(), sim(), MemoryLog::with_capacity(1_024));
    let _ =
        engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("spec"));
    engine.add_strategy(crossover()).expect("strategy");
    let mut feed = signal_script().build();
    run(&mut feed, &mut engine).expect("session");

    assert!(!engine.would_allocate());
    assert!(!engine.log().would_grow());
}
