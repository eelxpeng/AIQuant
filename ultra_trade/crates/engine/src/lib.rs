//! The loop that wires market data, strategies, risk, and orders together.
//!
//! This is a **library**. It owns no `main`: a thin binary under `bin/` binds a
//! feed, a venue, and a clock and calls it. That is what makes backtest/live
//! parity structural rather than a rule people have to remember — backtest and
//! live are two *callers* of one library, not two modes of one program, so
//! there is no place for an `if backtest` to live (Constitution III).
//!
//! # The shape of one step
//!
//! Every inbound event goes through the same path:
//!
//! 1. **Record it.** The input is appended to the log before anything reads it.
//!    A failed append halts the engine, because a decision that is not in the
//!    log is not replayable (Constitution II).
//! 2. **Update state.** Books, aggregators, orders, and positions.
//! 3. **Dispatch.** Strategies see the event and whatever bars it completed.
//! 4. **Gate.** Every intent goes through [`risk::RiskGate`]. There is no
//!    second path (Constitution V).
//! 5. **Record every decision**, each naming the record that caused it.
//!
//! The loop is single-threaded and synchronous. Nothing here is async, nothing
//! spawns, and nothing shares state across threads — a future's completion
//! order is not reproducible and this loop has to be (Constitution II).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod config;

pub use config::{Capacity, EngineConfig};

use event::{
    Command, CommandEvent, EngineState, Event, EventLog, Inbound, Intent, LogError, MarketEvent,
    MarketKind, Outbound, PositionReport, RiskReason, Seq, StateReason, TimerEvent, VenueKind,
};
use marketdata::{
    AggregateError, Aggregator, Aggregators, Applied, Bar, BarSubscription, Books, MarkRule,
};
use oms::{
    OmsError, Order, OrderState, Orders, PositionError, Positions, ReconState, VenueAdapter,
};
use risk::{Decision, GateInput, RiskGate};
use std::collections::VecDeque;
use strategy::{Context, Strategy, StrategyEvent, TimerRequest};
use types::{ExchangeTime, Instrument, InstrumentId, OrderId, Qty, Side, StrategyId};

/// The most events one input may cascade into before the engine calls it a
/// storm.
///
/// A fill can produce a strategy order, which can produce another fill. That
/// terminates in practice, but "in practice" is not a bound, and the hot path
/// admits no unbounded loop (Constitution VI). Hitting this is a bug worth
/// stopping for, not a condition to ride out.
const MAX_CASCADE: usize = 4_096;

/// What the engine refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineError {
    /// The log would not take a record. Nothing after this point is
    /// replayable, so the engine halts.
    Log(LogError),
    /// The order machine refused a venue report.
    Oms(OmsError),
    /// Position accounting refused a fill.
    Position(PositionError),
    /// Bar aggregation refused a trade.
    Aggregate(AggregateError),
    /// An event named an instrument that is not configured.
    UnknownInstrument,
    /// A strategy was registered out of id order.
    StrategyOutOfOrder,
    /// One input cascaded past [`MAX_CASCADE`] events.
    EventStorm,
}

impl From<LogError> for EngineError {
    fn from(e: LogError) -> EngineError {
        EngineError::Log(e)
    }
}
impl From<OmsError> for EngineError {
    fn from(e: OmsError) -> EngineError {
        EngineError::Oms(e)
    }
}
impl From<PositionError> for EngineError {
    fn from(e: PositionError) -> EngineError {
        EngineError::Position(e)
    }
}
impl From<AggregateError> for EngineError {
    fn from(e: AggregateError) -> EngineError {
        EngineError::Aggregate(e)
    }
}

