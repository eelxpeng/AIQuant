//! A deterministic simulated venue.
//!
//! This is an **adapter, not test scaffolding**. Every backtest runs through
//! it, so it carries a real workload and belongs beside the venue adapters it
//! mirrors (`docs/ARCHITECTURE.md`). `simkit` is what remains: scripted feeds
//! and fixtures you would never trade on.
//!
//! > **Known hazard.** Sharing this between backtest and unit tests means
//! > someone tweaking it to make a test pass can silently move every backtest
//! > result. The mitigation is that [`FillModel`] states its assumptions and
//! > has its own tests. If that discipline slips, this is where it costs you.
//!
//! Nothing here reads a clock. Venue timestamps are derived from the market
//! data the venue has observed, plus a configured latency, so two runs over the
//! same input produce the same reports at the same times (Constitution II).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use event::{Inbound, MarketEvent, MarketKind, OrderKind, RejectReason, VenueEvent, VenueKind};
use marketdata::{Applied, Books, TopOfBook};
use oms::{Order, VenueAdapter, VenueError};
use types::{
    ExchangeSpan, ExchangeTime, InstrumentId, Notional, OrderId, Px, Qty, ReceiveTime, RoundDir,
    Side, Timestamp,
};

/// How the simulated venue decides whether and at what price an order executes.
///
/// Both models fill at the **touch price**, not at the order's limit price, so
/// a marketable limit order gets the price improvement a real venue would give
/// it. They differ only in how much size the touch is assumed to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FillModel {
    /// A marketable order fills in full at the opposite touch.
    ///
    /// **Assumption: unlimited depth at the touch and no market impact.** This
    /// is optimistic and gets more optimistic as order size grows. Use it when
    /// order sizes are small against displayed size, and do not use it to
    /// justify a strategy that trades large.
    TouchUnlimited,
    /// A marketable order fills only up to the size displayed at the touch.
    ///
    /// **Assumptions**: the displayed size is really available; we are at the
    /// front of the queue for resting orders; and a resting order fills when a
    /// trade prints at or through its price, for up to that print's quantity.
    /// Being at the front of the queue is the optimistic part — a real queue
    /// position would fill less and later.
    TouchDisplayed,
}

/// Fees, expressed as money per unit traded.
///
/// Per unit rather than in basis points because a basis-point fee needs a
/// division and this crate has no rounding budget to spend on one. A
/// proportional fee schedule is deferred (#1).
/// No `Default`: `Px` has none, deliberately, and a fee schedule that
/// defaulted to zero is exactly the kind of silent assumption that makes a
/// backtest lie. Say [`Fees::NONE`] and mean it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fees {
    /// Charged when an order provides liquidity.
    pub maker: Px,
    /// Charged when an order takes liquidity.
    pub taker: Px,
}

impl Fees {
    /// No fees at all.
    pub const NONE: Fees = Fees {
        maker: Px::ZERO,
        taker: Px::ZERO,
    };
}

/// Fees round **up**: a simulated fill never costs less than it would in life.
const FEE_ROUNDING: RoundDir = RoundDir::Up;

/// An order the venue is holding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Resting {
    id: OrderId,
    instrument: InstrumentId,
    side: Side,
    limit: Px,
    remaining: Qty,
}

/// A simulated venue with an explicit fill model.
#[derive(Debug, Clone)]
pub struct SimVenue {
    model: FillModel,
    fees: Fees,
    latency: ExchangeSpan,
    books: Books,
    resting: Vec<Resting>,
    pending: Vec<Inbound>,
    now: ExchangeTime,
    connected: bool,
}

impl SimVenue {
    /// Builds a venue for `instruments` instruments.
    ///
    /// `latency` is added to every report's timestamp. Constant rather than
    /// modelled: a distribution would need a random source, and an ambient one
    /// would break replay (Constitution II). A latency *model* is a separate
    /// component with its own assumptions (#1).
    pub fn new(
        instruments: usize,
        model: FillModel,
        fees: Fees,
        latency: ExchangeSpan,
    ) -> SimVenue {
        SimVenue {
            model,
            fees,
            latency,
            books: Books::with_instruments(instruments),
            resting: Vec::with_capacity(256),
            pending: Vec::with_capacity(256),
            now: Timestamp::from_nanos(0),
            connected: true,
        }
    }

