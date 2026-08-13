//! Bar aggregation.
//!
//! A trade print is a fact; a bar is a convention. This module owns the
//! machinery — the windowing and, above all, the rule for when a bar is
//! complete. Definitions (1-minute, volume, dollar, tick) are configuration
//! (`docs/ARCHITECTURE.md` seam 6).
//!
//! The invariant this module exists to protect: **an incomplete bar is never
//! visible as a complete one.** It is enforced structurally — the forming bar
//! has no accessor, and the only way to obtain a [`Bar`] is for one of the
//! completion rules below to fire.

use types::{ExchangeSpan, ExchangeTime, InstrumentId, Notional, Px, Qty, RoundDir};

/// How a bar decides it is finished.
///
/// Configuration, not code: strategies legitimately disagree about the
/// convention, so the convention is data. What they may not do is re-implement
/// the completion rule, because "using a bar's close before the bar is
/// complete" is look-ahead, is silently profitable in backtest, and must exist
/// in exactly one tested place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BarSpec {
    /// Fixed windows on the exchange clock, aligned to multiples of the period
    /// from the epoch.
    Time {
        /// Window length.
        period: ExchangeSpan,
    },
    /// Complete once this much has traded.
    Volume {
        /// Quantity threshold.
        threshold: Qty,
    },
    /// Complete once this many trades have printed.
    Tick {
        /// Trade-count threshold.
        threshold: u32,
    },
    /// Complete once this much traded value has changed hands.
    Dollar {
        /// Notional threshold.
        threshold: Notional,
    },
}

/// A completed aggregate. Its values can no longer change.
///
/// Only complete bars exist as values of this type, so holding one is proof the
/// window closed (`CONTEXT.md`, "Complete Bar").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Bar {
    /// What it aggregates.
    pub instrument: InstrumentId,
    /// First trade price in the window.
    pub open: Px,
    /// Highest trade price in the window.
    pub high: Px,
    /// Lowest trade price in the window.
    pub low: Px,
    /// Last trade price in the window.
    pub close: Px,
    /// Total quantity traded.
    pub volume: Qty,
    /// Number of prints.
    pub trades: u32,
    /// Total value traded, each print truncated toward zero.
    pub value: Notional,
    /// The first trade's exchange timestamp.
    pub open_time: ExchangeTime,
    /// When the window closed, on the exchange clock.
    ///
    /// For a time bar this is the window boundary, not the timestamp of the
    /// trade that revealed the window had closed.
    pub close_time: ExchangeTime,
}

/// Why aggregation refused a trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AggregateError {
    /// Accumulated volume or value left the representable range.
    Overflow,
    /// A trade arrived stamped earlier than one already in the bar.
    OutOfOrder,
    /// The period or threshold was not positive.
    InvalidSpec,
    /// The trade named an instrument this aggregator does not follow.
    WrongInstrument,
}

/// Each print's value is truncated toward zero before it is accumulated, so a
/// bar's traded value never overstates what actually changed hands.
const VALUE_ROUNDING: RoundDir = RoundDir::TowardZero;

/// The bar being built. Deliberately private: there is no way to read it.
#[derive(Debug, Clone, Copy)]
struct Forming {
    open: Px,
    high: Px,
    low: Px,
    close: Px,
    volume: Qty,
    trades: u32,
    value: Notional,
    open_time: ExchangeTime,
    last_time: ExchangeTime,
    /// Set for time bars only: the instant this window closes.
    window_end: Option<ExchangeTime>,
}

/// Builds bars of one specification for one instrument.
#[derive(Debug, Clone)]
pub struct Aggregator {
    instrument: InstrumentId,
    spec: BarSpec,
    forming: Option<Forming>,
}

impl Aggregator {
    /// Builds an aggregator, rejecting a specification that could never
    /// complete.
    ///
    /// A zero period or threshold is refused here rather than looping forever
    /// or completing on every print.
    pub fn new(instrument: InstrumentId, spec: BarSpec) -> Result<Aggregator, AggregateError> {
        let valid = match spec {
            BarSpec::Time { period } => period.to_nanos() > 0,
            BarSpec::Volume { threshold } => threshold.to_scaled() > 0,
            BarSpec::Tick { threshold } => threshold > 0,
            BarSpec::Dollar { threshold } => threshold.to_scaled() > 0,
        };
        if !valid {
            return Err(AggregateError::InvalidSpec);
        }
        Ok(Aggregator {
            instrument,
            spec,
            forming: None,
        })
    }

