//! The risk gate: the one path from intent to order.
//!
//! There is no second path — not for tests, not for manual override, not for
//! "just this strategy" (`docs/ARCHITECTURE.md` seam 3). Everything that
//! becomes an order comes through [`RiskGate::check`].
//!
//! # What reduce-only bypasses
//!
//! A reduce-only order can only decrease `|position|`, so permitting one is
//! strictly less risky than blocking it. The rule this module applies:
//!
//! > Reduce-only bypasses every check about **how much risk is being taken**,
//! > and binds on every check about **whether the order is well-formed or would
//! > harm the venue connection**.
//!
//! Bypassed: halted state, unreconciled position, stale or absent market data,
//! position limit, exposure limit, single-order notional limit.
//! Still binding: the kill switch, unknown instrument, missing limit config,
//! non-positive price or quantity, the venue's minimum quantity, and the order
//! rate limit.
//!
//! Two of those deserve their reasons stated. The **kill switch** binds because
//! it means "this system is wrong, stop it" — and if the system's own position
//! accounting is what is wrong, letting it emit more orders makes the incident
//! worse. The **rate limit** binds because an unbounded order rate is how a
//! venue disconnects you, which traps the operator just as thoroughly as a
//! refused order.
//!
//! This is the semantics `docs/ARCHITECTURE.md` leaves open under "Reduce-only
//! semantics". It is a decision made here, not a ratified one.

use crate::limits::LimitBook;
use event::{EngineState, Intent, OrderKind, RiskReason};
use std::collections::VecDeque;
use types::{
    ExchangeSpan, ExchangeTime, Instrument, InstrumentId, Px, Qty, RoundDir, Side, StrategyId,
};

/// Everything the gate needs to know that it does not own.
///
/// Passed in rather than looked up, so this crate depends on nothing but the
/// event alphabet — it does not reach sideways into `marketdata` or `oms`
/// (Constitution I). It also makes every check testable without building a
/// book or a position.
#[derive(Debug, Clone, Copy)]
pub struct GateInput {
    /// Where the engine is.
    pub state: EngineState,
    /// The exchange time of the event being processed.
    pub now: ExchangeTime,
    /// The venue's conventions for this instrument.
    pub instrument: Instrument,
    /// The system's own signed position.
    pub position: Qty,
    /// Whether that position agrees with the venue's.
    pub reconciled: bool,
    /// How old the top of book is. `None` when none has arrived — a different
    /// condition from "arrived long ago", and never collapsed into it.
    pub quote_age: Option<ExchangeSpan>,
    /// The valuation price under the configured rule, if one is computable.
    pub mark: Option<Px>,
    /// What a taker on this intent's side would pay right now.
    pub taker_px: Option<Px>,
}

/// What the gate decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Decision {
    /// The intent may become an order, on these terms.
    Approved(Approved),
    /// The intent does not become an order, for this reason.
    Rejected(RiskReason),
}

/// An intent the gate approved, with venue conventions already applied.
///
/// The gate is the only thing that rounds to a tick or a lot, so by the time an
/// order exists its numbers are already acceptable to the venue
/// (Constitution VII).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Approved {
    /// Who asked.
    pub strategy: StrategyId,
    /// What to trade.
    pub instrument: InstrumentId,
    /// Which way.
    pub side: Side,
    /// How much, rounded down to the venue's lot.
    pub qty: Qty,
    /// Market, or limit at a price rounded to the venue's tick.
    pub kind: OrderKind,
    /// Whether this may only decrease `|position|`.
    pub reduce_only: bool,
}

/// Quantities round **down** to the lot: never trade more than was asked for.
const QTY_ROUNDING: RoundDir = RoundDir::Down;

/// Exposure and order value round **away from zero**: never understate size
/// when comparing against a bound.
const SIZE_ROUNDING: RoundDir = RoundDir::AwayFromZero;

/// The single mandatory checkpoint every intent passes to become an order.
#[derive(Debug, Clone)]
pub struct RiskGate {
    limits: LimitBook,
    /// Per instrument, the exchange times of recently approved orders.
    recent: Vec<VecDeque<ExchangeTime>>,
}

impl RiskGate {
    /// A gate enforcing `limits`.
    ///
    /// The rate windows are sized from each instrument's configured maximum, so
    /// a warmed session never allocates while checking.
    pub fn new(limits: LimitBook) -> RiskGate {
        let recent = (0..limits.len())
            .map(|i| {
                let capacity = limits
                    .get(InstrumentId::new(i as u32))
                    .map(|l| l.max_orders_in_window as usize + 1)
                    .unwrap_or(0);
                VecDeque::with_capacity(capacity)
            })
            .collect();
        RiskGate { limits, recent }
    }