    /// Feeds the venue the same market data the engine sees.
    ///
    /// The venue needs its own view of the book to fill against. The engine
    /// hands it the same events through [`VenueAdapter::observe_market`], which
    /// is what keeps the two views identical without either reaching into the
    /// other.
    fn observe(&mut self, market: &MarketEvent) {
        // Refuse whatever the engine's book would refuse, on the same rule.
        // The engine already filters these out before calling, so this is a
        // second line of defence rather than the only one — but a venue filling
        // against a book the engine had rejected would diverge from it
        // silently, and that divergence would show up as a fill nobody can
        // explain.
        if self.books.apply(market) != Applied::Accepted {
            return;
        }
        self.now = market.exchange_time;

        if let MarketKind::Trade { px, qty, .. } = market.kind {
            self.fill_resting(market.instrument, px, qty);
        } else {
            self.sweep_marketable(market.instrument);
        }
    }

    /// Whether the venue is reachable.
    #[inline]
    pub const fn is_connected(&self) -> bool {
        self.connected
    }

    /// Takes the venue offline, so every request fails.
    ///
    /// Exists so a test can reach the disconnected branch of the fail-closed
    /// path: an error arm no test reaches is an unexploded invariant
    /// (Constitution IV).
    pub fn disconnect(&mut self) {
        self.connected = false;
    }

    /// Brings the venue back.
    pub fn reconnect(&mut self) {
        self.connected = true;
    }

    /// How many orders are resting.
    #[inline]
    pub fn resting_count(&self) -> usize {
        self.resting.len()
    }

    fn report(&mut self, order: OrderId, kind: VenueKind) {
        let venue_time = self.now;
        // The report is observed one latency later than the venue stamped it.
        // Two different clocks, and never subtracted from each other.
        let receive_time: ReceiveTime = Timestamp::from_nanos(
            venue_time
                .to_nanos()
                .saturating_add(latency_nanos(self.latency)),
        );
        self.pending.push(Inbound::Venue(VenueEvent {
            order,
            venue_time,
            receive_time,
            kind,
        }));
    }

    /// The size a taker on `side` may lift, under the configured model.
    fn available(&self, top: &TopOfBook, side: Side, want: Qty) -> Qty {
        match self.model {
            FillModel::TouchUnlimited => want,
            FillModel::TouchDisplayed => {
                let displayed = top.taker_qty(side);
                if displayed.to_scaled() < want.to_scaled() {
                    displayed
                } else {
                    want
                }
            }
        }
    }

    /// Whether a limit price would cross the touch on this side.
    fn is_marketable(top: &TopOfBook, side: Side, kind: OrderKind) -> bool {
        match kind {
            OrderKind::Market => true,
            OrderKind::Limit(limit) => match side {
                Side::Buy => limit.to_scaled() >= top.ask_px.to_scaled(),
                Side::Sell => limit.to_scaled() <= top.bid_px.to_scaled(),
            },
        }
    }

    fn fee_for(&self, taking: bool, qty: Qty) -> Notional {
        let rate = if taking {
            self.fees.taker
        } else {
            self.fees.maker
        };
        rate.notional(qty, FEE_ROUNDING)
    }

    /// Executes whatever an incoming order can take right now, and returns what
    /// is left over.
    fn take_liquidity(&mut self, order: &Order) -> Qty {
        let Some(top) = self.books.top(order.instrument()).copied() else {
            return order.qty();
        };
        if !top.is_two_sided() || !Self::is_marketable(&top, order.side(), order.kind()) {
            return order.qty();
        }
        let take = self.available(&top, order.side(), order.qty());
        if take.to_scaled() <= 0 {
            return order.qty();
        }
        let px = top.taker_px(order.side());
        let fee = self.fee_for(true, take);
        self.report(order.id(), VenueKind::Filled { px, qty: take, fee });
        Qty::from_scaled(order.qty().to_scaled() - take.to_scaled())
    }

