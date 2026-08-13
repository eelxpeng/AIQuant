//! A reference strategy: price against its own moving average.
//!
//! It exists to prove the seam rather than to make money. What it demonstrates:
//!
//! - It reads only **completed bars**, so it cannot see a price before that
//!   price was final.
//! - It uses [`Context::now`] and never a clock.
//! - It tracks its own position from its own fills, because nothing hands a
//!   strategy the system's books.
//! - It compares `close × window` against the running sum rather than dividing
//!   to get an average. Exact, and no rounding decision to get wrong.

use crate::{Context, Strategy, StrategyEvent};
use event::OrderKind;
use marketdata::BarSubscription;
use oms::OrderState;
use std::collections::VecDeque;
use types::{InstrumentId, Px, Qty, Side, StrategyId};

/// Goes long while price is above its moving average and short while below.
#[derive(Debug, Clone)]
pub struct MovingAverageCrossover {
    id: StrategyId,
    instrument: InstrumentId,
    subscription: BarSubscription,
    window: usize,
    target_size: Qty,
    closes: VecDeque<Px>,
    /// Sum of the scale-unit counts in `closes`. Widened so a long window of
    /// high prices cannot overflow the accumulator.
    sum: i128,
    /// This strategy's own filled position, in scale units.
    position: i128,
    /// Quantity this strategy believes is working at the venue, signed.
    working: i128,
}

impl MovingAverageCrossover {
    /// Builds the strategy.
    ///
    /// `window` is how many completed bars the average covers, and
    /// `target_size` is the position magnitude it holds while a signal is on.
    /// Both are configuration; nothing here is tuned in code.
    pub fn new(
        id: StrategyId,
        instrument: InstrumentId,
        subscription: BarSubscription,
        window: usize,
        target_size: Qty,
    ) -> MovingAverageCrossover {
        MovingAverageCrossover {
            id,
            instrument,
            subscription,
            window: window.max(1),
            target_size,
            closes: VecDeque::with_capacity(window.max(1) + 1),
            sum: 0,
            position: 0,
            working: 0,
        }
    }

    /// The position this strategy's own fills add up to.
    #[inline]
    pub fn position(&self) -> Qty {
        Qty::from_scaled(clamp_to_qty(self.position))
    }

    /// The signed quantity it believes is still working at the venue.
    #[inline]
    pub fn working(&self) -> Qty {
        Qty::from_scaled(clamp_to_qty(self.working))
    }

    /// Whether enough bars have completed for the average to mean anything.
    #[inline]
    pub fn is_warm(&self) -> bool {
        self.closes.len() >= self.window
    }

    /// The position the current signal calls for, or `None` before warm-up and
    /// when price sits exactly on its average.
    fn target(&self, close: Px) -> Option<i128> {
        if !self.is_warm() {
            return None;
        }
        // close × window against the sum: the comparison an average would make,
        // without the division that an average would need.
        let scaled = close.to_scaled() as i128 * self.window as i128;
        let size = self.target_size.to_scaled() as i128;
        match scaled.cmp(&self.sum) {
            std::cmp::Ordering::Greater => Some(size),
            std::cmp::Ordering::Less => Some(-size),
            // Exactly on the average is not a signal. Compared exactly, never
            // with a tolerance (Constitution VII).
            std::cmp::Ordering::Equal => None,
        }
    }

    fn push_close(&mut self, close: Px) {
        self.closes.push_back(close);
        self.sum += close.to_scaled() as i128;
        while self.closes.len() > self.window {
            let dropped = self.closes.pop_front().expect("non-empty");
            self.sum -= dropped.to_scaled() as i128;
        }
    }
}

