//! D2.3: the kill switch works from every state.
//!
//! Constitution V does not accept inspection here — "it is proven by a test per
//! state". So this file enumerates the states a session can be in when the kill
//! arrives, and each one gets its own test, named after the state, so a failure
//! says which state broke.
//!
//! # What "the kill worked" means
//!
//! Every test ends in the same four assertions, applied by [`kill_and_prove`]:
//!
//! 1. The engine is `Killed` and recorded the transition.
//! 2. No order is created afterwards, however hard the session is pushed.
//! 3. Not even a **flatten** gets through. Reduce-only bypasses every check
//!    about how much risk is being taken, but not this one: the kill switch
//!    means *this system is wrong, stop it*, and if the system's own accounting
//!    is what is wrong, letting it emit more orders makes the incident worse
//!    (ADR #1, D-4).
//! 4. Something actually **tried** to trade and was refused with
//!    `KillSwitchEngaged`. Without this the other three could pass vacuously,
//!    because nothing asked.
//!
//! # States that do not exist in this design
//!
//! Constitution V names three situations by hand. Two are covered below.
//! The third is not, and the reason is that the state does not exist:
//!
//! - **Mid-order** — covered: pending, working, partially filled, cancel in
//!   flight, and already terminal each get a test.
//! - **During reconnect** — covered by `while_the_venue_is_unreachable`.
//! - **While a limit config is reloading** — there is no reload path. A
//!   `LimitBook` is moved into the gate at construction and never replaced, so
//!   there is no window to test. If configuration reloading is ever added, its
//!   case belongs in this file and this note should be deleted.
//!
//! A kill arriving **mid-cascade** is likewise not testable, because it cannot
//! happen: the loop takes one input at a time and runs it to completion,
//! including everything the venue says in response. A command is an input, so it
//! is never interleaved with the handling of another one.

use engine::{Engine, EngineConfig};
use event::{
    Command, CommandEvent, EngineState, Inbound, MarketEvent, MarketKind, MemoryLog, OrderKind,
    Outbound, RiskReason, StateReason,
};
use oms::{Order, OrderState, VenueAdapter, VenueError};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, Queue, SimVenue};
use strategy::{Context, Strategy, StrategyEvent};
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// A venue that takes everything and never answers.
///
/// It exists so a test can hold an order in a state a real venue would move it
/// out of within microseconds: `Pending` needs an acknowledgement that never
/// comes, and `PendingCancel` needs a cancel confirmation that never comes.
#[derive(Debug, Default)]
struct SilentVenue {
    submitted: Vec<OrderId>,
    cancelled: Vec<OrderId>,
}

impl VenueAdapter for SilentVenue {
    fn submit(&mut self, order: &Order) -> Result<(), VenueError> {
        self.submitted.push(order.id());
        Ok(())
    }

    fn cancel(&mut self, id: OrderId) -> Result<(), VenueError> {
        self.cancelled.push(id);
        Ok(())
    }

    fn drain(&mut self, _out: &mut Vec<Inbound>) {}
}

/// Acknowledges orders, then refuses to be reached.
///
/// Models the link dropping while an order is live at the venue: the submit and
/// its acknowledgement got through, and the cancel that the kill switch tries to
/// send does not.
#[derive(Debug, Default)]
struct LinkDrops {
    pending: Vec<Inbound>,
    cancel_attempts: usize,
}

impl VenueAdapter for LinkDrops {
    fn submit(&mut self, order: &Order) -> Result<(), VenueError> {
        self.pending.push(Inbound::Venue(event::VenueEvent {
            order: order.id(),
            venue_time: order.submitted_at(),
            receive_time: Timestamp::from_nanos(order.submitted_at().to_nanos()),
            kind: event::VenueKind::Accepted,
        }));
        Ok(())
    }

    fn cancel(&mut self, _id: OrderId) -> Result<(), VenueError> {
        self.cancel_attempts += 1;
        Err(VenueError::Disconnected)
    }

