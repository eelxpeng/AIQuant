//! Identities. Small, copyable, and totally ordered, so that every map keyed by
//! one of them iterates in a deterministic order (Constitution II).

use core::fmt;

/// Identifies one tradable contract at one venue, stable across sessions.
///
/// The wrapped `u32` is a **dense index**, not an opaque handle: the engine
/// stores per-instrument state in a slice and looks it up by [`index`]. That
/// makes the lookup a bounds check rather than a hash, which is what lets the
/// hot path avoid both allocation and `HashMap` iteration order
/// (Constitution II, VI).
///
/// [`index`]: InstrumentId::index
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct InstrumentId(u32);

impl InstrumentId {
    /// Builds an id from its dense index.
    #[inline]
    pub const fn new(index: u32) -> InstrumentId {
        InstrumentId(index)
    }

    /// The index into per-instrument storage.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The underlying number, for logging and record encoding.
    #[inline]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for InstrumentId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "i{}", self.0)
    }
}

/// A system-assigned order identity, unique within a session.
///
/// Assigned by `oms` from a counter that starts at a value recorded in the log,
/// never from a clock or a random source — replaying a session must reproduce
/// the same ids in the same order (Constitution II).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OrderId(u64);

impl OrderId {
    /// Builds an id from its raw number.
    #[inline]
    pub const fn new(raw: u64) -> OrderId {
        OrderId(raw)
    }

    /// The underlying number, for logging and record encoding.
    #[inline]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The next id in sequence.
    ///
    /// Saturating rather than wrapping: a wrapped order id would alias a live
    /// order, and silently addressing the wrong order is worse than refusing to
    /// issue a new one. `u64` exhaustion is unreachable in practice, so the
    /// saturated value exists to be provably harmless rather than to be used.
    #[inline]
    pub const fn next(self) -> OrderId {
        OrderId(self.0.saturating_add(1))
    }
}

impl fmt::Display for OrderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "o{}", self.0)
    }
}

/// Identifies one strategy instance within a session.
///
/// Every intent carries one, so that an order in the log can be attributed to
/// the strategy that asked for it without a second lookup path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StrategyId(u16);

impl StrategyId {
    /// Builds an id from its dense index.
    #[inline]
    pub const fn new(index: u16) -> StrategyId {
        StrategyId(index)
    }

    /// The index into per-strategy storage.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The underlying number, for logging and record encoding.
    #[inline]
    pub const fn raw(self) -> u16 {
        self.0
    }
}

impl fmt::Display for StrategyId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_instrument_id_indexes_dense_storage() {
        assert_eq!(InstrumentId::new(7).index(), 7);
        assert_eq!(InstrumentId::new(7).raw(), 7);
    }

    #[test]
    fn order_ids_advance_by_one() {
        assert_eq!(OrderId::new(41).next(), OrderId::new(42));
    }

    #[test]
    fn an_order_id_saturates_rather_than_aliasing_a_live_order() {
        let last = OrderId::new(u64::MAX);
        assert_eq!(last.next(), last);
    }

    #[test]
    fn ids_order_by_their_number() {
        assert!(OrderId::new(1) < OrderId::new(2));
        assert!(InstrumentId::new(1) < InstrumentId::new(2));
        assert!(StrategyId::new(1) < StrategyId::new(2));
    }
}
