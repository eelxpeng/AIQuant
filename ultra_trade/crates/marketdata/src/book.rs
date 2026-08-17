//! Top-of-book state, kept per instrument.

use event::{MarketEvent, MarketKind};
use types::{ExchangeSpan, ExchangeTime, InstrumentId, Px, Qty, RoundDir, Side};

/// The best bid and best ask with their sizes, at a stated event timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TopOfBook {
    /// Best bid.
    pub bid_px: Px,
    /// Size at the best bid.
    pub bid_qty: Qty,
    /// Best ask.
    pub ask_px: Px,
    /// Size at the best ask.
    pub ask_qty: Qty,
    /// The venue timestamp this snapshot is as of.
    pub exchange_time: ExchangeTime,
}

impl TopOfBook {
    /// The arithmetic midpoint of bid and ask.
    ///
    /// The caller names the rounding direction because the midpoint of two
    /// ticks routinely falls between representable values, and this crate has
    /// no default rounding (Constitution VII). A derived convenience — never a
    /// price the system can trade at (`CONTEXT.md`).
    ///
    /// `None` when the book is crossed or one-sided: there is no meaningful
    /// midpoint, and returning a number anyway is how a stale or broken feed
    /// gets priced into a position.
    #[inline]
    pub fn mid(&self, dir: RoundDir) -> Option<Px> {
        if !self.is_two_sided() || self.is_crossed() {
            return None;
        }
        // Widen before summing: two near-`i64::MAX` prices overflow otherwise.
        let sum = self.bid_px.to_scaled() as i128 + self.ask_px.to_scaled() as i128;
        let halved = crate::halve(sum, dir);
        i64::try_from(halved).ok().map(Px::from_scaled)
    }

    /// Ask minus bid.
    ///
    /// `None` for a one-sided book. A crossed book yields a negative spread
    /// rather than `None`, because that is a real and diagnosable condition.
    #[inline]
    pub fn spread(&self) -> Option<Px> {
        if !self.is_two_sided() {
            return None;
        }
        self.ask_px.checked_sub(self.bid_px).ok()
    }

    /// Whether both sides carry size.
    #[inline]
    pub const fn is_two_sided(&self) -> bool {
        self.bid_qty.to_scaled() > 0 && self.ask_qty.to_scaled() > 0
    }

    /// Whether the bid is at or above the ask.
    ///
    /// Compared exactly, never with a tolerance (Constitution VII).
    #[inline]
    pub const fn is_crossed(&self) -> bool {
        self.bid_px.to_scaled() >= self.ask_px.to_scaled()
    }

    /// The price a taker on this side would hit.
    #[inline]
    pub const fn taker_px(&self, side: Side) -> Px {
        match side {
            Side::Buy => self.ask_px,
            Side::Sell => self.bid_px,
        }
    }

    /// The size available to a taker on this side.
    #[inline]
    pub const fn taker_qty(&self, side: Side) -> Qty {
        match side {
            Side::Buy => self.ask_qty,
            Side::Sell => self.bid_qty,
        }
    }
}

/// The most recent trade print for an instrument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LastTrade {
    /// The price it printed at.
    pub px: Px,
    /// How much traded.
    pub qty: Qty,
    /// Which side took liquidity.
    pub aggressor: Side,
    /// The venue timestamp.
    pub exchange_time: ExchangeTime,
}

/// Levels a side kept by default, matching what a venue commonly publishes.
const DEFAULT_DEPTH: usize = 16;
/// Levels one update may carry by default.
const DEFAULT_PENDING: usize = 64;

/// One price level of the book.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    /// Where it rests.
    pub px: Px,
    /// How much rests there.
    pub qty: Qty,
}

/// The levels of one side, best first.
///
/// A fixed-capacity array rather than a map: the hot path may not allocate, and
/// a depth feed is a delta stream that touches one or two levels an update, so
/// the cost of keeping it sorted is a memmove of a handful of entries.
#[derive(Debug, Clone)]
pub struct Depth {
    side: Side,
    levels: Vec<Level>,
}

impl Depth {
    /// An empty side that can hold `capacity` levels.
    pub fn with_capacity(side: Side, capacity: usize) -> Depth {
        Depth {
            side,
            levels: Vec::with_capacity(capacity),
        }
    }

    /// The levels, best price first.
    #[inline]
    pub fn levels(&self) -> &[Level] {
        &self.levels
    }

