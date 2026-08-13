//! The order state machine.
//!
//! One rule dominates the design: **a terminal state is never left.** Every
//! path that could move an order out of filled, cancelled, rejected, or expired
//! is an error rather than a transition, including the reconciliation path
//! (`CONTEXT.md`, "Terminal State").

use event::{OrderKind, RejectReason, VenueKind};
use types::{ExchangeTime, InstrumentId, Notional, OrderId, Qty, Side, StrategyId};

/// Where an order is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OrderState {
    /// Sent to the venue, not yet acknowledged.
    Pending,
    /// Live at the venue. May already be partly filled.
    Working,
    /// A cancel has been sent and not yet confirmed.
    PendingCancel,
    /// Fully executed. Terminal.
    Filled,
    /// Cancelled, possibly after partial execution. Terminal.
    Cancelled,
    /// The venue never made it live. Terminal.
    Rejected,
    /// Reached its expiry without fully executing. Terminal.
    Expired,
}

impl OrderState {
    /// Whether no further transition is possible.
    #[inline]
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderState::Filled | OrderState::Cancelled | OrderState::Rejected | OrderState::Expired
        )
    }

    /// Whether the venue could still execute this order.
    #[inline]
    pub const fn is_live(self) -> bool {
        !self.is_terminal()
    }
}

/// What the order machine refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OmsError {
    /// No order with that id in this session.
    UnknownOrder,
    /// A report arrived for an order that is already terminal.
    AlreadyTerminal,
    /// The venue reported more executed than was ordered.
    Overfill,
    /// The report does not fit the order's current state.
    InvalidTransition,
    /// A quantity or price left the representable range.
    Overflow,
    /// The order store has no room for another order.
    CapacityExceeded,
}

/// A risk-approved instruction, tracked until terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Order {
    id: OrderId,
    strategy: StrategyId,
    instrument: InstrumentId,
    side: Side,
    qty: Qty,
    kind: OrderKind,
    reduce_only: bool,
    state: OrderState,
    filled: Qty,
    fees: Notional,
    reject_reason: Option<RejectReason>,
    submitted_at: ExchangeTime,
    updated_at: ExchangeTime,
}

impl Order {
    /// A newly submitted order, awaiting acknowledgement.
    #[allow(clippy::too_many_arguments)]
    pub const fn new(
        id: OrderId,
        strategy: StrategyId,
        instrument: InstrumentId,
        side: Side,
        qty: Qty,
        kind: OrderKind,
        reduce_only: bool,
        at: ExchangeTime,
    ) -> Order {
        Order {
            id,
            strategy,
            instrument,
            side,
            qty,
            kind,
            reduce_only,
            state: OrderState::Pending,
            filled: Qty::ZERO,
            fees: Notional::ZERO,
            reject_reason: None,
            submitted_at: at,
            updated_at: at,
        }
    }

    /// The system-assigned id.
    #[inline]
    pub const fn id(&self) -> OrderId {
        self.id
    }

    /// Who asked for it.
    #[inline]
    pub const fn strategy(&self) -> StrategyId {
        self.strategy
    }

    /// What it trades.
    #[inline]
    pub const fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// Which way.
    #[inline]
    pub const fn side(&self) -> Side {
        self.side
    }

    /// The full quantity ordered.
    #[inline]
    pub const fn qty(&self) -> Qty {
        self.qty
    }

    /// Market or limit.
    #[inline]
    pub const fn kind(&self) -> OrderKind {
        self.kind
    }

    /// Whether this order may only decrease `|position|`.
    #[inline]
    pub const fn reduce_only(&self) -> bool {
        self.reduce_only
    }

    /// Where it is in its life.
    #[inline]
    pub const fn state(&self) -> OrderState {
        self.state
    }

    /// How much has executed so far.
    #[inline]
    pub const fn filled(&self) -> Qty {
        self.filled
    }

    /// Fees charged so far.
    #[inline]
    pub const fn fees(&self) -> Notional {
        self.fees
    }

    /// Why the venue refused it, if it did.
    #[inline]
    pub const fn reject_reason(&self) -> Option<RejectReason> {
        self.reject_reason
    }

    /// When it was submitted.
    #[inline]
    pub const fn submitted_at(&self) -> ExchangeTime {
        self.submitted_at
    }

    /// When it last changed.
    #[inline]
    pub const fn updated_at(&self) -> ExchangeTime {
        self.updated_at
    }

    /// How much is still outstanding.
    #[inline]
    pub fn remaining(&self) -> Qty {
        Qty::from_scaled(self.qty.to_scaled() - self.filled.to_scaled())
    }

