//! What an engine needs before it can run.

use marketdata::MarkRule;
use risk::LimitBook;
use types::{ExchangeTime, Instrument, OrderId, RoundDir};

/// How much room to reserve up front.
///
/// Every one of these exists so the hot path does not allocate
/// (Constitution VI). They are reservations, not hard caps: exceeding one costs
/// an allocation rather than an error, and [`Engine::would_allocate`] reports
/// when that is about to happen.
///
/// [`Engine::would_allocate`]: crate::Engine::would_allocate
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Capacity {
    /// Records the session's log is sized for.
    pub log: usize,
    /// Orders the session is sized for.
    pub orders: usize,
    /// Open lots per instrument.
    pub lots_per_instrument: usize,
    /// Intents, bars, and venue reports one event may produce.
    pub per_step: usize,
}

impl Default for Capacity {
    fn default() -> Capacity {
        Capacity {
            log: 1 << 16,
            orders: 1 << 12,
            lots_per_instrument: 64,
            per_step: 64,
        }
    }
}

/// Everything an engine is configured with.
///
/// Note what is *not* here: no mode flag. Backtest, paper, and live differ only
/// in which feed and venue adapters the caller binds, so there is no field for
/// a strategy to read and no branch for one to take (Constitution III).
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Every tradable instrument, in id order.
    pub instruments: Vec<Instrument>,
    /// The bounds the gate enforces.
    pub limits: LimitBook,
    /// The rule that picks a valuation price.
    pub mark_rule: MarkRule,
    /// The id the first order of the session receives.
    ///
    /// Explicit rather than zero so that a session resumed from a log can
    /// continue the run without reissuing ids that already exist.
    pub first_order_id: OrderId,
    /// The exchange time the session starts at.
    ///
    /// Used only until the first event carrying one arrives. Operator commands
    /// have no exchange time of their own, so one arriving before any market
    /// data is stamped with this.
    pub session_start: ExchangeTime,
    /// How much room to reserve.
    pub capacity: Capacity,
}

impl EngineConfig {
    /// A configuration with default capacities and a mid-price mark.
    pub fn new(
        instruments: Vec<Instrument>,
        limits: LimitBook,
        first_order_id: OrderId,
        session_start: ExchangeTime,
    ) -> EngineConfig {
        EngineConfig {
            instruments,
            limits,
            // Rounding down rather than to nearest: a mark that never rounds
            // up cannot flatter an exposure check.
            mark_rule: MarkRule::Mid(RoundDir::Down),
            first_order_id,
            session_start,
            capacity: Capacity::default(),
        }
    }
}