/// The trading loop.
pub struct Engine<V: VenueAdapter, L: EventLog> {
    log: L,
    venue: V,
    instruments: Vec<Instrument>,
    books: Books,
    aggregators: Aggregators,
    orders: Orders,
    positions: Positions,
    gate: RiskGate,
    strategies: Vec<Box<dyn Strategy>>,
    state: EngineState,
    mark_rule: MarkRule,
    now: ExchangeTime,
    /// Whether events are being re-derived from records that are already in
    /// the log.
    ///
    /// Set only between [`begin_replay`] and [`end_replay`], which a recovery
    /// wraps around the recorded session (contract D-1). Everything else about
    /// an event is unchanged — the book updates, the strategy runs, the venue
    /// is driven — because the point is to arrive at the state the session
    /// had, and anything skipped is state that would be missing.
    ///
    /// [`begin_replay`]: Engine::begin_replay
    /// [`end_replay`]: Engine::end_replay
    replaying: bool,
    /// Stands in for the log's sequence while replaying.
    ///
    /// Nothing written during a replay reaches the log, so this never leaves
    /// the engine; it exists because `caused_by` is not optional and a
    /// fabricated value that never escapes is better than making it so.
    replay_seq: u64,

    // Reused across events so a warmed session does not allocate.
    queue: VecDeque<Inbound>,
    intents: Vec<Intent>,
    timers: Vec<TimerRequest>,
    bars: Vec<(BarSubscription, Bar)>,
    venue_reports: Vec<Inbound>,
    scheduled: Vec<TimerRequest>,
    cancels: Vec<OrderId>,
    /// Cancels a strategy asked for during the current dispatch.
    ///
    /// Separate from `cancels`, which is the operator's flatten-and-kill
    /// sweep. Sharing one buffer would let a strategy's cancel be attributed
    /// to the operator in the log, and "who pulled this quote" is exactly the
    /// question the log exists to answer.
    strategy_cancels: Vec<OrderId>,
}

impl<V: VenueAdapter, L: EventLog> Engine<V, L> {
    /// The strategy id operator-initiated orders are attributed to.
    ///
    /// A flatten creates orders but no strategy asked for them, and leaving the
    /// attribution blank would make the log ambiguous about who traded.
    pub const OPERATOR: StrategyId = StrategyId::new(u16::MAX);

    /// Builds an engine from its configuration, a venue, and a log.
    pub fn new(config: EngineConfig, venue: V, log: L) -> Engine<V, L> {
        let count = config.instruments.len();
        let cap = config.capacity;
        Engine {
            log,
            venue,
            instruments: config.instruments,
            books: Books::with_instruments(count),
            aggregators: Aggregators::new(),
            orders: Orders::new(config.first_order_id, cap.orders),
            positions: Positions::with_instruments(count, cap.lots_per_instrument),
            gate: RiskGate::new(config.limits),
            strategies: Vec::new(),
            state: EngineState::Running,
            mark_rule: config.mark_rule,
            now: config.session_start,
            replaying: false,
            replay_seq: 0,
            queue: VecDeque::with_capacity(cap.per_step),
            intents: Vec::with_capacity(cap.per_step),
            timers: Vec::with_capacity(cap.per_step),
            bars: Vec::with_capacity(cap.per_step),
            venue_reports: Vec::with_capacity(cap.per_step),
            scheduled: Vec::with_capacity(cap.per_step),
            cancels: Vec::with_capacity(cap.orders.min(1024)),
            strategy_cancels: Vec::with_capacity(cap.per_step),
        }
    }

    /// Registers a strategy.
    ///
    /// Strategies must arrive in id order, because the id is the index their
    /// fills are routed by. Refusing here turns a mis-wiring into a startup
    /// failure rather than fills delivered to the wrong strategy.
    pub fn add_strategy(&mut self, strategy: Box<dyn Strategy>) -> Result<(), EngineError> {
        if strategy.id().index() != self.strategies.len() {
            return Err(EngineError::StrategyOutOfOrder);
        }
        self.strategies.push(strategy);
        Ok(())
    }

    /// Registers a bar aggregation and returns the subscription naming it.
    pub fn add_aggregator(&mut self, aggregator: Aggregator) -> BarSubscription {
        self.aggregators.push(aggregator)
    }