    fn drain(&mut self, out: &mut Vec<Inbound>) {
        out.append(&mut self.pending);
    }
}

/// Wants to buy on every quote, so a session is never quiet by accident.
///
/// The fourth assertion in `kill_and_prove` depends on something trying to
/// trade after the kill. A strategy that gave up would make every test here
/// pass for the wrong reason.
#[derive(Debug)]
struct AlwaysWants {
    kind: OrderKind,
    size: i64,
}

impl Strategy for AlwaysWants {
    fn id(&self) -> StrategyId {
        StrategyId::new(0)
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
        if let StrategyEvent::Quote { instrument, .. } = event {
            ctx.order(*instrument, Side::Buy, qty(self.size), self.kind);
        }
    }
}

fn config() -> EngineConfig {
    let instrument =
        Instrument::new(I, Px::from_scaled(10_000_000), qty(1), qty(1)).expect("conventions");
    let mut limits = LimitBook::with_instruments(1);
    limits
        .set(
            I,
            Limits {
                max_position: qty(1_000),
                max_exposure: money(10_000_000),
                max_order_notional: money(1_000_000),
                max_orders_in_window: 1_000,
                rate_window: ExchangeSpan::from_nanos(1_000_000),
                max_quote_age: ExchangeSpan::from_nanos(10_000_000),
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

/// A monotonic clock for a test, so the book never refuses an event for
/// arriving late.
struct Clock(i64);

impl Clock {
    fn tick(&mut self) -> i64 {
        self.0 += 1_000;
        self.0
    }
}

fn quote(at: i64, bid: i64, bid_size: i64, ask: i64, ask_size: i64) -> Inbound {
    Inbound::Market(MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::Quote {
            bid_px: px(bid),
            bid_qty: qty(bid_size),
            ask_px: px(ask),
            ask_qty: qty(ask_size),
        },
    })
}

fn trade(at: i64, price: i64, size: i64) -> Inbound {
    Inbound::Market(MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::Trade {
            px: px(price),
            qty: qty(size),
            aggressor: Side::Sell,
        },
    })
}

fn command(at: i64, command: Command) -> Inbound {
    Inbound::Command(CommandEvent {
        command,
        receive_time: Timestamp::from_nanos(at),
    })
}

fn submitted(log: &MemoryLog) -> usize {
    log.outbound()
        .filter(|o| matches!(o, Outbound::OrderSubmitted { .. }))
        .count()
}

fn cancels(log: &MemoryLog) -> usize {
    log.outbound()
        .filter(|o| matches!(o, Outbound::CancelSubmitted { .. }))
        .count()
}

fn kill_refusals(log: &MemoryLog) -> usize {
    log.outbound()
        .filter(|o| {
            matches!(
                o,
                Outbound::IntentRejected {
                    reason: RiskReason::KillSwitchEngaged,
                    ..
                }
            )
        })
        .count()
}

/// Kills the engine and proves the kill took effect, whatever state it was in.
///
/// Takes the state's name so a failure reads as "the kill did not hold while an
/// order was working" rather than as an assertion number.
fn kill_and_prove<V: VenueAdapter>(
    engine: &mut Engine<V, MemoryLog>,
    clock: &mut Clock,
    state: &str,
) {
    let orders_before = engine.orders().len();
    let submitted_before = submitted(engine.log());
    let already_killed = engine.state() == EngineState::Killed;

    let at = clock.tick();
    engine
        .on_inbound(command(at, Command::Kill))
        .unwrap_or_else(|e| panic!("{state}: the kill itself errored: {e:?}"));

    assert_eq!(
        engine.state(),
        EngineState::Killed,
        "{state}: the kill did not take effect"
    );
    if !already_killed {
        let recorded = engine.log().outbound().any(|o| {
            matches!(
                o,
                Outbound::StateChanged {
                    to: EngineState::Killed,
                    reason: StateReason::OperatorCommand,
                    ..
                }
            )
        });
        assert!(recorded, "{state}: the kill was not recorded");
    }

    // Now push as hard as the interface allows: a quote the strategy will want
    // to trade on, and an explicit operator flatten.
    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .unwrap_or_else(|e| panic!("{state}: post-kill quote errored: {e:?}"));
    let at = clock.tick();
    engine
        .on_inbound(command(at, Command::Flatten))
        .unwrap_or_else(|e| panic!("{state}: post-kill flatten errored: {e:?}"));

    assert_eq!(
        submitted(engine.log()),
        submitted_before,
        "{state}: an order reached the venue after the kill"
    );
    assert_eq!(
        engine.orders().len(),
        orders_before,
        "{state}: an order was created after the kill"
    );
    assert!(
        kill_refusals(engine.log()) > 0,
        "{state}: nothing even tried to trade after the kill, so this proves nothing"
    );
}

/// Every order the session created is either finished or has a cancel out for
/// it.
///
/// Separate from [`kill_and_prove`] on purpose. Whether resting orders can be
/// cancelled depends on the venue being reachable, whereas the kill landing
/// does not — conflating them would let an unreachable venue look like a kill
/// that failed.
fn assert_nothing_still_live<V: VenueAdapter>(engine: &Engine<V, MemoryLog>, state: &str) {
    for order in engine.orders().iter() {
        assert!(
            order.state().is_terminal() || order.state() == OrderState::PendingCancel,
            "{state}: order {} is still live at {:?} after a kill",
            order.id(),
            order.state()
        );
    }
}

// ---- Engine states -------------------------------------------------------

#[test]
fn from_running_with_nothing_in_flight() {
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(
        config(),
        SilentVenue::default(),
        MemoryLog::with_capacity(64),
    );
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 1,
        }))
        .expect("strategy");
    kill_and_prove(&mut engine, &mut clock, "running, nothing in flight");
}