    /// Which instrument this follows.
    #[inline]
    pub const fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// The specification it builds to.
    #[inline]
    pub const fn spec(&self) -> BarSpec {
        self.spec
    }

    /// Folds a trade in, returning a bar only if one completed.
    ///
    /// Which bar the triggering trade belongs to differs by kind, and both
    /// answers are deliberate:
    ///
    /// - **Time**: a trade past the window boundary proves the window closed,
    ///   so the bar completes *without* it and the trade opens the next one.
    /// - **Volume, tick, dollar**: the threshold is reached *by* the trade, so
    ///   it belongs to the bar it completed.
    ///
    /// On error nothing is mutated: a rejected trade leaves the forming bar
    /// exactly as it was, so a caller that halts and investigates sees the
    /// state that produced the error.
    pub fn on_trade(
        &mut self,
        instrument: InstrumentId,
        px: Px,
        qty: Qty,
        at: ExchangeTime,
    ) -> Result<Option<Bar>, AggregateError> {
        if instrument != self.instrument {
            return Err(AggregateError::WrongInstrument);
        }

        // Ordering is checked before anything else. A trade stamped earlier
        // than the bar it would join sits in an earlier window too, and letting
        // it reach the window logic below would close the current bar on the
        // strength of a timestamp that went backwards.
        if let Some(f) = &self.forming
            && at < f.last_time
        {
            return Err(AggregateError::OutOfOrder);
        }

        // A time bar may close before this trade is folded in at all.
        let mut completed = None;
        if let BarSpec::Time { period } = self.spec {
            let boundary = window_end(at, period)?;
            let window_closed = matches!(&self.forming, Some(f) if f.window_end != Some(boundary));
            if window_closed {
                completed = self.close_forming(None);
            }
        }

        match &mut self.forming {
            Some(f) => {
                // Compute first, commit second: an overflow must not leave a
                // half-updated bar behind.
                let volume = qty
                    .checked_add(f.volume)
                    .map_err(|_| AggregateError::Overflow)?;
                // Plain addition, not checked, and the volume check above is
                // what makes that safe. Volume can never exceed `Qty::MAX` and
                // every price is bounded by `Px::MAX`, so the widest traded
                // value a bar can hold is about 8.5e19 whole units — roughly
                // ten orders of magnitude inside `Notional`'s range. The bound
                // is asserted in `the_traded_value_accumulator_cannot_overflow`.
                let value = f.value + px.notional(qty, VALUE_ROUNDING);
                f.volume = volume;
                f.value = value;
                // Saturating, unlike the two above, because the print count is
                // a diagnostic rather than an accounting field. Volume and
                // value are the numbers money is derived from and both are
                // checked; a counter that needs 4.29 billion prints in one bar
                // to be wrong does not justify an error arm no test can reach.
                f.trades = f.trades.saturating_add(1);
                f.close = px;
                f.last_time = at;
                if px > f.high {
                    f.high = px;
                }
                if px < f.low {
                    f.low = px;
                }
            }
            None => {
                let window_end = match self.spec {
                    BarSpec::Time { period } => Some(window_end(at, period)?),
                    _ => None,
                };
                self.forming = Some(Forming {
                    open: px,
                    high: px,
                    low: px,
                    close: px,
                    volume: qty,
                    trades: 1,
                    value: px.notional(qty, VALUE_ROUNDING),
                    open_time: at,
                    last_time: at,
                    window_end,
                });
            }
        }

        if let Some(bar) = completed {
            return Ok(Some(bar));
        }
        Ok(self.check_threshold(at))
    }

    /// Completes a time bar whose window has closed, without waiting for the
    /// next trade.
    ///
    /// This is not look-ahead: the window genuinely ended at or before `now`.
    /// It exists so a timer event can flush a bar in a quiet market, which is
    /// the only way a time bar closes when nothing trades.
    ///
    /// Always `None` for volume, tick, and dollar bars — the passage of time
    /// says nothing about their completion.
    pub fn on_time(&mut self, now: ExchangeTime) -> Option<Bar> {
        let end = self.forming.as_ref()?.window_end?;
        if now < end {
            return None;
        }
        self.close_forming(None)
    }

    /// Whether a bar is currently being built.
    ///
    /// Reports only *that* one exists, never anything about it. Its open, high,
    /// low, close, and volume stay unreachable until the window closes.
    #[inline]
    pub const fn is_forming(&self) -> bool {
        self.forming.is_some()
    }