    /// The bounds this gate enforces.
    #[inline]
    pub fn limits(&self) -> &LimitBook {
        &self.limits
    }

    /// Decides whether an intent may become an order.
    ///
    /// Checks run most-severe first, so the reason recorded in the log is the
    /// most important one rather than whichever happened to be evaluated first.
    pub fn check(&mut self, intent: &Intent, input: &GateInput) -> Decision {
        // The kill switch outranks everything, including reduce-only.
        if !input.state.permits_any_order() {
            return Decision::Rejected(RiskReason::KillSwitchEngaged);
        }
        if intent.instrument != input.instrument.id() {
            return Decision::Rejected(RiskReason::UnknownInstrument);
        }
        let Some(limits) = self.limits.get(intent.instrument).copied() else {
            return Decision::Rejected(RiskReason::NoLimitsConfigured);
        };

        // Well-formedness. These bind regardless of reduce-only.
        if intent.qty.to_scaled() <= 0 {
            return Decision::Rejected(RiskReason::NonPositiveQuantity);
        }
        if let OrderKind::Limit(px) = intent.kind
            && px.to_scaled() <= 0
        {
            return Decision::Rejected(RiskReason::NonPositivePrice);
        }

        let reduce_only = intent.reduce_only;
        let mut qty = intent.qty;

        if reduce_only {
            // A reduce-only order that does not reduce is a contradiction, not
            // a small order.
            if Side::reducing(input.position) != Some(intent.side) {
                return Decision::Rejected(RiskReason::NotReducing);
            }
            // Clamp to what is actually held, so "reduce-only" cannot overshoot
            // into a position on the other side. The log records the clamped
            // quantity, so the difference from the intent is visible.
            let held = Qty::from_scaled(magnitude_i64(input.position));
            if qty.to_scaled() > held.to_scaled() {
                qty = held;
            }
        } else {
            if !input.state.permits_risk_increase() {
                return Decision::Rejected(RiskReason::Halted);
            }
            if !input.reconciled {
                return Decision::Rejected(RiskReason::UnreconciledPosition);
            }
            match input.quote_age {
                None => return Decision::Rejected(RiskReason::NoMarketData),
                Some(age) if age.to_nanos() > limits.max_quote_age.to_nanos() => {
                    return Decision::Rejected(RiskReason::StaleMarketData);
                }
                Some(_) => {}
            }
        }

        // Venue conventions.
        let Ok(qty) = input.instrument.round_qty(qty, QTY_ROUNDING) else {
            return Decision::Rejected(RiskReason::Unrepresentable);
        };
        if qty.to_scaled() <= 0 || !input.instrument.meets_min_qty(qty) {
            return Decision::Rejected(RiskReason::BelowMinimumQty);
        }
        let kind = match intent.kind {
            OrderKind::Market => OrderKind::Market,
            OrderKind::Limit(px) => {
                // Passive rounding: a buy rounds down and a sell rounds up, so
                // rounding never pushes the quote through the asked price.
                match input
                    .instrument
                    .round_price(px, intent.side.passive_rounding())
                {
                    Ok(rounded) if rounded.to_scaled() > 0 => OrderKind::Limit(rounded),
                    Ok(_) => return Decision::Rejected(RiskReason::NonPositivePrice),
                    Err(_) => return Decision::Rejected(RiskReason::Unrepresentable),
                }
            }
        };

        // Size bounds. Skipped for reduce-only: every one of them is a
        // statement about taking risk, and this order only sheds it.
        if !reduce_only {
            let reference = match kind.limit_px().or(input.taker_px).or(input.mark) {
                Some(px) => px,
                None => return Decision::Rejected(RiskReason::NoMarketData),
            };
            let order_value = reference.notional(qty, SIZE_ROUNDING);
            if magnitude_i128(order_value.to_scaled())
                > magnitude_i128(limits.max_order_notional.to_scaled())
            {
                return Decision::Rejected(RiskReason::OrderNotionalLimit);
            }

            let Ok(signed) = signed_qty(intent.side, qty) else {
                return Decision::Rejected(RiskReason::Unrepresentable);
            };
            let Ok(after) = input.position.checked_add(signed) else {
                return Decision::Rejected(RiskReason::PositionLimit);
            };
            if magnitude_i64(after) > magnitude_i64(limits.max_position) {
                return Decision::Rejected(RiskReason::PositionLimit);
            }

            let Some(mark) = input.mark else {
                return Decision::Rejected(RiskReason::NoMarketData);
            };
            let exposure = mark.notional(after, SIZE_ROUNDING);
            if magnitude_i128(exposure.to_scaled())
                > magnitude_i128(limits.max_exposure.to_scaled())
            {
                return Decision::Rejected(RiskReason::ExposureLimit);
            }
        }

        // The venue-protection bound, which binds on everything.
        if !self.rate_permits(intent.instrument, input.now, &limits) {
            return Decision::Rejected(RiskReason::OrderRateLimit);
        }
        self.record(intent.instrument, input.now);

        Decision::Approved(Approved {
            strategy: intent.strategy,
            instrument: intent.instrument,
            side: intent.side,
            qty,
            kind,
            reduce_only,
        })
    }