    /// Feeds one input through the loop.
    ///
    /// Anything the venue says in response is processed in the same call, so
    /// that a caller driving the engine sees a settled state when it returns.
    pub fn on_inbound(&mut self, inbound: Inbound) -> Result<(), EngineError> {
        self.queue.push_back(inbound);
        let mut steps = 0usize;
        while let Some(event) = self.queue.pop_front() {
            steps += 1;
            if steps > MAX_CASCADE {
                self.queue.clear();
                return Err(EngineError::EventStorm);
            }
            self.step(event)?;

            self.venue_reports.clear();
            self.venue.drain(&mut self.venue_reports);
            // Front to back. Draining from the end would reverse the venue's
            // report order, so an acknowledgement would arrive after the fill
            // it acknowledged.
            for report in self.venue_reports.drain(..) {
                self.queue.push_back(report);
            }
        }
        Ok(())
    }

    /// Re-derives state from events that are already in the log.
    ///
    /// Between this and [`end_replay`] the engine processes events normally —
    /// the book updates, strategies run, the venue is driven — but writes
    /// nothing, because every one of those records already exists. That is
    /// what rebuilds a crashed session's state: not a second way to install
    /// it, but the same path that produced it the first time (Constitution II).
    ///
    /// [`end_replay`]: Engine::end_replay
    pub fn begin_replay(&mut self) {
        self.replaying = true;
    }

    /// Cancels everything still live, as a restart must.
    ///
    /// A recovered session comes back with the orders it had resting. Left
    /// alone they keep filling while it is halted — the position moves and
    /// nobody decided that it should. The contract's answer is to cancel and
    /// accept the lost queue position, because a known state beats a good
    /// queue position after a crash (D-3).
    ///
    /// These cancels are ordinary decisions of the resumed session and are
    /// recorded in its new segment, so the log says who pulled the quotes.
    pub fn cancel_resting(&mut self, caused_by: Seq) -> Result<(), EngineError> {
        self.cancel_all(caused_by)
    }

    /// Ends a replay and halts, which is where a recovered session starts.
    ///
    /// Halted rather than running, and never automatic: a crash is an incident,
    /// something should look at it before the system trades again, and leaving
    /// a halt is an explicit operator decision (contract D-2, Constitution V).
    ///
    /// The halt is not itself logged. A fresh session does not record that it
    /// started running either, and the segment boundary already says a
    /// recovery happened; what must be logged is the operator's decision to
    /// leave the halt, and that still is.
    pub fn end_replay(&mut self) {
        self.replaying = false;
        self.state = EngineState::Halted;
    }

    /// Whether the engine is re-deriving state rather than trading.
    #[inline]
    pub const fn is_replaying(&self) -> bool {
        self.replaying
    }

    /// Timer requests raised since the last call.
    ///
    /// The engine records requests but does not fire them: the timer source is
    /// bound at the edge, which is what lets a backtest fire off simulated time
    /// and a live session off a real clock with nothing downstream knowing the
    /// difference (`docs/ARCHITECTURE.md` seam 2).
    pub fn take_timer_requests(&mut self, out: &mut Vec<TimerRequest>) {
        out.append(&mut self.scheduled);
    }

    /// Where the engine is.
    #[inline]
    pub const fn state(&self) -> EngineState {
        self.state
    }

    /// The exchange time of the last event carrying one.
    #[inline]
    pub const fn now(&self) -> ExchangeTime {
        self.now
    }

    /// The event log.
    #[inline]
    pub const fn log(&self) -> &L {
        &self.log
    }

    /// The system's own position accounting.
    #[inline]
    pub const fn positions(&self) -> &Positions {
        &self.positions
    }

    /// Every order the session created.
    #[inline]
    pub const fn orders(&self) -> &Orders {
        &self.orders
    }

    /// Current book state.
    #[inline]
    pub const fn books(&self) -> &Books {
        &self.books
    }

    /// The venue this engine is bound to.
    #[inline]
    pub const fn venue(&self) -> &V {
        &self.venue
    }