    /// Records that a cancel has been sent.
    ///
    /// Refused for a terminal order: cancelling something already finished is a
    /// caller bug, and letting it through would put a terminal order back into
    /// a live state.
    pub fn request_cancel(&mut self, at: ExchangeTime) -> Result<(), OmsError> {
        match self.state {
            OrderState::Pending | OrderState::Working => {
                self.state = OrderState::PendingCancel;
                self.updated_at = at;
                Ok(())
            }
            // Already asked. Asking twice is harmless and common.
            OrderState::PendingCancel => Ok(()),
            _ => Err(OmsError::AlreadyTerminal),
        }
    }

    /// Applies a venue report.
    ///
    /// Late reports about a terminal order are not all equal. A cancel
    /// confirmation or cancel rejection arriving after the order finished is an
    /// ordinary race — we asked, and it filled or died while the request was in
    /// flight — so those are accepted and ignored. Anything else arriving after
    /// terminal means the venue and this process disagree about the order's
    /// life, which is a condition to surface rather than absorb.
    pub fn apply(&mut self, kind: VenueKind, at: ExchangeTime) -> Result<(), OmsError> {
        if self.state.is_terminal() {
            return match kind {
                VenueKind::Cancelled | VenueKind::CancelRejected { .. } => Ok(()),
                _ => Err(OmsError::AlreadyTerminal),
            };
        }

        match kind {
            VenueKind::Accepted => match self.state {
                OrderState::Pending => {
                    self.state = OrderState::Working;
                }
                // A duplicate ack, or one that raced our cancel. Neither
                // changes what the order is.
                OrderState::Working | OrderState::PendingCancel => {}
                _ => return Err(OmsError::InvalidTransition),
            },

            VenueKind::Filled { qty, fee, .. } => {
                if qty.to_scaled() <= 0 {
                    return Err(OmsError::InvalidTransition);
                }
                // Compute before committing: an overfill must leave the order
                // exactly as it was, so the state that produced it is visible.
                let filled = qty
                    .checked_add(self.filled)
                    .map_err(|_| OmsError::Overflow)?;
                if filled.to_scaled() > self.qty.to_scaled() {
                    return Err(OmsError::Overfill);
                }
                let fees = self.fees.checked_add(fee).map_err(|_| OmsError::Overflow)?;
                self.filled = filled;
                self.fees = fees;
                if filled == self.qty {
                    self.state = OrderState::Filled;
                } else if self.state == OrderState::Pending {
                    // An execution proves the venue made it live, whether or
                    // not a separate acknowledgement ever arrives.
                    self.state = OrderState::Working;
                }
            }

            VenueKind::Cancelled => {
                self.state = OrderState::Cancelled;
            }

            VenueKind::CancelRejected { .. } => {
                // The order is still live; put it back where it was.
                if self.state == OrderState::PendingCancel {
                    self.state = OrderState::Working;
                }
            }

            VenueKind::Rejected { reason } => {
                self.reject_reason = Some(reason);
                self.state = OrderState::Rejected;
            }

            VenueKind::Expired => {
                self.state = OrderState::Expired;
            }
        }

        self.updated_at = at;
        Ok(())
    }
}

/// Every order in the session, addressed by id.
///
/// Ids are a dense monotonic run from a known base, so the lookup is
/// `id − base` into a slice rather than a hash. That keeps order lookup off the
/// allocator and out of `HashMap` iteration order (Constitution II, VI).
#[derive(Debug, Clone)]
pub struct Orders {
    base: OrderId,
    orders: Vec<Order>,
}

impl Orders {
    /// An empty store issuing ids from `base`, with room for `capacity` orders.
    pub fn new(base: OrderId, capacity: usize) -> Orders {
        Orders {
            base,
            orders: Vec::with_capacity(capacity),
        }
    }

    /// The id the next submitted order will receive.
    ///
    /// Derived from the count, never from a clock or a random source: replaying
    /// a session must reproduce the same ids in the same order
    /// (Constitution II).
    #[inline]
    pub fn next_id(&self) -> OrderId {
        OrderId::new(self.base.raw() + self.orders.len() as u64)
    }

    /// How many orders the session has created.
    #[inline]
    pub fn len(&self) -> usize {
        self.orders.len()
    }