    /// Whether another order fits inside the instrument's rate window.
    fn rate_permits(
        &mut self,
        instrument: InstrumentId,
        now: ExchangeTime,
        limits: &crate::Limits,
    ) -> bool {
        let Some(window) = self.recent.get_mut(instrument.index()) else {
            return false;
        };
        // A window that cannot be computed is not a reason to allow the order.
        let Ok(cutoff) = now.checked_sub(limits.rate_window) else {
            return false;
        };
        while window.front().is_some_and(|t| *t < cutoff) {
            window.pop_front();
        }
        (window.len() as u32) < limits.max_orders_in_window
    }

    fn record(&mut self, instrument: InstrumentId, now: ExchangeTime) {
        if let Some(window) = self.recent.get_mut(instrument.index()) {
            window.push_back(now);
        }
    }
}

/// `|value|`, widened so that `i64::MIN` has an answer.
#[inline]
fn magnitude_i64(value: Qty) -> i64 {
    // Saturating rather than wrapping: `-i64::MIN` is not representable, and a
    // wrapped magnitude would compare as negative and pass every bound.
    value.to_scaled().saturating_abs()
}

/// `|value|` for the wide type.
#[inline]
fn magnitude_i128(value: i128) -> i128 {
    value.saturating_abs()
}

fn signed_qty(side: Side, qty: Qty) -> Result<Qty, types::ValueError> {
    match side {
        Side::Buy => Ok(qty),
        Side::Sell => qty.checked_neg(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Limits;
    use types::Notional;

    const SCALE: i64 = types::SCALE;

    fn px(whole: i64) -> Px {
        Px::from_scaled(whole * SCALE)
    }

    fn qty(whole: i64) -> Qty {
        Qty::from_scaled(whole * SCALE)
    }

    fn money(whole: i64) -> Notional {
        Notional::from_scaled(whole as i128 * SCALE as i128)
    }

    fn nanos(n: i64) -> ExchangeSpan {
        ExchangeSpan::from_nanos(n as i128)
    }

    fn at(n: i64) -> ExchangeTime {
        ExchangeTime::from_nanos(n)
    }

    const ID: InstrumentId = InstrumentId::new(0);

    fn instrument() -> Instrument {
        // Tick 0.01, lot 1, minimum 1.
        Instrument::new(ID, Px::from_scaled(10_000_000), qty(1), qty(1)).expect("conventions")
    }

    fn limits() -> Limits {
        Limits {
            max_position: qty(100),
            max_exposure: money(1_000_000),
            max_order_notional: money(10_000),
            max_orders_in_window: 3,
            rate_window: nanos(1_000),
            max_quote_age: nanos(500),
        }
    }

    fn gate() -> RiskGate {
        let mut book = LimitBook::with_instruments(1);
        book.set(ID, limits()).expect("limits");
        RiskGate::new(book)
    }

    fn healthy() -> GateInput {
        GateInput {
            state: EngineState::Running,
            now: at(10_000),
            instrument: instrument(),
            position: Qty::ZERO,
            reconciled: true,
            quote_age: Some(nanos(10)),
            mark: Some(px(100)),
            taker_px: Some(px(100)),
        }
    }

    fn buy(n: i64) -> Intent {
        Intent {
            strategy: StrategyId::new(0),
            instrument: ID,
            side: Side::Buy,
            qty: qty(n),
            kind: OrderKind::Market,
            reduce_only: false,
        }
    }

    fn reduce(side: Side, n: i64) -> Intent {
        Intent {
            strategy: StrategyId::new(0),
            instrument: ID,
            side,
            qty: qty(n),
            kind: OrderKind::Market,
            reduce_only: true,
        }
    }

    fn reason(d: Decision) -> Option<RiskReason> {
        match d {
            Decision::Rejected(r) => Some(r),
            Decision::Approved(_) => None,
        }
    }

    fn approved(d: Decision) -> Approved {
        match d {
            Decision::Approved(a) => a,
            Decision::Rejected(r) => panic!("expected approval, got {r:?}"),
        }
    }

    #[test]
    fn a_healthy_intent_becomes_an_order() {
        let a = approved(gate().check(&buy(5), &healthy()));
        assert_eq!(a.qty, qty(5));
        assert_eq!(a.side, Side::Buy);
        assert!(!a.reduce_only);
    }

    #[test]
    fn the_kill_switch_refuses_everything_including_reduce_only() {
        let input = GateInput {
            state: EngineState::Killed,
            position: qty(10),
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&buy(1), &input)),
            Some(RiskReason::KillSwitchEngaged)
        );
        assert_eq!(
            reason(gate().check(&reduce(Side::Sell, 1), &input)),
            Some(RiskReason::KillSwitchEngaged)
        );
    }

    #[test]
    fn a_halt_refuses_new_risk_but_still_lets_the_operator_out() {
        let input = GateInput {
            state: EngineState::Halted,
            position: qty(10),
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&buy(1), &input)),
            Some(RiskReason::Halted)
        );
        let a = approved(gate().check(&reduce(Side::Sell, 10), &input));
        assert_eq!(a.qty, qty(10));
    }

    #[test]
    fn an_instrument_without_limits_cannot_be_traded() {
        let mut bare = RiskGate::new(LimitBook::with_instruments(1));
        assert_eq!(
            reason(bare.check(&buy(1), &healthy())),
            Some(RiskReason::NoLimitsConfigured)
        );
    }

    #[test]
    fn an_intent_naming_a_different_instrument_than_the_context_is_refused() {
        let mut intent = buy(1);
        intent.instrument = InstrumentId::new(9);
        assert_eq!(
            reason(gate().check(&intent, &healthy())),
            Some(RiskReason::UnknownInstrument)
        );
    }

    #[test]
    fn a_non_positive_quantity_or_price_is_refused() {
        let mut zero = buy(0);
        zero.qty = Qty::ZERO;
        assert_eq!(
            reason(gate().check(&zero, &healthy())),
            Some(RiskReason::NonPositiveQuantity)
        );

        let mut negative_px = buy(1);
        negative_px.kind = OrderKind::Limit(px(-1));
        assert_eq!(
            reason(gate().check(&negative_px, &healthy())),
            Some(RiskReason::NonPositivePrice)
        );
    }

    #[test]
    fn stale_market_data_refuses_a_normal_order_and_permits_a_reduce_only_one() {
        let input = GateInput {
            quote_age: Some(nanos(501)),
            position: qty(10),
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&buy(1), &input)),
            Some(RiskReason::StaleMarketData)
        );
        approved(gate().check(&reduce(Side::Sell, 5), &input));
    }

    #[test]
    fn absent_market_data_is_a_different_refusal_from_stale_data() {
        let input = GateInput {
            quote_age: None,
            mark: None,
            taker_px: None,
            position: qty(10),
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&buy(1), &input)),
            Some(RiskReason::NoMarketData)
        );
        // Getting out does not require a price to get out at.
        approved(gate().check(&reduce(Side::Sell, 5), &input));
    }

    #[test]
    fn an_unreconciled_position_refuses_new_risk_and_permits_getting_out() {
        let input = GateInput {
            reconciled: false,
            position: qty(10),
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&buy(1), &input)),
            Some(RiskReason::UnreconciledPosition)
        );
        approved(gate().check(&reduce(Side::Sell, 10), &input));
    }

    #[test]
    fn the_position_limit_binds_on_the_resulting_position_not_the_order() {
        let input = GateInput {
            position: qty(98),
            ..healthy()
        };
        approved(gate().check(&buy(2), &input));
        assert_eq!(
            reason(gate().check(&buy(3), &input)),
            Some(RiskReason::PositionLimit)
        );
    }

    #[test]
    fn the_position_limit_binds_on_magnitude_so_a_short_is_bounded_too() {
        let input = GateInput {
            position: qty(-98),
            ..healthy()
        };
        let mut sell = buy(3);
        sell.side = Side::Sell;
        assert_eq!(
            reason(gate().check(&sell, &input)),
            Some(RiskReason::PositionLimit)
        );
    }

    #[test]
    fn a_single_order_larger_than_its_notional_bound_is_refused() {
        // 101 units at 100 is 10,100, past the 10,000 bound.
        assert_eq!(
            reason(gate().check(&buy(101), &healthy())),
            Some(RiskReason::OrderNotionalLimit)
        );
    }

    #[test]
    fn the_exposure_bound_refuses_a_position_that_is_small_but_expensive() {
        let mut book = LimitBook::with_instruments(1);
        book.set(
            ID,
            Limits {
                max_exposure: money(100),
                ..limits()
            },
        )
        .expect("limits");
        let mut narrow = RiskGate::new(book);
        // Two units at 100 is 200 of exposure against a bound of 100.
        assert_eq!(
            reason(narrow.check(&buy(2), &healthy())),
            Some(RiskReason::ExposureLimit)
        );
    }

    #[test]
    fn the_order_rate_bound_binds_even_on_reduce_only_orders() {
        let mut g = gate();
        let input = GateInput {
            position: qty(50),
            ..healthy()
        };
        for _ in 0..3 {
            approved(g.check(&reduce(Side::Sell, 1), &input));
        }
        assert_eq!(
            reason(g.check(&reduce(Side::Sell, 1), &input)),
            Some(RiskReason::OrderRateLimit)
        );
    }

    #[test]
    fn the_rate_window_slides_so_the_bound_recovers() {
        let mut g = gate();
        for _ in 0..3 {
            approved(g.check(&buy(1), &healthy()));
        }
        assert_eq!(
            reason(g.check(&buy(1), &healthy())),
            Some(RiskReason::OrderRateLimit)
        );
        let later = GateInput {
            now: at(20_000),
            ..healthy()
        };
        approved(g.check(&buy(1), &later));
    }

    #[test]
    fn a_reduce_only_order_on_the_wrong_side_is_refused() {
        let input = GateInput {
            position: qty(10),
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&reduce(Side::Buy, 1), &input)),
            Some(RiskReason::NotReducing)
        );
    }

    #[test]
    fn a_reduce_only_order_against_a_flat_position_is_refused() {
        // The degenerate case: there is nothing to reduce, so neither side is
        // the reducing one.
        assert_eq!(
            reason(gate().check(&reduce(Side::Sell, 1), &healthy())),
            Some(RiskReason::NotReducing)
        );
    }

    #[test]
    fn a_reduce_only_order_is_clamped_to_what_is_actually_held() {
        let input = GateInput {
            position: qty(7),
            ..healthy()
        };
        let a = approved(gate().check(&reduce(Side::Sell, 100), &input));
        // Asked to sell 100 against a position of 7; it cannot flip the sign.
        assert_eq!(a.qty, qty(7));
    }

    #[test]
    fn quantities_round_down_to_the_lot_so_no_order_exceeds_what_was_asked() {
        let mut intent = buy(1);
        intent.qty = Qty::from_scaled(3 * SCALE / 2); // 1.5 units, lot is 1.
        let a = approved(gate().check(&intent, &healthy()));
        assert_eq!(a.qty, qty(1));
    }

    #[test]
    fn an_order_that_rounds_away_to_nothing_is_refused() {
        let mut intent = buy(1);
        intent.qty = Qty::from_scaled(SCALE / 2); // half a lot
        assert_eq!(
            reason(gate().check(&intent, &healthy())),
            Some(RiskReason::BelowMinimumQty)
        );
    }

    #[test]
    fn limit_prices_round_passively_so_rounding_never_crosses_the_asked_price() {
        // Tick is 0.01. A buy at 100.005 rounds down to 100.00.
        let mut bid = buy(1);
        bid.kind = OrderKind::Limit(Px::from_scaled(100_005_000_000));
        assert_eq!(
            approved(gate().check(&bid, &healthy())).kind,
            OrderKind::Limit(Px::from_scaled(100_000_000_000))
        );

        // A sell at the same price rounds up to 100.01.
        let mut ask = bid;
        ask.side = Side::Sell;
        assert_eq!(
            approved(gate().check(&ask, &healthy())).kind,
            OrderKind::Limit(Px::from_scaled(100_010_000_000))
        );
    }

    #[test]
    fn a_limit_price_that_rounds_to_zero_is_refused() {
        let mut intent = buy(1);
        // Half a tick, rounding down for a buy, lands on zero.
        intent.kind = OrderKind::Limit(Px::from_scaled(5_000_000));
        assert_eq!(
            reason(gate().check(&intent, &healthy())),
            Some(RiskReason::NonPositivePrice)
        );
    }

    #[test]
    fn a_position_that_would_overflow_is_refused_rather_than_wrapped() {
        let input = GateInput {
            position: Qty::MAX,
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&buy(1), &input)),
            Some(RiskReason::PositionLimit)
        );
    }

    #[test]
    fn a_rate_window_that_cannot_be_computed_refuses_rather_than_allows() {
        let input = GateInput {
            now: ExchangeTime::MIN,
            ..healthy()
        };
        assert_eq!(
            reason(gate().check(&buy(1), &input)),
            Some(RiskReason::OrderRateLimit)
        );
    }
}