    /// Consumes the engine and hands back its log.
    ///
    /// Consuming rather than lending `&mut`: a mutable log would let a caller
    /// append records the engine never decided, which is a different bypass
    /// from the risk gate's but the same kind of mistake. Taking the engine
    /// apart is only reasonable once the session is over, and then it is the
    /// only way to flush a file-backed log and find out whether the flush
    /// worked.
    #[inline]
    pub fn into_log(self) -> L {
        self.log
    }

    /// Whether any reserved buffer is full, so the next event may allocate.
    ///
    /// Reported rather than enforced: growing costs an allocation, and a caller
    /// that cares about the latency budget wants to know before it happens
    /// (Constitution VI). Log growth is the log implementation's business and
    /// is not covered here.
    pub fn would_allocate(&self) -> bool {
        self.orders.would_grow()
            || self.queue.len() == self.queue.capacity()
            || self.intents.len() == self.intents.capacity()
            || self.timers.len() == self.timers.capacity()
            || self.bars.len() == self.bars.capacity()
            || self.venue_reports.len() == self.venue_reports.capacity()
            || self.scheduled.len() == self.scheduled.capacity()
            || self.positions.iter().any(|p| p.lots_would_grow())
    }

    /// Records a decision, unless it is already in the log.
    ///
    /// Every decision the engine makes goes through here. A replay re-derives
    /// decisions that were recorded the first time round, so writing them
    /// again would double them; suppressing them in one place means no caller
    /// has to remember to.
    fn record(&mut self, out: Outbound) -> Result<(), LogError> {
        if self.replaying {
            return Ok(());
        }
        self.log.append(Event::Out(out)).map(|_| ())
    }

    // ---- one event -------------------------------------------------------

    fn step(&mut self, inbound: Inbound) -> Result<(), EngineError> {
        // Record the input before anything reads it. If this fails there is no
        // point continuing: whatever the engine decided next would not be in
        // the log, and the session would stop being replayable. Halting is not
        // recorded either, for the same reason.
        // Record the input before anything reads it — unless this event is
        // already in the log, which is exactly what a replay is. Writing it
        // again would duplicate the session it is rebuilding.
        let seq = if self.replaying {
            self.replay_seq += 1;
            Seq::new(self.replay_seq)
        } else {
            match self.log.append(Event::In(inbound)) {
                Ok(seq) => seq,
                Err(e) => {
                    self.state = EngineState::Halted;
                    return Err(EngineError::Log(e));
                }
            }
        };

        if let Some(t) = inbound.exchange_time() {
            self.now = t;
        }

        match inbound {
            Inbound::Market(m) => self.on_market(m)?,
            Inbound::Venue(v) => self.on_venue_report(v)?,
            Inbound::VenuePosition(p) => self.on_position_report(p, seq)?,
            Inbound::Timer(t) => self.on_timer(t),
            Inbound::Command(c) => self.on_command(c, seq)?,
        }

        self.flush_intents(seq)?;
        self.flush_strategy_cancels(seq)?;
        self.flush_timer_requests(seq)?;
        Ok(())
    }

