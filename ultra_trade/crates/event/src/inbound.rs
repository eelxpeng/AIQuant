//! Everything that happens *to* the system.
//!
//! An inbound event is an input to the session. Replay feeds exactly this set
//! back through a fresh engine, so if something influenced an order and is not
//! here, replay is a lie (Constitution II).

use types::{
    ExchangeTime, InstrumentId, Notional, OrderId, Px, Qty, ReceiveTime, Side, StrategyId,
};

/// An input to the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Inbound {
    /// The market moved.
    Market(MarketEvent),
    /// The venue said something about an order.
    Venue(VenueEvent),
    /// The venue reported its own view of a position.
    VenuePosition(PositionReport),
    /// A timer a strategy asked for came due.
    Timer(TimerEvent),
    /// An operator acted.
    Command(CommandEvent),
}

impl Inbound {
    /// When the process observed this event.
    ///
    /// Every input has one: something arrived, locally, at a moment. This is
    /// the clock for latency measurement and never for market-state logic
    /// (`CONTEXT.md`).
    #[inline]
    pub const fn receive_time(&self) -> ReceiveTime {
        match self {
            Inbound::Market(e) => e.receive_time,
            Inbound::Venue(e) => e.receive_time,
            Inbound::VenuePosition(e) => e.receive_time,
            Inbound::Timer(e) => e.receive_time,
            Inbound::Command(e) => e.receive_time,
        }
    }

    /// The venue's timestamp, where a venue assigned one.
    ///
    /// `None` for an operator command, because no venue was involved and there
    /// is no honest value to put here. Synthesizing one — say, the last quote's
    /// time — would let a strategy read a market time that no market produced.
    #[inline]
    pub const fn exchange_time(&self) -> Option<ExchangeTime> {
        match self {
            Inbound::Market(e) => Some(e.exchange_time),
            Inbound::Venue(e) => Some(e.venue_time),
            Inbound::VenuePosition(e) => Some(e.venue_time),
            Inbound::Timer(e) => Some(e.fires_at),
            Inbound::Command(_) => None,
        }
    }
}

/// A market-data event for one instrument.
///
/// Carries both clocks because both are real and neither substitutes for the
/// other: the venue said when it happened, and the process observed it later.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MarketEvent {
    /// Which instrument moved.
    pub instrument: InstrumentId,
    /// When the venue says it happened. The only clock valid for ordering
    /// market events.
    pub exchange_time: ExchangeTime,
    /// When this process saw it.
    pub receive_time: ReceiveTime,
    /// What happened.
    pub kind: MarketKind,
}

/// What kind of market event this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarketKind {
    /// Top of book changed.
    Quote {
        /// Best bid.
        bid_px: Px,
        /// Size at the best bid.
        bid_qty: Qty,
        /// Best ask.
        ask_px: Px,
        /// Size at the best ask.
        ask_qty: Qty,
    },
    /// One price level of the order book changed.
    ///
    /// A book update is a run of these followed by [`MarketKind::BookApplied`],
    /// because ten levels a side does not fit a fixed 80-byte record and the
    /// fixed size is what makes seeking arithmetic (ADR, order-book depth).
    ///
    /// **Nothing downstream sees a level on its own.** Between the first level
    /// of an update and its completion the book can be crossed — a state the
    /// venue never published — so the engine holds them until the update is
    /// whole.
    Level {
        /// Which side of the book.
        side: Side,
        /// The price of the level.
        px: Px,
        /// The size resting there now. **Zero removes the level**, which is
        /// how the venue says it too.
        qty: Qty,
    },
    /// Every level of the update just before this one is now in force.
    ///
    /// The marker that makes a run of levels atomic. Without it a strategy
    /// could read a book part-way through being rewritten.
    BookApplied,
    /// A trade printed.
    Trade {
        /// The price it printed at.
        px: Px,
        /// How much traded.
        qty: Qty,
        /// Which side took liquidity.
        aggressor: Side,
    },
}

/// Something the venue said about one of our orders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct VenueEvent {
    /// The order this concerns.
    pub order: OrderId,
    /// The venue's timestamp.
    pub venue_time: ExchangeTime,
    /// When this process saw it.
    pub receive_time: ReceiveTime,
    /// What the venue said.
    pub kind: VenueKind,
}

/// What the venue said.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VenueKind {
    /// The order is live.
    Accepted,
    /// The order never became live.
    Rejected {
        /// Why.
        reason: RejectReason,
    },
    /// Part or all of the order executed.
    Filled {
        /// The execution price.
        px: Px,
        /// How much executed on this report.
        qty: Qty,
        /// The fee charged, signed: positive is paid, negative is a rebate.
        fee: Notional,
    },
    /// The order is no longer live and will not execute further.
    Cancelled,
    /// A cancel request did not take effect.
    CancelRejected {
        /// Why.
        reason: RejectReason,
    },
    /// The order reached its expiry without fully executing.
    Expired,
}