impl Strategy for MovingAverageCrossover {
    fn id(&self) -> StrategyId {
        self.id
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
        match *event {
            StrategyEvent::Bar { subscription, bar } => {
                if subscription != self.subscription {
                    return;
                }
                self.push_close(bar.close);
                let Some(target) = self.target(bar.close) else {
                    return;
                };

                // Order only the difference from where it already is, counting
                // what is still working. Without that, every bar would re-send
                // the whole position while the first order was still in flight.
                let delta = target - (self.position + self.working);
                if delta == 0 {
                    return;
                }
                let side = if delta > 0 { Side::Buy } else { Side::Sell };
                let qty = Qty::from_scaled(clamp_to_qty(delta.abs()));
                if qty.to_scaled() == 0 {
                    return;
                }
                ctx.order(self.instrument, side, qty, OrderKind::Market);
                self.working += delta.signum() * qty.to_scaled() as i128;
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
                let signed = side.sign() as i128 * qty.to_scaled() as i128;
                self.position += signed;
                self.working -= signed;
            }

            StrategyEvent::IntentRefused {
                instrument,
                side,
                qty,
                ..
            } => {
                if instrument != self.instrument {
                    return;
                }
                // Nothing was created, so nothing is working.
                self.working -= side.sign() as i128 * qty.to_scaled() as i128;
            }

            StrategyEvent::OrderDone {
                instrument,
                side,
                state,
                unfilled,
                ..
            } => {
                if instrument != self.instrument || state == OrderState::Filled {
                    return;
                }
                // Whatever will never execute is no longer working.
                self.working -= side.sign() as i128 * unfilled.to_scaled() as i128;
            }

            StrategyEvent::Quote { .. }
            | StrategyEvent::Trade { .. }
            | StrategyEvent::Timer { .. } => {}
        }
    }
}