    fn on_market(&mut self, m: MarketEvent) -> Result<(), EngineError> {
        match self.books.apply(&m) {
            Applied::Accepted => {}
            Applied::UnknownInstrument => return Err(EngineError::UnknownInstrument),
            Applied::OutOfOrder { .. } | Applied::UpdateTooLarge => {
                // The book refused it, so nothing downstream may see it either.
                // Letting it reach the venue would leave a simulated venue's
                // book ahead of the engine's; letting it reach the aggregators
                // would put a bar out of order. `Books` counts both refusals,
                // and the counts are reachable through `books()`.
                return Ok(());
            }
        }

        // Unconditional, and *before* the level check below: a simulated venue
        // keeps its own book and needs the levels to build one. It buffers them
        // exactly as `Books` does, so it reaches the same state at the same
        // moment. A real venue ignores this entirely — no branch here asks
        // which is bound.
        self.venue.observe_market(&m);

        // A level on its own is half an update. It is in the books' buffers,
        // not in the books, so nothing further may run on it: a strategy would
        // read a crossed book and an aggregator would take a bar from one
        // (ADR, order-book depth D-2).
        if matches!(m.kind, MarketKind::Level { .. }) {
            return Ok(());
        }

        // Bars whose window closed at or before this event belong *before* it:
        // they describe a period that had already ended when it arrived.
        self.bars.clear();
        self.aggregators.on_time(m.exchange_time, &mut self.bars);
        self.dispatch_bars();

        match m.kind {
            MarketKind::Quote { .. } => {
                let top = *self.books.top(m.instrument).expect("just applied");
                self.dispatch_all(&StrategyEvent::Quote {
                    instrument: m.instrument,
                    top: &top,
                });
            }
            // The update is whole now, so the book has a new top and everyone
            // may read it. Same event a quote feed would have produced.
            MarketKind::BookApplied => {
                if let Some(top) = self.books.top(m.instrument).copied() {
                    self.dispatch_all(&StrategyEvent::Quote {
                        instrument: m.instrument,
                        top: &top,
                    });
                }
            }
            MarketKind::Level { .. } => {}
            MarketKind::Trade { px, qty, aggressor } => {
                self.dispatch_all(&StrategyEvent::Trade {
                    instrument: m.instrument,
                    px,
                    qty,
                    aggressor,
                });
                // The bar this trade completes contains it, so it is delivered
                // after the trade rather than before.
                self.bars.clear();
                let mut bars = std::mem::take(&mut self.bars);
                let folded =
                    self.aggregators
                        .on_trade(m.instrument, px, qty, m.exchange_time, &mut bars);
                self.bars = bars;
                folded?;
                self.dispatch_bars();
            }
        }
        Ok(())
    }

    fn on_venue_report(&mut self, v: event::VenueEvent) -> Result<(), EngineError> {
        let before = *self.orders.get(v.order).ok_or(OmsError::UnknownOrder)?;
        self.orders.apply(v.order, v.kind, v.venue_time)?;
        let after = *self.orders.get(v.order).expect("order exists");

        if let VenueKind::Filled { px, qty, fee } = v.kind {
            self.positions
                .apply_fill(after.instrument(), after.side(), qty, px, fee)?;
            self.dispatch_one(
                after.strategy(),
                &StrategyEvent::Fill {
                    order: after.id(),
                    instrument: after.instrument(),
                    side: after.side(),
                    px,
                    qty,
                    fee,
                },
            );
        }

        // Tell the strategy the moment its order becomes nameable. Before this
        // there is no id to hand it, so a resting order it could not cancel
        // would be a quote it could not pull.
        // `is_live` is "not terminal", so a freshly submitted order is already
        // live by that reading and this cannot be phrased in terms of it. What
        // is wanted is narrower: the order stopped being unacknowledged and is
        // now working at the venue. An order that went straight to a fill
        // never rested, and the fill says everything there is to say about it.
        if before.state() == OrderState::Pending && after.state() == OrderState::Working {
            self.dispatch_one(
                after.strategy(),
                &StrategyEvent::OrderLive {
                    order: after.id(),
                    instrument: after.instrument(),
                    side: after.side(),
                    qty: after.qty(),
                    kind: after.kind(),
                },
            );
        }

        // Tell the strategy once, at the moment the order stops being live.
        if !before.state().is_terminal() && after.state().is_terminal() {
            self.dispatch_one(
                after.strategy(),
                &StrategyEvent::OrderDone {
                    order: after.id(),
                    instrument: after.instrument(),
                    side: after.side(),
                    state: after.state(),
                    unfilled: after.remaining(),
                },
            );
        }
        Ok(())
    }

    fn on_position_report(&mut self, p: PositionReport, seq: Seq) -> Result<(), EngineError> {
        let position = self
            .positions
            .get_mut(p.instrument)
            .ok_or(EngineError::UnknownInstrument)?;
        let recon = position.observe_venue(p.venue_qty);

        // A divergence halts trading and never resolves itself
        // (Constitution V). Reduce-only orders still pass the gate, so an
        // operator can still get out.
        if matches!(recon, ReconState::Diverged { .. }) && self.state == EngineState::Running {
            self.transition(
                EngineState::Halted,
                StateReason::ReconciliationDivergence,
                seq,
            )?;
        }
        Ok(())
    }