#[test]
fn from_halted() {
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(
        config(),
        SilentVenue::default(),
        MemoryLog::with_capacity(64),
    );
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 1,
        }))
        .expect("strategy");
    let at = clock.tick();
    engine.on_inbound(command(at, Command::Halt)).expect("halt");
    assert_eq!(engine.state(), EngineState::Halted);
    kill_and_prove(&mut engine, &mut clock, "halted");
}

#[test]
fn from_already_killed() {
    // Killing twice must be safe. An operator who is not sure the first one
    // landed will press it again, and that is the correct instinct.
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(
        config(),
        SilentVenue::default(),
        MemoryLog::with_capacity(64),
    );
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 1,
        }))
        .expect("strategy");
    let at = clock.tick();
    engine.on_inbound(command(at, Command::Kill)).expect("kill");
    let transitions = engine
        .log()
        .outbound()
        .filter(|o| {
            matches!(
                o,
                Outbound::StateChanged {
                    to: EngineState::Killed,
                    ..
                }
            )
        })
        .count();

    kill_and_prove(&mut engine, &mut clock, "already killed");

    // The second kill changes nothing, so it records no second transition.
    assert_eq!(
        engine
            .log()
            .outbound()
            .filter(|o| matches!(
                o,
                Outbound::StateChanged {
                    to: EngineState::Killed,
                    ..
                }
            ))
            .count(),
        transitions
    );
}

// ---- Mid-order states ----------------------------------------------------

#[test]
fn while_an_order_is_pending_acknowledgement() {
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(
        config(),
        SilentVenue::default(),
        MemoryLog::with_capacity(64),
    );
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    let order = engine.orders().iter().next().expect("an order").id();
    assert_eq!(
        engine.orders().get(order).expect("order").state(),
        OrderState::Pending,
        "the silent venue should have left it unacknowledged"
    );

    kill_and_prove(&mut engine, &mut clock, "order pending acknowledgement");
    assert_nothing_still_live(&engine, "order pending acknowledgement");
    assert!(
        cancels(engine.log()) > 0,
        "the pending order was not cancelled"
    );
}