/// Brings a widened count back into `Qty`'s range without wrapping.
///
/// Saturating rather than wrapping because this is the strategy's own estimate,
/// not the books: a wrapped estimate would flip a long into a short and order
/// the wrong way, whereas a saturated one merely stops growing. The
/// authoritative position lives in `oms` and the bound that matters is enforced
/// by the risk gate.
#[inline]
fn clamp_to_qty(value: i128) -> i64 {
    value.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use event::Intent;
    use marketdata::Bar;
    use types::{ExchangeTime, Notional, OrderId, Timestamp};

    const SCALE: i64 = types::SCALE;
    const INSTRUMENT: InstrumentId = InstrumentId::new(0);
    const SUB: BarSubscription = BarSubscription::from_index(0);

    fn px(whole: i64) -> Px {
        Px::from_scaled(whole * SCALE)
    }

    fn qty(whole: i64) -> Qty {
        Qty::from_scaled(whole * SCALE)
    }

    fn strategy() -> MovingAverageCrossover {
        MovingAverageCrossover::new(StrategyId::new(0), INSTRUMENT, SUB, 3, qty(10))
    }

    fn bar(close: i64) -> Bar {
        Bar {
            instrument: INSTRUMENT,
            open: px(close),
            high: px(close),
            low: px(close),
            close: px(close),
            volume: qty(1),
            trades: 1,
            value: Notional::ZERO,
            open_time: Timestamp::from_nanos(0),
            close_time: Timestamp::from_nanos(1),
        }
    }

    /// Runs one event and returns whatever intents it produced.
    fn feed(s: &mut MovingAverageCrossover, event: &StrategyEvent<'_>) -> Vec<Intent> {
        let mut intents = Vec::new();
        let mut timers = Vec::new();
        let mut ctx = Context::new(
            s.id(),
            ExchangeTime::from_nanos(1),
            &mut intents,
            &mut timers,
        );
        s.on_event(event, &mut ctx);
        intents
    }

    fn feed_bar(s: &mut MovingAverageCrossover, close: i64) -> Vec<Intent> {
        let b = bar(close);
        feed(
            s,
            &StrategyEvent::Bar {
                subscription: SUB,
                bar: &b,
            },
        )
    }

    #[test]
    fn nothing_is_ordered_before_the_average_has_enough_bars() {
        let mut s = strategy();
        assert!(feed_bar(&mut s, 100).is_empty());
        assert!(feed_bar(&mut s, 101).is_empty());
        assert!(!s.is_warm());
    }

    #[test]
    fn a_close_above_the_average_opens_a_long() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        let intents = feed_bar(&mut s, 130);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Buy);
        assert_eq!(intents[0].qty, qty(10));
        assert!(!intents[0].reduce_only);
    }

    #[test]
    fn a_close_below_the_average_opens_a_short() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        let intents = feed_bar(&mut s, 70);
        assert_eq!(intents[0].side, Side::Sell);
        assert_eq!(intents[0].qty, qty(10));
    }

    #[test]
    fn a_close_exactly_on_the_average_is_not_a_signal() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        // Compared exactly: 100 x 3 equals the sum of three hundreds.
        assert!(feed_bar(&mut s, 100).is_empty());
    }

    #[test]
    fn a_repeated_signal_does_not_re_order_what_is_already_working() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        assert_eq!(feed_bar(&mut s, 130).len(), 1);
        // The first order has not filled yet; the signal is unchanged.
        assert!(feed_bar(&mut s, 140).is_empty());
        assert_eq!(s.working(), qty(10));
        assert_eq!(s.position(), Qty::ZERO);
    }

    #[test]
    fn a_fill_moves_quantity_from_working_to_position() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 130);
        feed(
            &mut s,
            &StrategyEvent::Fill {
                order: OrderId::new(0),
                instrument: INSTRUMENT,
                side: Side::Buy,
                px: px(130),
                qty: qty(10),
                fee: Notional::ZERO,
            },
        );
        assert_eq!(s.position(), qty(10));
        assert_eq!(s.working(), Qty::ZERO);
    }

    #[test]
    fn a_reversal_orders_the_whole_distance_between_the_two_targets() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 130);
        feed(
            &mut s,
            &StrategyEvent::Fill {
                order: OrderId::new(0),
                instrument: INSTRUMENT,
                side: Side::Buy,
                px: px(130),
                qty: qty(10),
                fee: Notional::ZERO,
            },
        );
        // Price collapses: the average is now above the close.
        let intents = feed_bar(&mut s, 1);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].side, Side::Sell);
        // From +10 to -10 is twenty units, not ten.
        assert_eq!(intents[0].qty, qty(20));
    }

    #[test]
    fn a_cancelled_order_releases_the_quantity_it_will_never_fill() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 130);
        assert_eq!(s.working(), qty(10));

        feed(
            &mut s,
            &StrategyEvent::OrderDone {
                order: OrderId::new(0),
                instrument: INSTRUMENT,
                side: Side::Buy,
                state: OrderState::Cancelled,
                unfilled: qty(10),
            },
        );
        assert_eq!(s.working(), Qty::ZERO);

        // With nothing working and nothing held, the signal orders again.
        let intents = feed_bar(&mut s, 140);
        assert_eq!(intents.len(), 1);
        assert_eq!(intents[0].qty, qty(10));
    }

    #[test]
    fn a_partly_filled_cancel_releases_only_the_unfilled_part() {
        let mut s = strategy();
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 130);
        feed(
            &mut s,
            &StrategyEvent::Fill {
                order: OrderId::new(0),
                instrument: INSTRUMENT,
                side: Side::Buy,
                px: px(130),
                qty: qty(4),
                fee: Notional::ZERO,
            },
        );
        feed(
            &mut s,
            &StrategyEvent::OrderDone {
                order: OrderId::new(0),
                instrument: INSTRUMENT,
                side: Side::Buy,
                state: OrderState::Cancelled,
                unfilled: qty(6),
            },
        );
        assert_eq!(s.position(), qty(4));
        assert_eq!(s.working(), Qty::ZERO);
    }

    #[test]
    fn events_for_other_instruments_and_subscriptions_are_ignored() {
        let mut s = strategy();
        let other = bar(500);
        assert!(
            feed(
                &mut s,
                &StrategyEvent::Bar {
                    subscription: BarSubscription::from_index(7),
                    bar: &other,
                }
            )
            .is_empty()
        );
        assert!(!s.is_warm());

        feed(
            &mut s,
            &StrategyEvent::Fill {
                order: OrderId::new(0),
                instrument: InstrumentId::new(9),
                side: Side::Buy,
                px: px(1),
                qty: qty(1),
                fee: Notional::ZERO,
            },
        );
        assert_eq!(s.position(), Qty::ZERO);
    }

    #[test]
    fn the_average_covers_only_the_configured_window() {
        let mut s = strategy();
        // Three bars of 100, then one very high bar rolls the oldest out.
        for _ in 0..3 {
            feed_bar(&mut s, 100);
        }
        assert!(s.is_warm());
        assert_eq!(s.closes.len(), 3);
        feed_bar(&mut s, 200);
        assert_eq!(s.closes.len(), 3);
        assert_eq!(s.sum, 400i128 * SCALE as i128);
    }
}