    /// Fills resting orders against a trade print.
    fn fill_resting(&mut self, instrument: InstrumentId, px: Px, qty: Qty) {
        let mut budget = qty.to_scaled();
        let mut index = 0;
        while index < self.resting.len() {
            let r = self.resting[index];
            let touched = r.instrument == instrument
                && match r.side {
                    Side::Buy => px.to_scaled() <= r.limit.to_scaled(),
                    Side::Sell => px.to_scaled() >= r.limit.to_scaled(),
                };
            if !touched || budget <= 0 {
                index += 1;
                continue;
            }
            let take = budget.min(r.remaining.to_scaled());
            if take <= 0 {
                index += 1;
                continue;
            }
            let take_qty = Qty::from_scaled(take);
            let fee = self.fee_for(false, take_qty);
            // A resting order fills at its own price, not at the print's: that
            // is the price the venue promised when it accepted the order.
            self.report(
                r.id,
                VenueKind::Filled {
                    px: r.limit,
                    qty: take_qty,
                    fee,
                },
            );
            budget -= take;
            let left = r.remaining.to_scaled() - take;
            if left <= 0 {
                self.resting.remove(index);
            } else {
                self.resting[index].remaining = Qty::from_scaled(left);
                index += 1;
            }
        }
    }

    /// Fills resting orders that a quote update has made marketable.
    fn sweep_marketable(&mut self, instrument: InstrumentId) {
        let Some(top) = self.books.top(instrument).copied() else {
            return;
        };
        if !top.is_two_sided() {
            return;
        }
        let mut index = 0;
        while index < self.resting.len() {
            let r = self.resting[index];
            if r.instrument != instrument
                || !Self::is_marketable(&top, r.side, OrderKind::Limit(r.limit))
            {
                index += 1;
                continue;
            }
            let take = self.available(&top, r.side, r.remaining);
            if take.to_scaled() <= 0 {
                index += 1;
                continue;
            }
            let px = top.taker_px(r.side);
            let fee = self.fee_for(true, take);
            self.report(r.id, VenueKind::Filled { px, qty: take, fee });
            let left = r.remaining.to_scaled() - take.to_scaled();
            if left <= 0 {
                self.resting.remove(index);
            } else {
                self.resting[index].remaining = Qty::from_scaled(left);
                index += 1;
            }
        }
    }
}

/// A latency span as nanoseconds, clamped into the timestamp's range.
fn latency_nanos(span: ExchangeSpan) -> i64 {
    span.to_nanos().clamp(0, i64::MAX as i128) as i64
}

impl VenueAdapter for SimVenue {
    fn submit(&mut self, order: &Order) -> Result<(), VenueError> {
        if !self.connected {
            return Err(VenueError::Disconnected);
        }
        if order.instrument().index() >= self.books.instrument_count() {
            return Err(VenueError::Malformed(RejectReason::UnknownInstrument));
        }
        if order.qty().to_scaled() <= 0 {
            return Err(VenueError::Malformed(RejectReason::InvalidQuantity));
        }

        self.report(order.id(), VenueKind::Accepted);
        let leftover = self.take_liquidity(order);
        if leftover.to_scaled() <= 0 {
            return Ok(());
        }

        match order.kind() {
            // A market order does not rest. Whatever the touch could not fill
            // is gone, and saying so is better than leaving a phantom order
            // resting at no price.
            OrderKind::Market => self.report(order.id(), VenueKind::Cancelled),
            OrderKind::Limit(limit) => self.resting.push(Resting {
                id: order.id(),
                instrument: order.instrument(),
                side: order.side(),
                limit,
                remaining: leftover,
            }),
        }
        Ok(())
    }

    fn cancel(&mut self, id: OrderId) -> Result<(), VenueError> {
        if !self.connected {
            return Err(VenueError::Disconnected);
        }
        match self.resting.iter().position(|r| r.id == id) {
            Some(index) => {
                self.resting.remove(index);
                self.report(id, VenueKind::Cancelled);
                Ok(())
            }
            None => {
                // Nothing to cancel. The order already finished, or never
                // rested; either way the venue says so rather than staying
                // silent, so the engine's state machine can settle.
                self.report(
                    id,
                    VenueKind::CancelRejected {
                        reason: RejectReason::UnknownOrder,
                    },
                );
                Ok(())
            }
        }
    }

    fn drain(&mut self, out: &mut Vec<Inbound>) {
        out.append(&mut self.pending);
    }

    fn observe_market(&mut self, market: &MarketEvent) {
        self.observe(market);
    }
}