#[test]
fn while_an_order_is_working_at_the_venue() {
    let mut clock = Clock(1_000_000);
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(128));
    // A limit below the offer rests instead of filling.
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Limit(px(99)),
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    let order = engine.orders().iter().next().expect("an order").id();
    assert_eq!(
        engine.orders().get(order).expect("order").state(),
        OrderState::Working
    );

    kill_and_prove(&mut engine, &mut clock, "order working at the venue");
    assert_nothing_still_live(&engine, "order working at the venue");
    assert!(
        cancels(engine.log()) > 0,
        "the working order was not cancelled"
    );
    assert!(
        engine
            .orders()
            .get(order)
            .expect("order")
            .state()
            .is_terminal(),
        "the venue confirmed the cancel, so the order should be terminal"
    );
}

#[test]
fn while_an_order_is_partly_filled_and_still_working() {
    let mut clock = Clock(1_000_000);
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(128));
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Limit(px(99)),
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    let order = engine.orders().iter().next().expect("an order").id();
    // A print at the resting price, smaller than the order.
    let at = clock.tick();
    engine.on_inbound(trade(at, 99, 2)).expect("partial fill");

    let partly = engine.orders().get(order).expect("order");
    assert_eq!(partly.filled(), qty(2));
    assert_eq!(partly.state(), OrderState::Working);

    kill_and_prove(&mut engine, &mut clock, "order partly filled and working");
    assert_nothing_still_live(&engine, "order partly filled and working");
    // What was filled stays filled. A kill stops trading; it does not unwind.
    assert_eq!(engine.orders().get(order).expect("order").filled(), qty(2));
    assert_eq!(engine.positions().get(I).expect("position").qty(), qty(2));
}

#[test]
fn while_a_cancel_is_already_in_flight() {
    // `PendingCancel` is only reachable through a prior kill in the v1 command
    // surface — there is no operator cancel. So this is the honest shape of the
    // state: kill, the cancel goes unanswered, kill again.
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(
        config(),
        SilentVenue::default(),
        MemoryLog::with_capacity(64),
    );
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    let order = engine.orders().iter().next().expect("an order").id();
    let at = clock.tick();
    engine
        .on_inbound(command(at, Command::Kill))
        .expect("first kill");
    assert_eq!(
        engine.orders().get(order).expect("order").state(),
        OrderState::PendingCancel,
        "the silent venue should have left the cancel unanswered"
    );

    kill_and_prove(&mut engine, &mut clock, "cancel already in flight");
    assert_nothing_still_live(&engine, "cancel already in flight");
}

#[test]
fn while_every_order_is_already_terminal() {
    let mut clock = Clock(1_000_000);
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(128));
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    let order = engine.orders().iter().next().expect("an order").id();
    assert_eq!(
        engine.orders().get(order).expect("order").state(),
        OrderState::Filled
    );
    let cancels_before = cancels(engine.log());

    kill_and_prove(&mut engine, &mut clock, "every order already terminal");
    // Nothing was live, so nothing was cancelled — and no terminal order was
    // dragged back out of its terminal state to be cancelled.
    assert_eq!(cancels(engine.log()), cancels_before);
}

// ---- Transport and reconciliation states ---------------------------------

#[test]
fn while_the_venue_is_unreachable() {
    // Constitution V's "during reconnect": the order is live at the venue and
    // the link is down when the kill arrives. The kill must still take effect —
    // one that depended on the venue being up would be useless in the incident
    // it exists for.
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(config(), LinkDrops::default(), MemoryLog::with_capacity(64));
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Limit(px(99)),
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    let order = engine.orders().iter().next().expect("an order").id();
    assert_eq!(
        engine.orders().get(order).expect("order").state(),
        OrderState::Working,
        "the order should be live at the venue before the link drops"
    );

    kill_and_prove(&mut engine, &mut clock, "venue unreachable");

    // What actually happens, stated rather than asserted away: the cancel could
    // not be sent, so the order is still working at the venue, and nothing in
    // the log says so.
    //
    // The kill did its primary job — no new order can be created. But an
    // operator reading this log cannot tell that a live order was left behind.
    // Recording a failed cancel needs a new outbound record, which is an
    // alphabet change and does not belong in a test PR (#1).
    assert_eq!(
        engine.orders().get(order).expect("order").state(),
        OrderState::Working
    );
    assert_eq!(
        cancels(engine.log()),
        0,
        "a cancel that never left must not be recorded as sent"
    );
}