    fn on_timer(&mut self, t: TimerEvent) {
        self.dispatch_one(t.strategy, &StrategyEvent::Timer { token: t.token });
    }

    fn on_command(&mut self, c: CommandEvent, seq: Seq) -> Result<(), EngineError> {
        match c.command {
            Command::Halt => {
                if self.state == EngineState::Running {
                    self.transition(EngineState::Halted, StateReason::OperatorCommand, seq)?;
                }
            }
            Command::Resume => {
                if self.state == EngineState::Halted {
                    if self.positions.any_diverged() {
                        // Resuming out of a reconciliation halt is impossible
                        // until the books agree. Recorded as a transition to
                        // the same state so that the refusal is in the log
                        // rather than being an absence of one.
                        self.transition(
                            EngineState::Halted,
                            StateReason::ReconciliationDivergence,
                            seq,
                        )?;
                    } else {
                        self.transition(EngineState::Running, StateReason::OperatorCommand, seq)?;
                    }
                }
            }
            Command::Kill => {
                if self.state != EngineState::Killed {
                    self.transition(EngineState::Killed, StateReason::OperatorCommand, seq)?;
                }
                self.cancel_all(seq)?;
            }
            Command::Flatten => self.raise_flatten_intents(),
        }
        Ok(())
    }

    // ---- decisions -------------------------------------------------------

    fn transition(
        &mut self,
        to: EngineState,
        reason: StateReason,
        caused_by: Seq,
    ) -> Result<(), EngineError> {
        let from = self.state;
        self.state = to;
        self.record(Outbound::StateChanged {
            caused_by,
            from,
            to,
            reason,
        })?;
        Ok(())
    }

    /// Asks the venue to cancel everything still live.
    fn cancel_all(&mut self, caused_by: Seq) -> Result<(), EngineError> {
        self.cancels.clear();
        let mut cancels = std::mem::take(&mut self.cancels);
        cancels.extend(self.orders.live().map(|o| o.id()));
        for id in cancels.drain(..) {
            // A venue that will not take the cancel is recorded and moved past:
            // the kill switch has to work from every state, including a
            // disconnected one (Constitution V).
            let sent = self.venue.cancel(id).is_ok();
            if sent {
                let at = self.now;
                if let Some(order) = self.orders.get_mut(id) {
                    let _ = order.request_cancel(at);
                }
                self.record(Outbound::CancelSubmitted {
                    caused_by,
                    order: id,
                })?;
            }
        }
        self.cancels = cancels;
        Ok(())
    }

    /// Raises a reduce-only intent for every instrument that is not flat.
    fn raise_flatten_intents(&mut self) {
        for index in 0..self.positions.len() {
            let instrument = InstrumentId::new(index as u32);
            let Some(position) = self.positions.get(instrument) else {
                continue;
            };
            let held = position.qty();
            let Some(side) = Side::reducing(held) else {
                continue;
            };
            self.intents.push(Intent {
                strategy: Self::OPERATOR,
                instrument,
                side,
                qty: Qty::from_scaled(held.to_scaled().saturating_abs()),
                kind: event::OrderKind::Market,
                reduce_only: true,
            });
        }
    }

    fn flush_intents(&mut self, caused_by: Seq) -> Result<(), EngineError> {
        let mut intents = std::mem::take(&mut self.intents);
        let mut outcome = Ok(());
        for intent in intents.drain(..) {
            if let Err(e) = self.process_intent(&intent, caused_by) {
                outcome = Err(e);
                break;
            }
        }
        intents.clear();
        // Put the buffer back either way, so an error does not cost the
        // reservation and turn the next event into an allocation.
        self.intents = intents;
        outcome
    }