    /// The best level, if the side has one.
    #[inline]
    pub fn best(&self) -> Option<Level> {
        self.levels.first().copied()
    }

    /// Whether this side holds nothing.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.levels.is_empty()
    }

    /// Whether the next insert of a new price would grow the backing store.
    ///
    /// The hot path may not allocate, so a caller that cares checks this rather
    /// than discovering it in a profile (Constitution VI).
    #[inline]
    pub fn would_grow(&self) -> bool {
        self.levels.len() == self.levels.capacity()
    }

    /// Whether `a` should sort before `b` on this side.
    ///
    /// Bids descend and asks ascend, which is the only place in this type that
    /// knows which side it is.
    #[inline]
    fn better(&self, a: Px, b: Px) -> bool {
        match self.side {
            Side::Buy => a.to_scaled() > b.to_scaled(),
            Side::Sell => a.to_scaled() < b.to_scaled(),
        }
    }

    /// Sets the size at a price. **Zero removes the level.**
    ///
    /// Removal-by-zero is how the venue expresses it, so translating it into
    /// something else here would be inventing a second vocabulary.
    ///
    /// Returns `false` if a new level could not be added because the side is
    /// full. Refused rather than dropping the worst level: silently trimming
    /// would make the book look thinner than the venue said, and a fill model
    /// walking it would under-fill for a reason nothing recorded.
    pub fn set(&mut self, px: Px, qty: Qty) -> bool {
        let found = self.levels.iter().position(|l| l.px == px);
        if qty.to_scaled() <= 0 {
            if let Some(at) = found {
                self.levels.remove(at);
            }
            return true;
        }
        if let Some(at) = found {
            self.levels[at].qty = qty;
            return true;
        }
        if self.levels.len() == self.levels.capacity() {
            return false;
        }
        let at = self
            .levels
            .iter()
            .position(|l| self.better(px, l.px))
            .unwrap_or(self.levels.len());
        self.levels.insert(at, Level { px, qty });
        true
    }

    /// Forgets every level, for a snapshot that replaces the side.
    pub fn clear(&mut self) {
        self.levels.clear();
    }
}

/// The rule that picks a valuation price.
///
/// A policy decision stated at the call site, not "whatever price was handy"
/// (`CONTEXT.md`). Risk and reporting may legitimately choose differently, so
/// the choice is a parameter rather than a constant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MarkRule {
    /// Midpoint of top-of-book, rounded in the named direction.
    Mid(RoundDir),
    /// The price of the last trade.
    LastTrade,
}

/// What folding a market event into the book did.
///
/// Returned rather than swallowed, and `#[must_use]`, because the interesting
/// answers are the ones that are easy to ignore: a caller that drops this
/// silently is the shape of the bug this type exists to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[must_use]
pub enum Applied {
    /// Folded in. The book now reflects this event.
    Accepted,
    /// Stamped earlier than what is already stored, so the book is unchanged.
    ///
    /// A feed that reorders is an ordinary network condition, not a fault. What
    /// is not ordinary is letting it move the book backwards, because risk then
    /// prices against a top of book that the market has already left.
    ///
    /// Distinct from *stale market data* in the risk sense, which is about a
    /// quote's **age**. This is about its **arrival order**, and the two must
    /// not be conflated: a book can be perfectly ordered and hours old, or
    /// current and reordered.
    OutOfOrder {
        /// The timestamp already stored for this instrument and event kind.
        stored: ExchangeTime,
    },
    /// A book update carried more levels than the buffer holds.
    ///
    /// The whole update is dropped and the book is unchanged. Refused rather
    /// than truncated: a partly applied update is a book the venue never
    /// published, and a fill model walking it would under-fill for a reason
    /// nothing in the log explains (ADR, order-book depth D-2).
    UpdateTooLarge,
    /// The instrument id is outside the configured range. A configuration
    /// error, not a market condition.
    UnknownInstrument,
}

/// Top-of-book and last-trade state for every configured instrument.
///
/// Indexed by [`InstrumentId`], which is a dense index, so a lookup is a bounds
/// check rather than a hash. That keeps the read off both the allocator and
/// `HashMap` iteration order (Constitution II, VI).
#[derive(Debug, Clone, Default)]
pub struct Books {
    tops: Vec<Option<TopOfBook>>,
    trades: Vec<Option<LastTrade>>,
    out_of_order: Vec<u32>,
    /// Depth per instrument, bids and asks. Empty until a depth feed arrives;
    /// a top-of-book-only session never touches it (ADR, order-book depth D-3).
    bids: Vec<Depth>,
    asks: Vec<Depth>,
    /// Levels of the update in flight, not yet in force.
    ///
    /// D-2: between the first level of an update and its completion the book
    /// can be crossed, which is a state the venue never published. Nothing sees
    /// it, so the levels wait here.
    pending: Vec<(InstrumentId, Side, Px, Qty)>,
    /// Updates refused for carrying more levels than the buffer holds.
    overflowed: u64,
}

