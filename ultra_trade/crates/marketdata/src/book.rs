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

/// Top-of-book and last-trade state for every configured instrument.
///
/// Indexed by [`InstrumentId`], which is a dense index, so a lookup is a bounds
/// check rather than a hash. That keeps the read off both the allocator and
/// `HashMap` iteration order (Constitution II, VI).
#[derive(Debug, Clone, Default)]
pub struct Books {
    tops: Vec<Option<TopOfBook>>,
    trades: Vec<Option<LastTrade>>,
}

impl Books {
    /// Storage for `count` instruments, all initially without data.
    ///
    /// Sized once at session start. Nothing here grows afterwards, so the hot
    /// path never allocates.
    pub fn with_instruments(count: usize) -> Books {
        Books {
            tops: vec![None; count],
            trades: vec![None; count],
        }
    }

    /// How many instruments this store was built for.
    #[inline]
    pub fn instrument_count(&self) -> usize {
        self.tops.len()
    }

    /// Folds a market event into the book state.
    ///
    /// Returns whether the id was in range. An out-of-range id is a
    /// configuration error, not a market condition, and silently ignoring it
    /// would leave risk checking a book that never updates.
    pub fn apply(&mut self, ev: &MarketEvent) -> bool {
        let index = ev.instrument.index();
        if index >= self.tops.len() {
            return false;
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
        }
        true
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
        assert!(!books.apply(&quote_event(7, 1, 2, 0)));
        assert!(books.apply(&quote_event(0, 1, 2, 0)));
    }

    #[test]
    fn quote_age_is_measured_on_the_exchange_clock() {
        let mut books = Books::with_instruments(1);
        books.apply(&quote_event(0, 1, 2, 1_000));
        let age = books
            .quote_age(InstrumentId::new(0), at(1_500))
            .expect("age");
        assert_eq!(age.to_nanos(), 500);
    }

    #[test]
    fn a_trade_updates_the_last_trade_without_disturbing_the_quote() {
        let mut books = Books::with_instruments(1);
        let id = InstrumentId::new(0);
        books.apply(&quote_event(0, 10, 12, 1));
        books.apply(&MarketEvent {
            instrument: id,
            exchange_time: at(2),
            receive_time: Timestamp::from_nanos(3),
            kind: MarketKind::Trade {
                px: px(11),
                qty: qty(5),
                aggressor: Side::Buy,
            },
        });
        assert_eq!(books.mark(id, MarkRule::LastTrade), Some(px(11)));
        assert_eq!(books.top(id).expect("quote").ask_px, px(12));
    }
}
