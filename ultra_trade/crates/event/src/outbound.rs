//! Everything the system *decided*.
//!
//! Replay reproduces this sequence byte-for-byte from the inbound records
//! alone. A difference is a determinism bug, which is why every decision names
//! the record that caused it (Constitution II).

use crate::{Seq, TimerToken};
use types::{ExchangeTime, InstrumentId, OrderId, Px, Qty, Side, StrategyId};

/// A decision the engine made.
///
/// Every arm carries `caused_by`: the sequence number of the inbound record
/// being processed when the decision was taken. That turns "why does this order
/// exist" into a lookup instead of an investigation, and it is what makes two
/// runs diffable at the point they first diverge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Outbound {
    /// Risk approved an intent and an order left for the venue.
    OrderSubmitted {
        /// The inbound record being processed.
        caused_by: Seq,
        /// The id the system assigned.
        order: OrderId,
        /// Who asked for it.
        strategy: StrategyId,
        /// What it trades.
        instrument: InstrumentId,
        /// Which way.
        side: Side,
        /// How much, already rounded to the venue's lot.
        qty: Qty,
        /// Market or limit, with the limit price already rounded to tick.
        kind: OrderKind,
        /// Whether this order may only decrease `|position|`.
        reduce_only: bool,
    },
    /// A cancel left for the venue.
    CancelSubmitted {
        /// The inbound record being processed.
        caused_by: Seq,
        /// The order to cancel.
        order: OrderId,
    },
    /// The risk gate refused an intent, so no order exists.
    IntentRejected {
        /// The inbound record being processed.
        caused_by: Seq,
        /// Who asked.
        strategy: StrategyId,
        /// What they wanted to trade.
        instrument: InstrumentId,
        /// Which way.
        side: Side,
        /// How much they asked for, before any rounding.
        qty: Qty,
        /// Which check refused it.
        reason: RiskReason,
    },
    /// A strategy asked to be woken at a future instant.
    ///
    /// Recorded even though replay would reproduce it anyway: the request and
    /// the firing are both inputs to the audit trail, and a request that
    /// differs between two runs localises a divergence to the strategy rather
    /// than to whatever it later ordered (`CONTEXT.md`, "Timer Event").
    TimerRequested {
        /// The inbound record being processed.
        caused_by: Seq,
        /// Who asked.
        strategy: StrategyId,
        /// Their own label for it, returned when it fires.
        token: TimerToken,
        /// When they want to be woken, on the exchange clock.
        at: ExchangeTime,
    },
    /// The engine moved between running, halted, and killed.
    StateChanged {
        /// The inbound record being processed.
        caused_by: Seq,
        /// Where it was.
        from: EngineState,
        /// Where it went.
        to: EngineState,
        /// Why.
        reason: StateReason,
    },
}

/// What a strategy wants to do, before any risk check.
///
/// It has no venue identity and no id — those exist only after the gate
/// approves it (`CONTEXT.md`). Lives here rather than in `strategy` because
/// `risk` consumes it and sits below `strategy` in the dependency order
/// (Constitution I).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Intent {
    /// Who is asking.
    pub strategy: StrategyId,
    /// What to trade.
    pub instrument: InstrumentId,
    /// Which way.
    pub side: Side,
    /// How much, in the strategy's own terms — not yet rounded to a lot.
    pub qty: Qty,
    /// Market or limit.
    pub kind: OrderKind,
    /// Whether this may only decrease `|position|`.
    ///
    /// A reduce-only intent survives checks that refuse a risk-increasing one,
    /// because permitting it is strictly less risky than blocking it
    /// (`CONTEXT.md`).
    pub reduce_only: bool,
}

/// Market or limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrderKind {
    /// Take whatever the book offers.
    ///
    /// Carries no price, so a market order cannot accidentally be read as
    /// having a meaningful one.
    Market,
    /// Rest at this price, or take through it.
    Limit(Px),
}

impl OrderKind {
    /// The limit price, if there is one.
    #[inline]
    pub const fn limit_px(self) -> Option<Px> {
        match self {
            OrderKind::Market => None,
            OrderKind::Limit(px) => Some(px),
        }
    }
}

/// Which risk check refused an intent.
///
/// Every arm here must have a test that reaches it: an error arm with zero test
/// hits is an unexploded invariant, not a defense (Constitution IV).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RiskReason {
    /// The kill switch is engaged. Refuses everything, including reduce-only.
    KillSwitchEngaged,
    /// The engine is halted and this order would increase risk.
    Halted,
    /// No instrument with this id is configured.
    UnknownInstrument,
    /// No configured limits for this instrument.
    NoLimitsConfigured,
    /// The last quote is older than the configured staleness bound.
    StaleMarketData,
    /// There is no market data for this instrument at all yet.
    NoMarketData,
    /// The resulting position would exceed the configured bound.
    PositionLimit,
    /// The resulting exposure would exceed the configured bound.
    ExposureLimit,
    /// This single order's notional exceeds the configured bound.
    OrderNotionalLimit,
    /// Too many orders in the configured window.
    OrderRateLimit,
    /// The system's position and the venue's disagree.
    UnreconciledPosition,
    /// Below the venue's minimum, or rounded to nothing by the lot size.
    BelowMinimumQty,
    /// A quantity of zero or less.
    NonPositiveQuantity,
    /// A limit price of zero or less.
    NonPositivePrice,
    /// Marked reduce-only but would not reduce the position.
    NotReducing,
    /// The order could not be sent to the venue, so none exists.
    ///
    /// Not a bound being hit but the fail-closed answer to an ambiguity: we do
    /// not know whether the venue would have taken it, and assuming either way
    /// is a silent decision about money (Constitution V).
    VenueUnreachable,
    /// A price or quantity left the representable range while being prepared.
    Unrepresentable,
}

/// Where the engine is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum EngineState {
    /// Processing normally.
    Running,
    /// Taking no new risk. Reduce-only orders still pass, so an operator can
    /// still get out of the position the halt trapped them in.
    Halted,
    /// Refusing everything. Terminal for the session: there is no transition
    /// out, by design.
    Killed,
}

impl EngineState {
    /// Whether this state permits any new order at all.
    #[inline]
    pub const fn permits_any_order(self) -> bool {
        !matches!(self, EngineState::Killed)
    }

    /// Whether this state permits an order that increases risk.
    #[inline]
    pub const fn permits_risk_increase(self) -> bool {
        matches!(self, EngineState::Running)
    }
}

/// Why the engine changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateReason {
    /// An operator asked for it.
    OperatorCommand,
    /// The system's position and the venue's disagreed.
    ReconciliationDivergence,
    /// The log refused a write, so the next decision would not be replayable.
    LogWriteFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_market_order_carries_no_price() {
        assert_eq!(OrderKind::Market.limit_px(), None);
        assert_eq!(
            OrderKind::Limit(Px::from_scaled(7)).limit_px(),
            Some(Px::from_scaled(7))
        );
    }

    #[test]
    fn a_halt_stops_new_risk_but_still_permits_getting_out() {
        assert!(EngineState::Halted.permits_any_order());
        assert!(!EngineState::Halted.permits_risk_increase());
    }

    #[test]
    fn a_kill_stops_everything() {
        assert!(!EngineState::Killed.permits_any_order());
        assert!(!EngineState::Killed.permits_risk_increase());
    }

    #[test]
    fn running_permits_both() {
        assert!(EngineState::Running.permits_any_order());
        assert!(EngineState::Running.permits_risk_increase());
    }
}