impl Books {
    /// Storage for `count` instruments, all initially without data.
    ///
    /// Sized once at session start. Nothing here grows afterwards, so the hot
    /// path never allocates.
    pub fn with_instruments(count: usize) -> Books {
        Books::with_depth(count, DEFAULT_DEPTH, DEFAULT_PENDING)
    }

    /// Storage sized for a depth feed.
    ///
    /// `depth` is levels a side, and `pending` is the most levels one update
    /// may carry. Both are fixed here so the hot path never allocates.
    pub fn with_depth(count: usize, depth: usize, pending: usize) -> Books {
        Books {
            tops: vec![None; count],
            trades: vec![None; count],
            out_of_order: vec![0; count],
            bids: (0..count)
                .map(|_| Depth::with_capacity(Side::Buy, depth))
                .collect(),
            asks: (0..count)
                .map(|_| Depth::with_capacity(Side::Sell, depth))
                .collect(),
            pending: Vec::with_capacity(pending),
            overflowed: 0,
        }
    }

    /// The resting size on one side of an instrument, best price first.
    #[inline]
    pub fn depth(&self, instrument: InstrumentId, side: Side) -> Option<&Depth> {
        let index = instrument.index();
        match side {
            Side::Buy => self.bids.get(index),
            Side::Sell => self.asks.get(index),
        }
    }

    /// Book updates refused for carrying more levels than the buffer holds.
    ///
    /// A number that is not zero means this session was fed a book it could not
    /// represent, and every fill model that walked it was walking a partial
    /// one. Counted rather than logged, like the out-of-order refusals.
    #[inline]
    pub const fn overflowed_updates(&self) -> u64 {
        self.overflowed
    }

    /// How many instruments this store was built for.
    #[inline]
    pub fn instrument_count(&self) -> usize {
        self.tops.len()
    }

    /// Folds a market event into the book state.
    ///
    /// An event stamped **earlier** than what is stored is refused and the book
    /// is left alone. An event stamped at the **same** instant is folded in:
    /// venues stamp at coarser granularity than a nanosecond, so many updates
    /// legitimately share a timestamp, and refusing ties would discard most of
    /// a millisecond-stamped feed.
    ///
    /// Quotes and trades are ordered **separately**. They are two sequences
    /// that interleave with different latencies, so a trade arriving after a
    /// newer quote says nothing about either one's ordering.
    ///
    /// What this does *not* catch is a **duplicate**: the same event delivered
    /// twice. Two genuine executions can share a timestamp, price, quantity,
    /// and side, so content equality is not evidence of duplication. Detecting
    /// one needs a per-venue sequence number, which belongs to feed
    /// normalization and is deferred with it (#1).
    pub fn apply(&mut self, ev: &MarketEvent) -> Applied {
        let index = ev.instrument.index();
        if index >= self.tops.len() {
            return Applied::UnknownInstrument;
        }

        let stored = match ev.kind {
            MarketKind::Quote { .. } => self.tops[index].map(|t| t.exchange_time),
            MarketKind::Trade { .. } => self.trades[index].map(|t| t.exchange_time),
            // A run of levels is one update. Ordering is checked when it
            // completes, because the levels within it share a timestamp and
            // checking each against the last would refuse the update's own
            // second level.
            MarketKind::Level { .. } => None,
            MarketKind::BookApplied => self.tops[index].map(|t| t.exchange_time),
        };
        if let Some(stored) = stored
            && ev.exchange_time < stored
        {
            self.out_of_order[index] = self.out_of_order[index].saturating_add(1);
            return Applied::OutOfOrder { stored };
        }

        match ev.kind {
            MarketKind::Quote {
                bid_px,
                bid_qty,
                ask_px,
                ask_qty,
            } => {
                self.tops[index] = Some(TopOfBook {
                    bid_px,
                    bid_qty,
                    ask_px,
                    ask_qty,
                    exchange_time: ev.exchange_time,
                });
            }
            MarketKind::Trade { px, qty, aggressor } => {
                self.trades[index] = Some(LastTrade {
                    px,
                    qty,
                    aggressor,
                    exchange_time: ev.exchange_time,
                });
            }
            MarketKind::Level { side, px, qty } => {
                if self.pending.len() == self.pending.capacity() {
                    // Refused, not truncated: half a book applied is worse than
                    // none, and a model walking it would under-fill for a
                    // reason nothing recorded (ADR D-2).
                    self.pending.clear();
                    self.overflowed = self.overflowed.saturating_add(1);
                    return Applied::UpdateTooLarge;
                }
                self.pending.push((ev.instrument, side, px, qty));
            }
            MarketKind::BookApplied => {
                let mut full = false;
                for (instrument, side, px, qty) in self.pending.drain(..) {
                    let at = instrument.index();
                    let book = match side {
                        Side::Buy => &mut self.bids[at],
                        Side::Sell => &mut self.asks[at],
                    };
                    full |= !book.set(px, qty);
                }
                if full {
                    self.overflowed = self.overflowed.saturating_add(1);
                }
                // The top of book now follows from the depth, so a depth feed
                // and a quote feed leave `top()` meaning the same thing (D-3).
                let bid = self.bids[index].best();
                let ask = self.asks[index].best();
                if let (Some(bid), Some(ask)) = (bid, ask) {
                    self.tops[index] = Some(TopOfBook {
                        bid_px: bid.px,
                        bid_qty: bid.qty,
                        ask_px: ask.px,
                        ask_qty: ask.qty,
                        exchange_time: ev.exchange_time,
                    });
                }
            }
        }
        Applied::Accepted
    }

