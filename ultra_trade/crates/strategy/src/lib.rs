//! The strategy seam: what a strategy sees, and what it can do about it.
//!
//! The interface is narrower than it first looks. A strategy sees events,
//! current-event-time, and the aggregates it subscribed to — and reaches
//! nothing. No clock, no socket, no file (Constitution I, III).
//!
//! # Why there is no clock
//!
//! "What time is it now?" is answered by [`Context::now`], the exchange
//! timestamp of the event being processed. That is not a clock, it is a
//! property of the event, and it makes **look-ahead structurally impossible**:
//! a strategy cannot observe a time later than the event in hand. The most
//! expensive bug class in this domain is eliminated by the shape of the
//! interface rather than by review.
//!
//! "Wake me later, even if nothing happens" is answered by [`Context::timer`].
//! The request is a record and the firing is an event, so both replay, and a
//! backtest fires them off simulated time without anything downstream knowing
//! (`docs/ARCHITECTURE.md` seam 4).
//!
//! # Why a strategy tracks its own position
//!
//! Nothing hands a strategy the system's position, because the system's
//! position is the sum of *every* strategy's fills plus whatever an operator
//! did. A strategy is told about its own fills and may keep its own count. The
//! authoritative books live in `oms`, and the risk gate is what compares an
//! intent against them.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod crossover;
mod quoter;

pub use crossover::MovingAverageCrossover;
pub use quoter::Quoter;

use event::{Intent, OrderKind, RiskReason, TimerToken};
use marketdata::{Bar, BarSubscription, TopOfBook};
use oms::OrderState;
use types::{ExchangeTime, InstrumentId, Notional, OrderId, Px, Qty, Side, StrategyId};

/// Something a strategy is told about.
///
/// Borrowed rather than owned: the engine holds the book and the completed bar
/// already, and copying them into the callback would allocate on the hot path.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum StrategyEvent<'a> {
    /// Top of book changed.
    Quote {
        /// Which instrument.
        instrument: InstrumentId,
        /// The new top of book.
        top: &'a TopOfBook,
    },
    /// A trade printed.
    Trade {
        /// Which instrument.
        instrument: InstrumentId,
        /// The price it printed at.
        px: Px,
        /// How much traded.
        qty: Qty,
        /// Which side took liquidity.
        aggressor: Side,
    },
    /// A bar completed.
    ///
    /// Only complete bars are ever delivered. There is no event for a forming
    /// one, because reading a bar's close before its window shuts is
    /// look-ahead (`docs/ARCHITECTURE.md` seam 6).
    Bar {
        /// Which configured aggregation produced it.
        subscription: BarSubscription,
        /// The completed bar.
        bar: &'a Bar,
    },
    /// One of this strategy's orders is live at the venue.
    ///
    /// The only place a strategy learns the id of an order it asked for. It
    /// cannot be told at [`Context::order`] time, because no order exists
    /// until the risk gate has approved the intent and the venue has taken it
    /// — until then there is nothing to name (`CONTEXT.md`).
    ///
    /// Without this a strategy can place a resting order and never manage it:
    /// no id means no cancel, and a quote it cannot pull is a quote that fills
    /// on every adverse move.
    OrderLive {
        /// The order, now nameable.
        order: OrderId,
        /// What it trades.
        instrument: InstrumentId,
        /// Which way.
        side: Side,
        /// How much, as the gate rounded it.
        qty: Qty,
        /// Market or limit, with the limit price as the gate rounded it.
        kind: OrderKind,
    },
    /// One of this strategy's orders executed, in part or in full.
    Fill {
        /// The order.
        order: OrderId,
        /// What it trades.
        instrument: InstrumentId,
        /// Which way.
        side: Side,
        /// The execution price.
        px: Px,
        /// How much executed on this report.
        qty: Qty,
        /// The fee charged.
        fee: Notional,
    },
    /// The risk gate refused an intent, so no order exists.
    ///
    /// Without this a strategy that tracks what it has in flight is wrong from
    /// the first refusal onwards: no order was created, so no fill and no
    /// [`OrderDone`] will ever arrive to release the quantity it thinks is
    /// working. It then under-orders forever, and only a session where a limit
    /// actually binds reveals it.
    ///
    /// [`OrderDone`]: StrategyEvent::OrderDone
    IntentRefused {
        /// What it wanted to trade.
        instrument: InstrumentId,
        /// Which way.
        side: Side,
        /// How much it asked for, before any rounding.
        qty: Qty,
        /// Which check refused it.
        reason: RiskReason,
    },
    /// One of this strategy's orders reached a state it will not leave.
    ///
    /// Carries the side and the unfilled remainder because a strategy that
    /// tracks what it has in flight has to release the part that will never
    /// execute. Without both, a cancelled order would leave the strategy
    /// believing it still had quantity working.
    OrderDone {
        /// The order.
        order: OrderId,
        /// What it trades.
        instrument: InstrumentId,
        /// Which way it was going.
        side: Side,
        /// What it ended as.
        state: OrderState,
        /// The quantity that will never execute.
        unfilled: Qty,
    },
    /// A timer this strategy asked for came due.
    Timer {
        /// The label it was requested with.
        token: TimerToken,
    },
}