    fn check_threshold(&mut self, at: ExchangeTime) -> Option<Bar> {
        let f = self.forming.as_ref()?;
        let reached = match self.spec {
            BarSpec::Time { .. } => false,
            BarSpec::Volume { threshold } => f.volume.to_scaled() >= threshold.to_scaled(),
            BarSpec::Tick { threshold } => f.trades >= threshold,
            BarSpec::Dollar { threshold } => f.value.to_scaled() >= threshold.to_scaled(),
        };
        if reached {
            self.close_forming(Some(at))
        } else {
            None
        }
    }

    /// Takes the forming bar and stamps its close time.
    ///
    /// `close_at` is `None` for a time bar, which closes at its window boundary
    /// rather than at any trade's timestamp.
    fn close_forming(&mut self, close_at: Option<ExchangeTime>) -> Option<Bar> {
        let f = self.forming.take()?;
        let close_time = close_at.or(f.window_end).unwrap_or(f.last_time);
        Some(Bar {
            instrument: self.instrument,
            open: f.open,
            high: f.high,
            low: f.low,
            close: f.close,
            volume: f.volume,
            trades: f.trades,
            value: f.value,
            open_time: f.open_time,
            close_time,
        })
    }
}

/// The end of the window containing `at`, aligned to multiples of `period`
/// from the epoch.
///
/// Alignment is to the epoch rather than to the first trade so that two
/// processes started at different moments cut identical bars — a replay that
/// disagreed about bar boundaries would not be a replay.
fn window_end(at: ExchangeTime, period: ExchangeSpan) -> Result<ExchangeTime, AggregateError> {
    let p = period.to_nanos();
    if p <= 0 {
        return Err(AggregateError::InvalidSpec);
    }
    let ts = at.to_nanos() as i128;
    let end = (ts.div_euclid(p) + 1) * p;
    i64::try_from(end)
        .map(ExchangeTime::from_nanos)
        .map_err(|_| AggregateError::Overflow)
}

/// One aggregator per subscription, fed from a single trade stream.
#[derive(Debug, Clone, Default)]
pub struct Aggregators {
    items: Vec<Aggregator>,
}

/// Identifies one configured aggregation. Strategies subscribe by this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BarSubscription(u16);

impl BarSubscription {
    /// Names a subscription by its index.
    ///
    /// Registration hands one back, and that is where they normally come from.
    /// This exists so wiring read from configuration can name a subscription
    /// without the configuration having to run the registration first.
    #[inline]
    pub const fn from_index(index: u16) -> BarSubscription {
        BarSubscription(index)
    }

    /// The index into aggregator storage.
    #[inline]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// The underlying number.
    #[inline]
    pub const fn raw(self) -> u16 {
        self.0
    }
}

impl Aggregators {
    /// An empty set.
    pub fn new() -> Aggregators {
        Aggregators { items: Vec::new() }
    }

    /// Registers an aggregator and returns the subscription that names it.
    ///
    /// Called at session start only. Registering during a session would change
    /// the bar boundaries mid-run, which replay could not reproduce.
    pub fn push(&mut self, aggregator: Aggregator) -> BarSubscription {
        let id = BarSubscription(self.items.len() as u16);
        self.items.push(aggregator);
        id
    }

    /// How many aggregations are configured.
    #[inline]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether nothing is configured.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Feeds a trade to every aggregator following that instrument, pushing any
    /// completed bars onto `out`.
    ///
    /// The caller supplies the buffer and reuses it, so a session that has
    /// warmed it once never allocates here (Constitution VI).
    pub fn on_trade(
        &mut self,
        instrument: InstrumentId,
        px: Px,
        qty: Qty,
        at: ExchangeTime,
        out: &mut Vec<(BarSubscription, Bar)>,
    ) -> Result<(), AggregateError> {
        for (index, aggregator) in self.items.iter_mut().enumerate() {
            if aggregator.instrument() != instrument {
                continue;
            }
            if let Some(bar) = aggregator.on_trade(instrument, px, qty, at)? {
                out.push((BarSubscription(index as u16), bar));
            }
        }
        Ok(())
    }