    /// How many events this instrument has had refused for arriving late.
    ///
    /// A counter rather than a log record. Each refusal is a deterministic
    /// function of records already in the log, so replay reproduces it without
    /// help — and a reordering feed would otherwise write an unbounded number
    /// of records saying nothing new. A number that climbs is a feed to
    /// investigate.
    #[inline]
    pub fn out_of_order_count(&self, instrument: InstrumentId) -> u32 {
        self.out_of_order
            .get(instrument.index())
            .copied()
            .unwrap_or(0)
    }

    /// How many events have been refused for arriving late, across all
    /// instruments.
    pub fn total_out_of_order(&self) -> u64 {
        self.out_of_order.iter().map(|n| u64::from(*n)).sum()
    }

    /// The current top of book, if any has arrived.
    #[inline]
    pub fn top(&self, instrument: InstrumentId) -> Option<&TopOfBook> {
        self.tops.get(instrument.index())?.as_ref()
    }

    /// The most recent trade, if any has printed.
    #[inline]
    pub fn last_trade(&self, instrument: InstrumentId) -> Option<&LastTrade> {
        self.trades.get(instrument.index())?.as_ref()
    }

    /// The valuation price under the named rule.
    ///
    /// `None` when the rule cannot be evaluated — no data, or a book too broken
    /// to have a midpoint. Callers must fail closed on `None` rather than
    /// substituting a last known value (Constitution V).
    pub fn mark(&self, instrument: InstrumentId, rule: MarkRule) -> Option<Px> {
        match rule {
            MarkRule::Mid(dir) => self.top(instrument)?.mid(dir),
            MarkRule::LastTrade => self.last_trade(instrument).map(|t| t.px),
        }
    }