    /// Sends the cancels strategies asked for during this event.
    ///
    /// A cancel is not put to the risk gate: pulling an order only ever
    /// reduces exposure, and a gate that could refuse one would be a gate that
    /// can trap a strategy in a position.
    fn flush_strategy_cancels(&mut self, caused_by: Seq) -> Result<(), EngineError> {
        let mut cancels = std::mem::take(&mut self.strategy_cancels);
        let mut outcome = Ok(());
        for id in cancels.drain(..) {
            // Ownership, and only ownership, is enforced here. One strategy
            // pulling another's quote would otherwise be silent, and the log
            // would show a cancel with no honest author.
            let Some(order) = self.orders.get(id) else {
                continue;
            };
            if !order.state().is_live() {
                // A fill and a cancel crossed. Not an error: a strategy that
                // had to win that race would be wrong occasionally instead of
                // never.
                continue;
            }
            let owner = order.strategy();
            let at = self.now;
            if self.venue.cancel(id).is_err() {
                // Unreachable venue. The order stays live in our books, which
                // is the honest state: we do not know that it is gone
                // (Constitution V).
                continue;
            }
            if let Some(order) = self.orders.get_mut(id) {
                let _ = order.request_cancel(at);
            }
            let _ = owner;
            if let Err(e) = self.record(Outbound::CancelSubmitted {
                caused_by,
                order: id,
            }) {
                outcome = Err(EngineError::Log(e));
                break;
            }
        }
        cancels.clear();
        self.strategy_cancels = cancels;
        outcome
    }

    fn process_intent(&mut self, intent: &Intent, caused_by: Seq) -> Result<(), EngineError> {
        let Some(instrument) = self.instruments.get(intent.instrument.index()).copied() else {
            return self.reject(intent, RiskReason::UnknownInstrument, caused_by);
        };

        let position = self.positions.get(intent.instrument);
        let input = GateInput {
            state: self.state,
            now: self.now,
            instrument,
            position: position.map(|p| p.qty()).unwrap_or(Qty::ZERO),
            reconciled: !matches!(
                position.map(|p| p.recon()),
                Some(ReconState::Diverged { .. })
            ),
            quote_age: self.books.quote_age(intent.instrument, self.now),
            mark: self.books.mark(intent.instrument, self.mark_rule),
            taker_px: self
                .books
                .top(intent.instrument)
                .map(|t| t.taker_px(intent.side)),
        };

        let approved = match self.gate.check(intent, &input) {
            Decision::Approved(a) => a,
            Decision::Rejected(reason) => return self.reject(intent, reason, caused_by),
        };

        // Build the order, then try to send it, and only record it if it left.
        // Doing it in this order means the log never contains an order that
        // never went anywhere, and no phantom sits Pending forever.
        let id = self.orders.next_id();
        let order = Order::new(
            id,
            approved.strategy,
            approved.instrument,
            approved.side,
            approved.qty,
            approved.kind,
            approved.reduce_only,
            self.now,
        );
        if self.venue.submit(&order).is_err() {
            // We could not reach the venue. That is exactly the ambiguity the
            // fail-closed rule is for: no order exists (Constitution V).
            return self.reject(intent, RiskReason::VenueUnreachable, caused_by);
        }
        self.orders.insert(order)?;
        self.record(Outbound::OrderSubmitted {
            caused_by,
            order: id,
            strategy: approved.strategy,
            instrument: approved.instrument,
            side: approved.side,
            qty: approved.qty,
            kind: approved.kind,
            reduce_only: approved.reduce_only,
        })?;
        Ok(())
    }

    fn reject(
        &mut self,
        intent: &Intent,
        reason: RiskReason,
        caused_by: Seq,
    ) -> Result<(), EngineError> {
        self.record(Outbound::IntentRejected {
            caused_by,
            strategy: intent.strategy,
            instrument: intent.instrument,
            side: intent.side,
            qty: intent.qty,
            reason,
        })?;
        // Tell the strategy. A strategy that tracks what it has in flight is
        // wrong from the first refusal onwards otherwise: no order exists, so
        // no fill and no terminal transition will ever release the quantity.
        //
        // Anything it raises in response lands in the buffer this flush already
        // took, so it is processed on the next event rather than re-entering
        // here. That is deliberate: a strategy that re-orders on every refusal
        // would otherwise loop inside one event.
        self.dispatch_one(
            intent.strategy,
            &StrategyEvent::IntentRefused {
                instrument: intent.instrument,
                side: intent.side,
                qty: intent.qty,
                reason,
            },
        );
        Ok(())
    }