#[test]
fn while_the_venue_never_accepted_anything() {
    // The other half of a broken link: it was already down when the strategy
    // tried to trade, so no order exists at all.
    let mut clock = Clock(1_000_000);
    let mut venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    venue.disconnect();
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(64));
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    assert!(engine.orders().is_empty());

    kill_and_prove(&mut engine, &mut clock, "venue never reachable");
    assert_nothing_still_live(&engine, "venue never reachable");
}

#[test]
fn while_the_position_disagrees_with_the_venue() {
    let mut clock = Clock(1_000_000);
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(128));
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    let at = clock.tick();
    engine
        .on_inbound(Inbound::VenuePosition(event::PositionReport {
            instrument: I,
            venue_qty: qty(9_999),
            venue_time: Timestamp::from_nanos(at),
            receive_time: Timestamp::from_nanos(at),
        }))
        .expect("divergence");
    assert_eq!(engine.state(), EngineState::Halted);

    kill_and_prove(&mut engine, &mut clock, "position diverged from the venue");
}

// ---- Position states -----------------------------------------------------

#[test]
fn while_holding_a_long_position() {
    let mut clock = Clock(1_000_000);
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(128));
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 5,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    assert_eq!(engine.positions().get(I).expect("position").qty(), qty(5));

    kill_and_prove(&mut engine, &mut clock, "holding a long position");
    // The position is still there. A kill stops the system; it does not flatten
    // — and the flatten this test sent afterwards was refused, which is the
    // whole point of the fourth assertion.
    assert_eq!(engine.positions().get(I).expect("position").qty(), qty(5));
}

#[test]
fn while_holding_a_short_position() {
    let mut clock = Clock(1_000_000);
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(128));

    /// Sells on every quote, so the session builds a short.
    #[derive(Debug)]
    struct Seller;
    impl Strategy for Seller {
        fn id(&self) -> StrategyId {
            StrategyId::new(0)
        }
        fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
            if let StrategyEvent::Quote { instrument, .. } = event {
                ctx.order(*instrument, Side::Sell, qty(4), OrderKind::Market);
            }
        }
    }
    engine.add_strategy(Box::new(Seller)).expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    assert_eq!(engine.positions().get(I).expect("position").qty(), qty(-4));

    kill_and_prove(&mut engine, &mut clock, "holding a short position");
    assert_eq!(engine.positions().get(I).expect("position").qty(), qty(-4));
}

// ---- Market-data states --------------------------------------------------

#[test]
fn before_any_market_data_has_arrived() {
    // The degenerate case: the session has never seen a quote, so there is no
    // book, no mark, and no position. The kill still has to land.
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(
        config(),
        SilentVenue::default(),
        MemoryLog::with_capacity(64),
    );
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 1,
        }))
        .expect("strategy");
    assert!(engine.books().top(I).is_none());

    kill_and_prove(&mut engine, &mut clock, "no market data yet");
}

#[test]
fn while_the_market_data_is_stale() {
    let mut clock = Clock(1_000_000);
    let mut engine = Engine::new(
        config(),
        SilentVenue::default(),
        MemoryLog::with_capacity(64),
    );
    engine
        .add_strategy(Box::new(AlwaysWants {
            kind: OrderKind::Market,
            size: 1,
        }))
        .expect("strategy");

    let at = clock.tick();
    engine
        .on_inbound(quote(at, 99, 500, 101, 500))
        .expect("quote");
    // Jump far past the staleness bound without a new quote.
    clock.0 += 1_000_000_000;

    kill_and_prove(&mut engine, &mut clock, "market data stale");
}
