//! A two-sided quoter: rest a bid and an ask, and pull them when they go
//! stale.
//!
//! The second strategy in the workspace, and it exists to be a *different
//! shape* rather than different arithmetic. The crossover takes liquidity with
//! market orders that are live and gone inside one event; this one posts
//! limit orders that rest, and has to manage them afterwards. Between them
//! they exercise both halves of the order lifecycle.
//!
//! Everything under it — resting orders at the venue, limit prices through the
//! risk gate, `CancelSubmitted` in the log, the order state machine's
//! `PendingCancel` — was already built and had no production caller. This is
//! that caller.
//!
//! # What it does
//!
//! On each quote it works out where it *wants* to be: `mid ± half_spread`. If
//! it has nothing resting on a side it posts. If what is resting has drifted
//! further than `reprice` from where it now wants to be, it cancels — and
//! posts again only once the venue confirms the order is gone.
//!
//! # One order per side, and why
//!
//! It never posts a replacement before the old order is confirmed dead. The
//! alternative — cancel and immediately re-post — is briefly quoting twice on
//! one side, and if the market takes both the strategy holds double the size
//! it ever intended. Quoting is a business of being repeatedly slightly wrong,
//! and doubling up in exactly the moment the market is moving is the wrong way
//! to be wrong.
//!
//! The replacement goes out on the **next** quote, not in the event that
//! cancelled. Reposting inside that event would mean pricing against a
//! midpoint the strategy has just decided is stale, which is the reason it
//! cancelled in the first place.
//!
//! The cost is real and stated: after a reprice this is out of the market
//! until the next book update.
//!
//! # Inventory
//!
//! It stops quoting the side that would make its position bigger once
//! `max_inventory` is reached, so it leans back towards flat instead of
//! accumulating. This is the strategy's own opinion and is not a risk control:
//! the gate is the risk control, it does not trust this, and a strategy that
//! forgot this check would be refused rather than obeyed (Constitution V).

use event::OrderKind;
use marketdata::TopOfBook;
use types::{InstrumentId, OrderId, Px, Qty, RoundDir, Side, StrategyId};

use crate::{Context, Strategy, StrategyEvent};

/// What this strategy has working on one side.
///
/// The states are the ones the venue can actually put it in. `Pending` exists
/// because an order has no id until the venue accepts it, so there is a window
/// where something is out there that cannot yet be named or cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Working {
    /// Nothing out.
    Idle,
    /// Asked for, no id yet.
    Pending,
    /// Resting, and nameable.
    Live {
        /// Its id.
        order: OrderId,
        /// Where it rests.
        px: Px,
    },
    /// A cancel is in flight; nothing new goes out until it lands.
    Cancelling,
}

/// Rests a bid and an ask around the midpoint.
#[derive(Debug)]
pub struct Quoter {
    id: StrategyId,
    instrument: InstrumentId,
    /// How far either side of the midpoint to rest.
    half_spread: Px,
    /// Size on each side.
    size: Qty,
    /// How far the wanted price must move before a resting order is pulled.
    ///
    /// Without it, a one-tick flicker would cancel and re-post forever and the
    /// strategy would spend the session out of the market waiting for its own
    /// round trips.
    reprice: Px,
    /// The position beyond which it stops adding on that side.
    max_inventory: Qty,

    position: i64,
    bid: Working,
    ask: Working,
}

impl Quoter {
    /// Builds a quoter.
    pub fn new(
        id: StrategyId,
        instrument: InstrumentId,
        half_spread: Px,
        size: Qty,
        reprice: Px,
        max_inventory: Qty,
    ) -> Quoter {
        Quoter {
            id,
            instrument,
            half_spread,
            size,
            reprice,
            max_inventory,
            position: 0,
            bid: Working::Idle,
            ask: Working::Idle,
        }
    }

    /// What this strategy believes it holds.
    ///
    /// Its own tally, not the books. `oms` is authoritative; this exists so the
    /// inventory check has something to read without reaching outside the
    /// strategy interface.
    pub const fn position(&self) -> Qty {
        Qty::from_scaled(self.position)
    }