    fn flush_timer_requests(&mut self, caused_by: Seq) -> Result<(), EngineError> {
        let mut timers = std::mem::take(&mut self.timers);
        let mut outcome = Ok(());
        for request in timers.drain(..) {
            if let Err(e) = self.record(Outbound::TimerRequested {
                caused_by,
                strategy: request.strategy,
                token: request.token,
                at: request.at,
            }) {
                outcome = Err(EngineError::Log(e));
                break;
            }
            self.scheduled.push(request);
        }
        timers.clear();
        self.timers = timers;
        outcome
    }

    // ---- dispatch --------------------------------------------------------

    fn dispatch_all(&mut self, event: &StrategyEvent<'_>) {
        let now = self.now;
        for s in self.strategies.iter_mut() {
            let mut ctx = Context::new(
                s.id(),
                now,
                &mut self.intents,
                &mut self.timers,
                &mut self.strategy_cancels,
            );
            s.on_event(event, &mut ctx);
        }
    }

    fn dispatch_one(&mut self, strategy: StrategyId, event: &StrategyEvent<'_>) {
        let now = self.now;
        // An operator-attributed order has no strategy to tell, which is the
        // only way this lookup misses.
        if let Some(s) = self.strategies.get_mut(strategy.index()) {
            let mut ctx = Context::new(
                s.id(),
                now,
                &mut self.intents,
                &mut self.timers,
                &mut self.strategy_cancels,
            );
            s.on_event(event, &mut ctx);
        }
    }

    fn dispatch_bars(&mut self) {
        if self.bars.is_empty() {
            return;
        }
        let mut bars = std::mem::take(&mut self.bars);
        for (subscription, bar) in bars.drain(..) {
            self.dispatch_all(&StrategyEvent::Bar {
                subscription,
                bar: &bar,
            });
        }
        self.bars = bars;
    }
}

/// Where inputs come from.
///
/// The other of the two edges (`docs/ARCHITECTURE.md` seam 2). A backtest binds
/// a recorded feed, a live session binds a socket, and neither the engine nor
/// any strategy can tell which.
///
/// It also owns the timer source, which is why [`schedule_timer`] is here
/// rather than on the engine: timers fire off simulated time in a backtest and
/// off a real clock live, and that difference belongs at the edge.
///
/// [`schedule_timer`]: FeedAdapter::schedule_timer
pub trait FeedAdapter {
    /// The next input, or `None` when the feed is exhausted.
    fn next_event(&mut self) -> Option<Inbound>;

    /// Takes a timer request a strategy raised.
    ///
    /// The default drops it. A feed that cannot produce timers should say so by
    /// not offering them rather than by pretending to schedule them, and a
    /// strategy that needs timers will visibly never wake.
    fn schedule_timer(&mut self, _request: TimerRequest) {}
}

/// Pumps a feed through an engine until the feed is exhausted.
///
/// This is the whole driver. It contains no domain logic and no mode branch —
/// which feed and which venue were bound is decided before it is called, and
/// nothing it does can observe the difference (Constitution III).
pub fn run<F, V, L>(feed: &mut F, engine: &mut Engine<V, L>) -> Result<(), EngineError>
where
    F: FeedAdapter,
    V: VenueAdapter,
    L: EventLog,
{
    let mut requests = Vec::new();
    while let Some(event) = feed.next_event() {
        engine.on_inbound(event)?;
        engine.take_timer_requests(&mut requests);
        for request in requests.drain(..) {
            feed.schedule_timer(request);
        }
    }
    Ok(())
}