    /// How old the top of book is, measured against a current exchange time.
    ///
    /// Both operands are exchange time, so the subtraction is legal and the
    /// span carries that kind. `None` when no quote has arrived — which is a
    /// different condition from "arrived long ago" and must not collapse into
    /// it (Constitution V).
    pub fn quote_age(&self, instrument: InstrumentId, now: ExchangeTime) -> Option<ExchangeSpan> {
        Some(now - self.top(instrument)?.exchange_time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use types::Timestamp;

    const fn px(units: i64) -> Px {
        Px::from_scaled(units)
    }

    const fn qty(units: i64) -> Qty {
        Qty::from_scaled(units)
    }

    fn at(n: i64) -> ExchangeTime {
        Timestamp::from_nanos(n)
    }

    fn quote(bid: i64, ask: i64) -> TopOfBook {
        TopOfBook {
            bid_px: px(bid),
            bid_qty: qty(1),
            ask_px: px(ask),
            ask_qty: qty(1),
            exchange_time: at(0),
        }
    }

    /// Applies an event the test expects the book to take.
    ///
    /// Asserting rather than discarding: a setup event that was quietly refused
    /// would make the rest of the test assert against a book that never
    /// changed, and it would still pass.
    fn accept(books: &mut Books, ev: &MarketEvent) {
        assert_eq!(
            books.apply(ev),
            Applied::Accepted,
            "setup event was refused"
        );
    }

    /// Applies an event the test expects the book to refuse for arriving late.
    fn refuse(books: &mut Books, ev: &MarketEvent) {
        assert!(
            matches!(books.apply(ev), Applied::OutOfOrder { .. }),
            "event should have been refused as late"
        );
    }

    fn trade_event(instrument: u32, price: i64, size: i64, ts: i64) -> MarketEvent {
        MarketEvent {
            instrument: InstrumentId::new(instrument),
            exchange_time: at(ts),
            receive_time: Timestamp::from_nanos(ts + 1),
            kind: MarketKind::Trade {
                px: px(price),
                qty: qty(size),
                aggressor: Side::Buy,
            },
        }
    }

    fn quote_event(instrument: u32, bid: i64, ask: i64, ts: i64) -> MarketEvent {
        MarketEvent {
            instrument: InstrumentId::new(instrument),
            exchange_time: at(ts),
            receive_time: Timestamp::from_nanos(ts + 1),
            kind: MarketKind::Quote {
                bid_px: px(bid),
                bid_qty: qty(1),
                ask_px: px(ask),
                ask_qty: qty(1),
            },
        }
    }

    #[test]
    fn the_midpoint_rounds_in_the_direction_the_caller_names() {
        // Bid 1, ask 2: the midpoint falls between scale units.
        let book = quote(1, 2);
        assert_eq!(book.mid(RoundDir::Down), Some(px(1)));
        assert_eq!(book.mid(RoundDir::Up), Some(px(2)));
    }

    #[test]
    fn a_crossed_book_has_no_midpoint() {
        assert_eq!(quote(5, 4).mid(RoundDir::Down), None);
        // Locked counts as crossed: there is no spread to be inside of.
        assert_eq!(quote(4, 4).mid(RoundDir::Down), None);
    }

    #[test]
    fn a_one_sided_book_has_no_midpoint_and_no_spread() {
        let mut book = quote(1, 2);
        book.ask_qty = Qty::ZERO;
        assert_eq!(book.mid(RoundDir::Down), None);
        assert_eq!(book.spread(), None);
    }

    #[test]
    fn a_crossed_book_reports_a_negative_spread_rather_than_hiding_it() {
        assert_eq!(quote(5, 4).spread(), Some(px(-1)));
    }

    #[test]
    fn the_midpoint_survives_the_widest_representable_prices() {
        let book = TopOfBook {
            bid_px: Px::from_scaled(i64::MAX - 1),
            bid_qty: qty(1),
            ask_px: Px::MAX,
            ask_qty: qty(1),
            exchange_time: at(0),
        };
        // Summing in i64 would overflow; the widened sum halves cleanly.
        assert_eq!(
            book.mid(RoundDir::Down),
            Some(Px::from_scaled(i64::MAX - 1))
        );
    }

    #[test]
    fn a_taker_lifts_the_offer_and_hits_the_bid() {
        let book = quote(1, 2);
        assert_eq!(book.taker_px(Side::Buy), px(2));
        assert_eq!(book.taker_px(Side::Sell), px(1));
    }

    #[test]
    fn an_instrument_without_data_reads_as_absent_not_as_zero() {
        let books = Books::with_instruments(2);
        let id = InstrumentId::new(0);
        assert_eq!(books.top(id), None);
        assert_eq!(books.last_trade(id), None);
        assert_eq!(books.mark(id, MarkRule::LastTrade), None);
        assert_eq!(books.quote_age(id, at(100)), None);
    }

    #[test]
    fn an_out_of_range_instrument_is_reported_rather_than_ignored() {
        let mut books = Books::with_instruments(1);
        assert_eq!(
            books.apply(&quote_event(7, 1, 2, 0)),
            Applied::UnknownInstrument
        );
        assert_eq!(books.apply(&quote_event(0, 1, 2, 0)), Applied::Accepted);
    }

    #[test]
    fn a_late_event_reports_the_timestamp_it_lost_to() {
        let mut books = Books::with_instruments(1);
        accept(&mut books, &quote_event(0, 10, 12, 1_000));
        assert_eq!(
            books.apply(&quote_event(0, 1, 99, 500)),
            Applied::OutOfOrder { stored: at(1_000) }
        );
    }

    #[test]
    fn quotes_and_trades_are_ordered_separately() {
        // Two sequences interleaving with different latencies. A trade stamped
        // before the newest quote says nothing about either one's ordering, so
        // refusing it would discard good data.
        let mut books = Books::with_instruments(1);
        accept(&mut books, &quote_event(0, 10, 12, 1_000));
        assert_eq!(books.apply(&trade_event(0, 11, 1, 500)), Applied::Accepted);
        assert_eq!(
            books.last_trade(InstrumentId::new(0)).expect("trade").px,
            px(11)
        );
    }

    #[test]
    fn late_events_are_counted_per_instrument() {
        let mut books = Books::with_instruments(2);
        let first = InstrumentId::new(0);
        let second = InstrumentId::new(1);
        assert_eq!(books.out_of_order_count(first), 0);

        accept(&mut books, &quote_event(0, 10, 12, 1_000));
        refuse(&mut books, &quote_event(0, 1, 2, 500));
        refuse(&mut books, &quote_event(0, 1, 2, 900));
        accept(&mut books, &quote_event(1, 10, 12, 1_000));

        assert_eq!(books.out_of_order_count(first), 2);
        assert_eq!(books.out_of_order_count(second), 0);
        assert_eq!(books.total_out_of_order(), 2);
    }

    #[test]
    fn the_first_event_for_an_instrument_is_never_late() {
        // The degenerate case: nothing is stored, so there is nothing to be
        // earlier than.
        let mut books = Books::with_instruments(1);
        assert_eq!(
            books.apply(&quote_event(0, 1, 2, i64::MIN)),
            Applied::Accepted
        );
    }

    #[test]
    fn quote_age_is_measured_on_the_exchange_clock() {
        let mut books = Books::with_instruments(1);
        accept(&mut books, &quote_event(0, 1, 2, 1_000));
        let age = books
            .quote_age(InstrumentId::new(0), at(1_500))
            .expect("age");
        assert_eq!(age.to_nanos(), 500);
    }

    #[test]
    fn a_quote_stamped_earlier_than_the_stored_one_does_not_move_the_book() {
        let mut books = Books::with_instruments(1);
        accept(&mut books, &quote_event(0, 10, 12, 1_000));
        refuse(&mut books, &quote_event(0, 1, 99, 500));

        let top = books.top(InstrumentId::new(0)).expect("quote");
        assert_eq!(top.bid_px, px(10));
        assert_eq!(top.ask_px, px(12));
        assert_eq!(top.exchange_time, at(1_000));
    }

    #[test]
    fn a_quote_stamped_at_the_same_instant_is_folded_in() {
        // Venues stamp at coarser granularity than a nanosecond, so many
        // updates legitimately share a timestamp. Refusing ties would drop most
        // of a millisecond-stamped feed.
        let mut books = Books::with_instruments(1);
        accept(&mut books, &quote_event(0, 10, 12, 1_000));
        accept(&mut books, &quote_event(0, 11, 13, 1_000));

        let top = books.top(InstrumentId::new(0)).expect("quote");
        assert_eq!(top.bid_px, px(11));
        assert_eq!(top.ask_px, px(13));
    }

    #[test]
    fn a_trade_stamped_earlier_than_the_stored_one_does_not_move_the_last_trade() {
        let mut books = Books::with_instruments(1);
        accept(&mut books, &trade_event(0, 100, 5, 1_000));
        refuse(&mut books, &trade_event(0, 1, 5, 500));
        assert_eq!(
            books.last_trade(InstrumentId::new(0)).expect("trade").px,
            px(100)
        );
    }

    #[test]
    fn a_trade_updates_the_last_trade_without_disturbing_the_quote() {
        let mut books = Books::with_instruments(1);
        let id = InstrumentId::new(0);
        accept(&mut books, &quote_event(0, 10, 12, 1));
        accept(
            &mut books,
            &MarketEvent {
                instrument: id,
                exchange_time: at(2),
                receive_time: Timestamp::from_nanos(3),
                kind: MarketKind::Trade {
                    px: px(11),
                    qty: qty(5),
                    aggressor: Side::Buy,
                },
            },
        );
        assert_eq!(books.mark(id, MarkRule::LastTrade), Some(px(11)));
        assert_eq!(books.top(id).expect("quote").ask_px, px(12));
    }
}