/// Why a venue refused something.
///
/// A closed set with no free-text arm: this crate is on the hot path and an
/// error carrying a `String` would allocate there (Constitution VI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RejectReason {
    /// The venue does not know this instrument.
    UnknownInstrument,
    /// The price was off-tick, outside bands, or otherwise unacceptable.
    InvalidPrice,
    /// The quantity was off-lot, below minimum, or otherwise unacceptable.
    InvalidQuantity,
    /// Not enough margin or buying power.
    InsufficientFunds,
    /// We sent too fast.
    RateLimited,
    /// The venue has no record of this order.
    UnknownOrder,
    /// The order was already terminal when the request arrived.
    AlreadyTerminal,
    /// The venue is not accepting orders right now.
    Unavailable,
}

/// The venue's own view of a position, for reconciliation.
///
/// Compared against the system's accounting; a divergence halts trading and is
/// never silently resolved (Constitution V).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PositionReport {
    /// Which instrument.
    pub instrument: InstrumentId,
    /// The venue's signed net quantity.
    pub venue_qty: Qty,
    /// The venue's timestamp.
    pub venue_time: ExchangeTime,
    /// When this process saw it.
    pub receive_time: ReceiveTime,
}

/// A wake-up a strategy asked for, now due.
///
/// Both the request and this firing are in the log, so both replay. This is how
/// a strategy acts when no market data arrives (`CONTEXT.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerEvent {
    /// Who asked.
    pub strategy: StrategyId,
    /// The strategy's own label for this timer, returned unchanged.
    pub token: TimerToken,
    /// The instant this timer represents, on the exchange clock.
    ///
    /// In live this is set from the real clock at the edge; in backtest from
    /// simulated time. Nothing downstream can tell which (Constitution III).
    pub fires_at: ExchangeTime,
    /// When this process saw it.
    pub receive_time: ReceiveTime,
}

/// A strategy's opaque label for one of its timers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimerToken(u64);

impl TimerToken {
    /// Builds a token from its raw number.
    #[inline]
    pub const fn new(raw: u64) -> TimerToken {
        TimerToken(raw)
    }

    /// The underlying number.
    #[inline]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// An operator action, recorded as an input to the session.
///
/// This is the only way a client reaches the engine (`docs/ARCHITECTURE.md`
/// seam 5). Because it lives in the log, replay reproduces the operator's
/// actions and the audit trail exists with no separate logging path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CommandEvent {
    /// What was asked for.
    pub command: Command,
    /// When this process saw it.
    pub receive_time: ReceiveTime,
}

/// The v1 control surface, in full.
///
/// Exactly four. Anything beyond these is a manual trading terminal, which is a
/// different product with a different risk profile (`docs/ARCHITECTURE.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Command {
    /// Stop taking new risk. Reduce-only orders still pass the gate.
    Halt,
    /// Leave a halt. Never automatic.
    Resume,
    /// Stop everything, and cancel what is resting. Terminal for the session.
    Kill,
    /// Drive every position to zero with reduce-only orders.
    Flatten,
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::Timestamp;

    fn exch(n: i64) -> ExchangeTime {
        Timestamp::from_nanos(n)
    }

    fn recv(n: i64) -> ReceiveTime {
        Timestamp::from_nanos(n)
    }

    #[test]
    fn every_input_reports_when_the_process_saw_it() {
        let command = Inbound::Command(CommandEvent {
            command: Command::Halt,
            receive_time: recv(99),
        });
        assert_eq!(command.receive_time(), recv(99));
    }

    #[test]
    fn a_command_has_no_exchange_time() {
        // The degenerate case that matters: no venue was involved, so there is
        // no honest exchange timestamp and we do not invent one.
        let command = Inbound::Command(CommandEvent {
            command: Command::Kill,
            receive_time: recv(5),
        });
        assert_eq!(command.exchange_time(), None);
    }

    #[test]
    fn a_market_event_reports_both_clocks_separately() {
        let quote = Inbound::Market(MarketEvent {
            instrument: InstrumentId::new(0),
            exchange_time: exch(1_000),
            receive_time: recv(1_200),
            kind: MarketKind::Quote {
                bid_px: Px::from_scaled(1),
                bid_qty: Qty::from_scaled(1),
                ask_px: Px::from_scaled(2),
                ask_qty: Qty::from_scaled(1),
            },
        });
        assert_eq!(quote.exchange_time(), Some(exch(1_000)));
        assert_eq!(quote.receive_time(), recv(1_200));
    }
}