    /// Whether no order has been created.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.orders.is_empty()
    }

    /// Whether the next submission would grow the store, and so allocate.
    #[inline]
    pub fn would_grow(&self) -> bool {
        self.orders.len() == self.orders.capacity()
    }

    /// Adds an order, which must carry the id [`next_id`] handed out.
    ///
    /// [`next_id`]: Orders::next_id
    pub fn insert(&mut self, order: Order) -> Result<OrderId, OmsError> {
        if order.id() != self.next_id() {
            return Err(OmsError::InvalidTransition);
        }
        let id = order.id();
        self.orders.push(order);
        Ok(id)
    }

    /// One order.
    #[inline]
    pub fn get(&self, id: OrderId) -> Option<&Order> {
        self.orders.get(self.index_of(id)?)
    }

    /// One order, mutably.
    #[inline]
    pub fn get_mut(&mut self, id: OrderId) -> Option<&mut Order> {
        let index = self.index_of(id)?;
        self.orders.get_mut(index)
    }

    /// Every order, in creation order.
    pub fn iter(&self) -> impl Iterator<Item = &Order> {
        self.orders.iter()
    }

    /// Every order the venue could still execute, in creation order.
    pub fn live(&self) -> impl Iterator<Item = &Order> {
        self.orders.iter().filter(|o| o.state().is_live())
    }

    /// Applies a venue report to the order it names.
    pub fn apply(
        &mut self,
        id: OrderId,
        kind: VenueKind,
        at: ExchangeTime,
    ) -> Result<(), OmsError> {
        self.get_mut(id)
            .ok_or(OmsError::UnknownOrder)?
            .apply(kind, at)
    }

    fn index_of(&self, id: OrderId) -> Option<usize> {
        id.raw().checked_sub(self.base.raw()).map(|i| i as usize)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::{Px, Timestamp};

    const fn qty(units: i64) -> Qty {
        Qty::from_scaled(units)
    }

    fn at(n: i64) -> ExchangeTime {
        Timestamp::from_nanos(n)
    }

    fn order() -> Order {
        Order::new(
            OrderId::new(0),
            StrategyId::new(0),
            InstrumentId::new(0),
            Side::Buy,
            qty(10),
            OrderKind::Market,
            false,
            at(1),
        )
    }

    fn fill(n: i64) -> VenueKind {
        VenueKind::Filled {
            px: Px::from_scaled(100),
            qty: qty(n),
            fee: Notional::ZERO,
        }
    }

    #[test]
    fn a_new_order_is_pending_with_nothing_filled() {
        let o = order();
        assert_eq!(o.state(), OrderState::Pending);
        assert_eq!(o.filled(), Qty::ZERO);
        assert_eq!(o.remaining(), qty(10));
        assert!(o.state().is_live());
    }

    #[test]
    fn acknowledgement_makes_an_order_working() {
        let mut o = order();
        o.apply(VenueKind::Accepted, at(2)).expect("ack");
        assert_eq!(o.state(), OrderState::Working);
        assert_eq!(o.updated_at(), at(2));
    }

    #[test]
    fn a_duplicate_acknowledgement_changes_nothing() {
        let mut o = order();
        o.apply(VenueKind::Accepted, at(2)).expect("ack");
        o.apply(VenueKind::Accepted, at(3)).expect("duplicate ack");
        assert_eq!(o.state(), OrderState::Working);
    }

    #[test]
    fn partial_fills_accumulate_and_the_order_stays_live() {
        let mut o = order();
        o.apply(VenueKind::Accepted, at(2)).expect("ack");
        o.apply(fill(3), at(3)).expect("partial");
        assert_eq!(o.filled(), qty(3));
        assert_eq!(o.remaining(), qty(7));
        assert_eq!(o.state(), OrderState::Working);
    }

    #[test]
    fn the_fill_that_completes_the_quantity_makes_the_order_terminal() {
        let mut o = order();
        o.apply(fill(4), at(2)).expect("partial");
        o.apply(fill(6), at(3)).expect("rest");
        assert_eq!(o.state(), OrderState::Filled);
        assert!(o.state().is_terminal());
    }

    #[test]
    fn a_venue_reporting_more_than_was_ordered_is_refused_and_changes_nothing() {
        let mut o = order();
        o.apply(fill(6), at(2)).expect("partial");
        assert_eq!(o.apply(fill(5), at(3)), Err(OmsError::Overfill));
        // The refused report left the order exactly as it was.
        assert_eq!(o.filled(), qty(6));
        assert_eq!(o.state(), OrderState::Working);
    }

    #[test]
    fn a_fill_on_an_unacknowledged_order_makes_it_working() {
        let mut o = order();
        o.apply(fill(1), at(2)).expect("fill without an ack");
        assert_eq!(o.state(), OrderState::Working);
    }

    #[test]
    fn a_zero_or_negative_fill_is_refused() {
        let mut o = order();
        assert_eq!(o.apply(fill(0), at(2)), Err(OmsError::InvalidTransition));
        assert_eq!(o.apply(fill(-1), at(2)), Err(OmsError::InvalidTransition));
    }

    #[test]
    fn a_cancel_after_a_partial_fill_keeps_what_was_filled() {
        let mut o = order();
        o.apply(fill(3), at(2)).expect("partial");
        o.apply(VenueKind::Cancelled, at(3)).expect("cancel");
        assert_eq!(o.state(), OrderState::Cancelled);
        assert_eq!(o.filled(), qty(3));
    }

    #[test]
    fn a_terminal_order_is_never_moved_out_of_its_terminal_state() {
        let mut o = order();
        o.apply(fill(10), at(2)).expect("full fill");
        assert_eq!(o.state(), OrderState::Filled);
        assert_eq!(o.apply(fill(1), at(3)), Err(OmsError::AlreadyTerminal));
        assert_eq!(
            o.apply(VenueKind::Accepted, at(3)),
            Err(OmsError::AlreadyTerminal)
        );
        assert_eq!(o.state(), OrderState::Filled);
    }

    #[test]
    fn a_cancel_confirmation_that_lost_a_race_with_a_fill_is_not_an_error() {
        // We asked to cancel; it filled first; the confirmation arrives late.
        // That is an ordinary race, not a disagreement about the order.
        let mut o = order();
        o.request_cancel(at(2)).expect("request");
        o.apply(fill(10), at(3)).expect("filled first");
        assert_eq!(o.state(), OrderState::Filled);
        o.apply(VenueKind::Cancelled, at(4)).expect("late confirm");
        o.apply(
            VenueKind::CancelRejected {
                reason: RejectReason::UnknownOrder,
            },
            at(5),
        )
        .expect("late reject");
        assert_eq!(o.state(), OrderState::Filled);
    }

    #[test]
    fn a_rejected_cancel_puts_the_order_back_to_working() {
        let mut o = order();
        o.apply(VenueKind::Accepted, at(2)).expect("ack");
        o.request_cancel(at(3)).expect("request");
        assert_eq!(o.state(), OrderState::PendingCancel);
        o.apply(
            VenueKind::CancelRejected {
                reason: RejectReason::AlreadyTerminal,
            },
            at(4),
        )
        .expect("cancel rejected");
        assert_eq!(o.state(), OrderState::Working);
    }

    #[test]
    fn a_rejection_records_why() {
        let mut o = order();
        o.apply(
            VenueKind::Rejected {
                reason: RejectReason::InsufficientFunds,
            },
            at(2),
        )
        .expect("reject");
        assert_eq!(o.state(), OrderState::Rejected);
        assert_eq!(o.reject_reason(), Some(RejectReason::InsufficientFunds));
    }

    #[test]
    fn an_expiry_is_terminal() {
        let mut o = order();
        o.apply(VenueKind::Expired, at(2)).expect("expire");
        assert!(o.state().is_terminal());
    }

    #[test]
    fn cancelling_a_terminal_order_is_refused() {
        let mut o = order();
        o.apply(fill(10), at(2)).expect("fill");
        assert_eq!(o.request_cancel(at(3)), Err(OmsError::AlreadyTerminal));
    }

    #[test]
    fn ids_are_handed_out_in_order_from_the_base() {
        let mut orders = Orders::new(OrderId::new(100), 4);
        assert_eq!(orders.next_id(), OrderId::new(100));
        let mut first = order();
        first.id = OrderId::new(100);
        orders.insert(first).expect("insert");
        assert_eq!(orders.next_id(), OrderId::new(101));
    }

    #[test]
    fn an_order_with_the_wrong_id_is_refused_rather_than_stored_out_of_place() {
        let mut orders = Orders::new(OrderId::new(100), 4);
        let mut wrong = order();
        wrong.id = OrderId::new(500);
        assert_eq!(orders.insert(wrong), Err(OmsError::InvalidTransition));
    }

    #[test]
    fn an_unknown_order_id_is_reported_rather_than_ignored() {
        let mut orders = Orders::new(OrderId::new(100), 4);
        assert_eq!(
            orders.apply(OrderId::new(7), VenueKind::Accepted, at(1)),
            Err(OmsError::UnknownOrder)
        );
        assert_eq!(
            orders.apply(OrderId::new(100), VenueKind::Accepted, at(1)),
            Err(OmsError::UnknownOrder)
        );
    }

    #[test]
    fn live_orders_exclude_terminal_ones_and_stay_in_creation_order() {
        let mut orders = Orders::new(OrderId::new(0), 4);
        for n in 0..3u64 {
            let mut o = order();
            o.id = OrderId::new(n);
            orders.insert(o).expect("insert");
        }
        orders
            .apply(OrderId::new(1), VenueKind::Cancelled, at(2))
            .expect("cancel");
        let live: Vec<OrderId> = orders.live().map(|o| o.id()).collect();
        assert_eq!(live, vec![OrderId::new(0), OrderId::new(2)]);
    }
}