    /// Flushes any time bar whose window has closed, pushing them onto `out`.
    pub fn on_time(&mut self, now: ExchangeTime, out: &mut Vec<(BarSubscription, Bar)>) {
        for (index, aggregator) in self.items.iter_mut().enumerate() {
            if let Some(bar) = aggregator.on_time(now) {
                out.push((BarSubscription(index as u16), bar));
            }
        }
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

    const I: InstrumentId = InstrumentId::new(0);

    fn volume_bars(threshold: i64) -> Aggregator {
        Aggregator::new(
            I,
            BarSpec::Volume {
                threshold: qty(threshold),
            },
        )
        .expect("valid spec")
    }

    #[test]
    fn a_specification_that_could_never_complete_is_rejected() {
        let rejected = [
            BarSpec::Tick { threshold: 0 },
            BarSpec::Time {
                period: ExchangeSpan::ZERO,
            },
            BarSpec::Time {
                period: ExchangeSpan::from_nanos(-1),
            },
            BarSpec::Volume {
                threshold: Qty::ZERO,
            },
            BarSpec::Dollar {
                threshold: Notional::ZERO,
            },
        ];
        for spec in rejected {
            assert_eq!(
                Aggregator::new(I, spec).err(),
                Some(AggregateError::InvalidSpec),
                "{spec:?} should not be constructible"
            );
        }
    }

    #[test]
    fn an_incomplete_bar_yields_nothing() {
        let mut agg = volume_bars(10);
        assert_eq!(agg.on_trade(I, px(100), qty(4), at(1)), Ok(None));
        assert_eq!(agg.on_trade(I, px(101), qty(5), at(2)), Ok(None));
        // Nine of ten traded. The bar exists but is unreachable.
        assert!(agg.is_forming());
    }

    #[test]
    fn a_volume_bar_completes_on_the_trade_that_reaches_the_threshold() {
        let mut agg = volume_bars(10);
        agg.on_trade(I, px(100), qty(4), at(1)).expect("fold");
        agg.on_trade(I, px(105), qty(5), at(2)).expect("fold");
        let bar = agg
            .on_trade(I, px(99), qty(3), at(3))
            .expect("fold")
            .expect("bar completed");

        // The triggering trade is inside the bar it completed.
        assert_eq!(bar.open, px(100));
        assert_eq!(bar.high, px(105));
        assert_eq!(bar.low, px(99));
        assert_eq!(bar.close, px(99));
        assert_eq!(bar.volume, qty(12));
        assert_eq!(bar.trades, 3);
        assert_eq!(bar.open_time, at(1));
        assert_eq!(bar.close_time, at(3));
        assert!(!agg.is_forming());
    }

    #[test]
    fn a_tick_bar_counts_prints() {
        let mut agg = Aggregator::new(I, BarSpec::Tick { threshold: 2 }).expect("spec");
        assert_eq!(agg.on_trade(I, px(1), qty(1), at(1)), Ok(None));
        let bar = agg
            .on_trade(I, px(2), qty(1), at(2))
            .expect("fold")
            .expect("bar");
        assert_eq!(bar.trades, 2);
    }

    #[test]
    fn a_dollar_bar_accumulates_truncated_traded_value() {
        // Price 2.0, quantity 3.0 -> value 6.0.
        let two = px(2_000_000_000);
        let three = qty(3_000_000_000);
        let mut agg = Aggregator::new(
            I,
            BarSpec::Dollar {
                threshold: Notional::from_scaled(6_000_000_000),
            },
        )
        .expect("spec");
        let bar = agg
            .on_trade(I, two, three, at(1))
            .expect("fold")
            .expect("bar");
        assert_eq!(bar.value, Notional::from_scaled(6_000_000_000));
    }

    #[test]
    fn a_time_bar_closes_at_its_window_boundary_not_at_the_trade_that_revealed_it() {
        let mut agg = Aggregator::new(
            I,
            BarSpec::Time {
                period: ExchangeSpan::from_nanos(1_000),
            },
        )
        .expect("spec");
        agg.on_trade(I, px(10), qty(1), at(100)).expect("fold");
        agg.on_trade(I, px(12), qty(1), at(900)).expect("fold");
        // This trade is in the next window, which proves the first one closed.
        let bar = agg
            .on_trade(I, px(20), qty(1), at(1_500))
            .expect("fold")
            .expect("bar");

        assert_eq!(bar.open, px(10));
        assert_eq!(bar.close, px(12));
        assert_eq!(bar.close_time, at(1_000));
        assert_eq!(bar.volume, qty(2));
        // The triggering trade opened the next bar rather than joining this one.
        assert!(agg.is_forming());
    }

    #[test]
    fn a_time_bar_can_be_flushed_by_a_timer_when_nothing_trades() {
        let mut agg = Aggregator::new(
            I,
            BarSpec::Time {
                period: ExchangeSpan::from_nanos(1_000),
            },
        )
        .expect("spec");
        agg.on_trade(I, px(10), qty(1), at(100)).expect("fold");
        assert_eq!(agg.on_time(at(999)), None);
        let bar = agg.on_time(at(1_000)).expect("window closed");
        assert_eq!(bar.close_time, at(1_000));
    }

    #[test]
    fn time_windows_align_to_the_epoch_so_two_runs_cut_identical_bars() {
        let period = ExchangeSpan::from_nanos(1_000);
        assert_eq!(window_end(at(0), period), Ok(at(1_000)));
        assert_eq!(window_end(at(999), period), Ok(at(1_000)));
        assert_eq!(window_end(at(1_000), period), Ok(at(2_000)));
        // Negative timestamps floor the same way rather than truncating toward
        // zero, which would make a window straddle the epoch.
        assert_eq!(window_end(at(-1), period), Ok(at(0)));
    }

    #[test]
    fn a_gap_skips_empty_windows_rather_than_emitting_phantom_bars() {
        let mut agg = Aggregator::new(
            I,
            BarSpec::Time {
                period: ExchangeSpan::from_nanos(1_000),
            },
        )
        .expect("spec");
        agg.on_trade(I, px(10), qty(1), at(100)).expect("fold");
        let bar = agg
            .on_trade(I, px(20), qty(1), at(50_000))
            .expect("fold")
            .expect("bar");
        assert_eq!(bar.close_time, at(1_000));
        // No bars were invented for the 48 windows nothing traded in.
        assert_eq!(agg.on_time(at(50_000)), None);
    }

    #[test]
    fn a_trade_stamped_before_the_bar_is_refused_without_disturbing_it() {
        let mut agg = volume_bars(100);
        agg.on_trade(I, px(10), qty(1), at(500)).expect("fold");
        assert_eq!(
            agg.on_trade(I, px(11), qty(1), at(499)),
            Err(AggregateError::OutOfOrder)
        );
        // The rejected trade left nothing behind: the next legal trade sees a
        // bar of exactly one print.
        let bar = agg
            .on_trade(I, px(12), qty(99), at(501))
            .expect("fold")
            .expect("bar");
        assert_eq!(bar.trades, 2);
        assert_eq!(bar.volume, qty(100));
    }

    #[test]
    fn accumulated_volume_that_would_overflow_is_refused_not_wrapped() {
        // A tick threshold high enough that only the volume check can fire.
        let mut agg = Aggregator::new(
            I,
            BarSpec::Tick {
                threshold: u32::MAX,
            },
        )
        .expect("spec");
        agg.on_trade(I, px(1), Qty::MAX, at(1)).expect("fold");
        assert_eq!(
            agg.on_trade(I, px(1), qty(1), at(2)),
            Err(AggregateError::Overflow)
        );
    }

    #[test]
    fn the_traded_value_accumulator_cannot_overflow() {
        // Why the value accumulator adds rather than checking: refusing the
        // volume overflow above bounds how much a bar can ever hold, and that
        // bound puts the widest possible traded value far inside `Notional`.
        // If this assertion ever fails, the plain `+` in `on_trade` has to
        // become a checked add.
        let max_px_whole = (i64::MAX / types::SCALE) as i128;
        let max_qty_whole = (i64::MAX / types::SCALE) as i128;
        let widest_value_scaled = max_px_whole * max_qty_whole * types::SCALE as i128;
        assert!(
            widest_value_scaled < i128::MAX / 2,
            "widest bar value {widest_value_scaled} is no longer safely inside Notional"
        );
    }

    #[test]
    fn an_aggregator_refuses_a_trade_in_an_instrument_it_does_not_follow() {
        let mut agg = volume_bars(10);
        assert_eq!(
            agg.on_trade(InstrumentId::new(1), px(1), qty(1), at(1)),
            Err(AggregateError::WrongInstrument)
        );
    }

    #[test]
    fn a_set_routes_each_trade_only_to_the_aggregators_that_follow_it() {
        let mut set = Aggregators::new();
        let first = set.push(volume_bars(2));
        let second = set
            .push(Aggregator::new(InstrumentId::new(1), BarSpec::Tick { threshold: 1 }).unwrap());
        let mut out = Vec::new();

        set.on_trade(I, px(10), qty(2), at(1), &mut out)
            .expect("fold");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, first);

        out.clear();
        set.on_trade(InstrumentId::new(1), px(10), qty(1), at(2), &mut out)
            .expect("fold");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, second);
    }
}