    /// Where it wants to rest on a side, given a midpoint.
    ///
    /// `None` when the price would be non-positive, which the gate would refuse
    /// anyway — better to not ask than to be told no every quote.
    fn wanted(&self, mid: Px, side: Side) -> Option<Px> {
        let px = match side {
            Side::Buy => mid.checked_sub(self.half_spread),
            Side::Sell => mid.checked_add(self.half_spread),
        }
        .ok()?;
        (px.to_scaled() > 0).then_some(px)
    }

    /// Whether adding on this side would push inventory past the cap.
    fn at_cap(&self, side: Side) -> bool {
        let after = self.position as i128 + side.sign() as i128 * self.size.to_scaled() as i128;
        after.abs() > self.max_inventory.to_scaled() as i128
    }

    fn working(&self, side: Side) -> Working {
        match side {
            Side::Buy => self.bid,
            Side::Sell => self.ask,
        }
    }

    fn set(&mut self, side: Side, state: Working) {
        match side {
            Side::Buy => self.bid = state,
            Side::Sell => self.ask = state,
        }
    }

    /// Brings one side into line with where it now wants to be.
    fn adjust(&mut self, side: Side, mid: Px, ctx: &mut Context<'_>) {
        let Some(wanted) = self.wanted(mid, side) else {
            return;
        };

        match self.working(side) {
            // Nothing out. Post, unless that would overfill the inventory.
            Working::Idle => {
                if self.at_cap(side) {
                    return;
                }
                ctx.order(self.instrument, side, self.size, OrderKind::Limit(wanted));
                self.set(side, Working::Pending);
            }

            // Something is resting. Pull it only if it has drifted far enough
            // to be worth a round trip out of the market.
            Working::Live { order, px } => {
                let drift = (wanted.to_scaled() - px.to_scaled()).abs();
                if drift >= self.reprice.to_scaled() {
                    ctx.cancel(order);
                    self.set(side, Working::Cancelling);
                }
            }

            // In flight either way: nothing to do until the venue answers.
            Working::Pending | Working::Cancelling => {}
        }
    }
}

impl Strategy for Quoter {
    fn id(&self) -> StrategyId {
        self.id
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
        match *event {
            StrategyEvent::Quote { instrument, top } => {
                if instrument != self.instrument {
                    return;
                }
                // A one-sided or crossed book has no honest midpoint, and
                // quoting around a made-up one is quoting at a price the market
                // never showed.
                let Some(mid) = midpoint(top) else {
                    return;
                };
                self.adjust(Side::Buy, mid, ctx);
                self.adjust(Side::Sell, mid, ctx);
            }

            // The order is nameable now, so remember what to cancel later.
            StrategyEvent::OrderLive {
                instrument,
                side,
                kind,
                order,
                ..
            } => {
                if instrument != self.instrument {
                    return;
                }
                if let OrderKind::Limit(px) = kind
                    && self.working(side) == Working::Pending
                {
                    self.set(side, Working::Live { order, px });
                }
            }

            StrategyEvent::Fill {
                instrument,
                side,
                qty,
                ..
            } => {
                if instrument != self.instrument {
                    return;
                }
                self.position = self
                    .position
                    .saturating_add(side.sign() * qty.to_scaled());
            }

            // Gone, however it went. Free the side so the next quote reposts.
            StrategyEvent::OrderDone {
                instrument, side, ..
            } => {
                if instrument != self.instrument {
                    return;
                }
                self.set(side, Working::Idle);
            }

            // No order exists, so the side is free again. Without this the
            // strategy would sit in `Pending` for ever and stop quoting that
            // side for the rest of the session.
            StrategyEvent::IntentRefused {
                instrument, side, ..
            } => {
                if instrument != self.instrument {
                    return;
                }
                if self.working(side) == Working::Pending {
                    self.set(side, Working::Idle);
                }
            }

            StrategyEvent::Trade { .. }
            | StrategyEvent::Bar { .. }
            | StrategyEvent::Timer { .. } => {}
        }
    }
}

/// The midpoint a quote is built around.
///
/// Rounded down, so the pair of quotes this produces is never wider on the buy
/// side than the arithmetic midpoint would justify — the direction that cannot
/// flatter a fill.
fn midpoint(top: &TopOfBook) -> Option<Px> {
    top.mid(RoundDir::Down)
}