#[cfg(test)]
mod refusals {
    //! A strategy that tracks in-flight quantity has to survive a refusal.
    //!
    //! This is the bug a live session found: every test before it passed
    //! because no limit ever bound, and the moment one did the strategy went
    //! quietly wrong and stayed wrong.

    use super::*;
    use crate::{Context, StrategyEvent};
    use event::{Intent, RiskReason};
    use marketdata::Bar;
    use types::{ExchangeTime, Notional, Timestamp};

    const SCALE: i64 = types::SCALE;
    const INSTRUMENT: InstrumentId = InstrumentId::new(0);
    const SUB: BarSubscription = BarSubscription::from_index(0);

    fn bar(close: i64) -> Bar {
        Bar {
            instrument: INSTRUMENT,
            open: Px::from_scaled(close * SCALE),
            high: Px::from_scaled(close * SCALE),
            low: Px::from_scaled(close * SCALE),
            close: Px::from_scaled(close * SCALE),
            volume: Qty::from_scaled(SCALE),
            trades: 1,
            value: Notional::ZERO,
            open_time: Timestamp::from_nanos(0),
            close_time: Timestamp::from_nanos(1),
        }
    }

    fn deliver(s: &mut MovingAverageCrossover, event: &StrategyEvent<'_>) -> Vec<Intent> {
        let mut intents = Vec::new();
        let mut timers = Vec::new();
        let mut ctx = Context::new(
            s.id(),
            ExchangeTime::from_nanos(1),
            &mut intents,
            &mut timers,
        );
        s.on_event(event, &mut ctx);
        intents
    }

    fn feed_bar(s: &mut MovingAverageCrossover, close: i64) -> Vec<Intent> {
        let b = bar(close);
        deliver(
            s,
            &StrategyEvent::Bar {
                subscription: SUB,
                bar: &b,
            },
        )
    }

    fn warmed() -> MovingAverageCrossover {
        let mut s = MovingAverageCrossover::new(
            StrategyId::new(0),
            INSTRUMENT,
            SUB,
            3,
            Qty::from_scaled(10 * SCALE),
        );
        feed_bar(&mut s, 100);
        feed_bar(&mut s, 100);
        s
    }

    #[test]
    fn a_refused_intent_releases_the_quantity_it_was_never_going_to_fill() {
        let mut s = warmed();
        let intents = feed_bar(&mut s, 130);
        assert_eq!(intents.len(), 1);
        assert_eq!(s.working(), Qty::from_scaled(10 * SCALE));

        deliver(
            &mut s,
            &StrategyEvent::IntentRefused {
                instrument: INSTRUMENT,
                side: intents[0].side,
                qty: intents[0].qty,
                reason: RiskReason::StaleMarketData,
            },
        );
        assert_eq!(
            s.working(),
            Qty::ZERO,
            "nothing was created, so nothing is working"
        );
    }

    #[test]
    fn after_a_refusal_the_signal_is_acted_on_again() {
        // The symptom the live session showed: without the refusal reaching the
        // strategy, it believes its order is live and never re-orders.
        let mut s = warmed();
        let intents = feed_bar(&mut s, 130);
        deliver(
            &mut s,
            &StrategyEvent::IntentRefused {
                instrument: INSTRUMENT,
                side: intents[0].side,
                qty: intents[0].qty,
                reason: RiskReason::StaleMarketData,
            },
        );

        let again = feed_bar(&mut s, 140);
        assert_eq!(again.len(), 1, "the signal is unchanged, so it asks again");
        assert_eq!(again[0].qty, Qty::from_scaled(10 * SCALE));
    }

    #[test]
    fn a_refusal_for_another_instrument_is_ignored() {
        let mut s = warmed();
        let intents = feed_bar(&mut s, 130);
        deliver(
            &mut s,
            &StrategyEvent::IntentRefused {
                instrument: InstrumentId::new(9),
                side: intents[0].side,
                qty: intents[0].qty,
                reason: RiskReason::StaleMarketData,
            },
        );
        assert_eq!(s.working(), Qty::from_scaled(10 * SCALE));
    }
}