/// A future wake-up a strategy asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimerRequest {
    /// Who asked.
    pub strategy: StrategyId,
    /// When they want to be woken, on the exchange clock.
    pub at: ExchangeTime,
    /// Their own label, returned unchanged when it fires.
    pub token: TimerToken,
}

/// What a strategy may do in response to an event.
///
/// Collecting into caller-owned buffers rather than returning a collection:
/// the engine reuses them across events, so a warmed session does not allocate
/// while dispatching (Constitution VI).
#[derive(Debug)]
pub struct Context<'a> {
    strategy: StrategyId,
    now: ExchangeTime,
    intents: &'a mut Vec<Intent>,
    timers: &'a mut Vec<TimerRequest>,
    cancels: &'a mut Vec<OrderId>,
}

impl<'a> Context<'a> {
    /// Builds a context for one dispatch.
    pub fn new(
        strategy: StrategyId,
        now: ExchangeTime,
        intents: &'a mut Vec<Intent>,
        timers: &'a mut Vec<TimerRequest>,
        cancels: &'a mut Vec<OrderId>,
    ) -> Context<'a> {
        Context {
            strategy,
            now,
            intents,
            timers,
            cancels,
        }
    }

    /// The exchange timestamp of the event being processed.
    ///
    /// This is what a strategy reads instead of a clock. It cannot run ahead of
    /// the event in hand, which is what makes look-ahead impossible rather than
    /// merely discouraged.
    #[inline]
    pub const fn now(&self) -> ExchangeTime {
        self.now
    }

    /// Which strategy this context belongs to.
    #[inline]
    pub const fn id(&self) -> StrategyId {
        self.strategy
    }

    /// Asks to trade. The intent still has to pass the risk gate.
    #[inline]
    pub fn order(&mut self, instrument: InstrumentId, side: Side, qty: Qty, kind: OrderKind) {
        self.intents.push(Intent {
            strategy: self.strategy,
            instrument,
            side,
            qty,
            kind,
            reduce_only: false,
        });
    }

    /// Asks to trade in a way that can only decrease `|position|`.
    #[inline]
    pub fn reduce(&mut self, instrument: InstrumentId, side: Side, qty: Qty, kind: OrderKind) {
        self.intents.push(Intent {
            strategy: self.strategy,
            instrument,
            side,
            qty,
            kind,
            reduce_only: true,
        });
    }

    /// Asks to pull one of this strategy's own orders.
    ///
    /// Not a risk decision: a cancel only ever reduces exposure, so it is not
    /// put to the gate. It is still refused if the order belongs to another
    /// strategy — one strategy pulling another's quote is a bug that would
    /// otherwise be silent and very hard to see.
    ///
    /// Cancelling an order that is already terminal is a no-op rather than an
    /// error. A fill and a cancel can cross, and a strategy that had to win
    /// that race would be wrong occasionally rather than never.
    #[inline]
    pub fn cancel(&mut self, order: OrderId) {
        self.cancels.push(order);
    }

    /// How many cancels have been raised in this dispatch.
    #[inline]
    pub fn cancel_count(&self) -> usize {
        self.cancels.len()
    }

    /// Asks to be woken at a future instant.
    ///
    /// A request in the past is still recorded rather than silently dropped;
    /// the engine decides when it fires, and hiding the request would hide a
    /// strategy bug.
    #[inline]
    pub fn timer(&mut self, at: ExchangeTime, token: TimerToken) {
        self.timers.push(TimerRequest {
            strategy: self.strategy,
            at,
            token,
        });
    }

    /// How many intents have been raised in this dispatch.
    #[inline]
    pub fn intent_count(&self) -> usize {
        self.intents.len()
    }
}

/// Consumes events, emits intents, reaches nothing.
pub trait Strategy {
    /// The id this instance is registered under.
    ///
    /// Every intent it raises carries this, so an order in the log traces back
    /// to the strategy that asked for it without a second lookup path.
    fn id(&self) -> StrategyId;

    /// Handles one event.
    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>);
}
