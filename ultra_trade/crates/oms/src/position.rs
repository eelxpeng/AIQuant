//! Position and PnL accounting, derived entirely from fills.
//!
//! # Why FIFO lots
//!
//! Realized PnL under average-cost accounting needs a division: closing part of
//! a position requires the *proportional* share of its cost basis. Division
//! needs a rounding rule, and a rounding rule applied to money on every fill is
//! a place errors accumulate silently.
//!
//! Matching each closing fill against the lots it closes avoids the division
//! entirely: the profit on a closed lot is `(exit − entry) × closed_qty`, which
//! is exact. Nothing in this module divides.
//!
//! # The three numbers
//!
//! - `cash` — signed money in and out, fees included. Exact.
//! - `realized` — closed-lot profit less fees. Exact.
//! - `total(mark) = cash + position × mark` — mark-to-market, and
//!   `unrealized = total − realized`.
//!
//! Keeping `cash` rather than deriving it means the identity above is a *check*
//! on the lot bookkeeping, not a restatement of it.

use std::collections::VecDeque;
use types::{InstrumentId, Notional, Px, Qty, RoundDir, Side, ValueError};

/// Rounding for money moving in or out.
///
/// Applied to the signed value of a fill, which is then *subtracted* from cash.
/// Rounding it up therefore always moves cash the conservative way: a buy costs
/// a scale unit more, a sale returns a scale unit less.
const CASH_ROUNDING: RoundDir = RoundDir::Up;

/// Rounding for closed-lot profit. Down, so PnL is never overstated.
const PNL_ROUNDING: RoundDir = RoundDir::Down;

/// What accounting refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PositionError {
    /// A fill quantity of zero or less.
    NonPositiveQuantity,
    /// A value left the representable range.
    Overflow,
    /// The instrument id is outside the configured range.
    UnknownInstrument,
}

impl From<ValueError> for PositionError {
    fn from(_: ValueError) -> PositionError {
        PositionError::Overflow
    }
}

/// One opening trade still on the books.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Lot {
    /// Magnitude, always positive.
    qty: Qty,
    /// What it was opened at.
    px: Px,
}

/// Whether the system's accounting has been checked against the venue's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReconState {
    /// The venue has not reported this instrument yet.
    ///
    /// Distinct from [`Agreed`] on purpose: "we agree" and "we have not asked"
    /// are different facts, and collapsing them would let an operator read a
    /// silent feed as a healthy one.
    ///
    /// [`Agreed`]: ReconState::Agreed
    NeverChecked,
    /// The last venue report matched.
    Agreed,
    /// The last venue report did not match. Trading halts and this never
    /// resolves itself (Constitution V).
    Diverged {
        /// What the venue said.
        venue_qty: Qty,
        /// What the system's own accounting says.
        own_qty: Qty,
    },
}

/// The system's accounting for one instrument.
#[derive(Debug, Clone)]
pub struct Position {
    instrument: InstrumentId,
    qty: Qty,
    cash: Notional,
    realized: Notional,
    fees: Notional,
    lots: VecDeque<Lot>,
    recon: ReconState,
}

impl Position {
    /// A flat position with room for `lot_capacity` open lots reserved.
    pub fn new(instrument: InstrumentId, lot_capacity: usize) -> Position {
        Position {
            instrument,
            qty: Qty::ZERO,
            cash: Notional::ZERO,
            realized: Notional::ZERO,
            fees: Notional::ZERO,
            lots: VecDeque::with_capacity(lot_capacity),
            recon: ReconState::NeverChecked,
        }
    }

    /// Which instrument this accounts for.
    #[inline]
    pub const fn instrument(&self) -> InstrumentId {
        self.instrument
    }

    /// Signed net quantity held.
    #[inline]
    pub const fn qty(&self) -> Qty {
        self.qty
    }

    /// Signed money in and out, fees included.
    #[inline]
    pub const fn cash(&self) -> Notional {
        self.cash
    }

    /// Profit from closed quantity, fees included.
    #[inline]
    pub const fn realized(&self) -> Notional {
        self.realized
    }

    /// Fees paid so far.
    #[inline]
    pub const fn fees(&self) -> Notional {
        self.fees
    }

    /// Whether this instrument's accounting agrees with the venue's.
    #[inline]
    pub const fn recon(&self) -> ReconState {
        self.recon
    }

    /// How many open lots are on the books.
    #[inline]
    pub fn open_lots(&self) -> usize {
        self.lots.len()
    }

    /// Whether the next opening fill would grow the lot queue, and so allocate.
    #[inline]
    pub fn lots_would_grow(&self) -> bool {
        self.lots.len() == self.lots.capacity()
    }

    /// Whether the position is flat.
    #[inline]
    pub const fn is_flat(&self) -> bool {
        self.qty.to_scaled() == 0
    }

    /// Mark-to-market value: cash plus the open position valued at `mark`.
    ///
    /// Rounded down, like realized profit, so the total is never overstated.
    pub fn total(&self, mark: Px) -> Result<Notional, PositionError> {
        let open_value = mark.notional(self.qty, PNL_ROUNDING);
        Ok(self.cash.checked_add(open_value)?)
    }

    /// Profit on the open position at `mark`.
    ///
    /// Defined as total minus realized, so the two always add back to the
    /// mark-to-market number rather than drifting apart.
    pub fn unrealized(&self, mark: Px) -> Result<Notional, PositionError> {
        Ok(self.total(mark)?.checked_sub(self.realized)?)
    }

    /// Folds a fill into the books.
    ///
    /// The order matters: any part of the fill that *closes* existing lots is
    /// matched first-in-first-out and realizes profit exactly; only what is
    /// left over opens a new lot. A fill that crosses through zero does both.
    pub fn apply_fill(
        &mut self,
        side: Side,
        qty: Qty,
        px: Px,
        fee: Notional,
    ) -> Result<(), PositionError> {
        if qty.to_scaled() <= 0 {
            return Err(PositionError::NonPositiveQuantity);
        }

        // Cash first: it depends only on the fill, not on the lot matching.
        let signed_qty = signed(side, qty)?;
        let value = px.notional(signed_qty, CASH_ROUNDING);
        self.cash = self.cash.checked_sub(value)?.checked_sub(fee)?;
        self.fees = self.fees.checked_add(fee)?;
        self.realized = self.realized.checked_sub(fee)?;

        let mut remaining = qty;

        // Close what this fill closes, oldest lot first.
        if self.reduces(side) {
            remaining = self.close_lots(remaining, px)?;
        }

        // Whatever is left opens a new lot on this side.
        if remaining.to_scaled() > 0 {
            self.lots.push_back(Lot { qty: remaining, px });
        }

        self.qty = self.qty.checked_add(signed_qty)?;
        Ok(())
    }

    /// Records what the venue says it holds.
    ///
    /// A divergence is recorded, never repaired: trusting either side would be
    /// a silent decision about money (Constitution V).
    pub fn observe_venue(&mut self, venue_qty: Qty) -> ReconState {
        self.recon = if venue_qty == self.qty {
            ReconState::Agreed
        } else {
            ReconState::Diverged {
                venue_qty,
                own_qty: self.qty,
            }
        };
        self.recon
    }

    /// Whether a fill on this side would reduce the position.
    fn reduces(&self, side: Side) -> bool {
        let position = self.qty.to_scaled();
        (position > 0 && side == Side::Sell) || (position < 0 && side == Side::Buy)
    }

    /// Matches `closing` against open lots, realizing profit. Returns whatever
    /// quantity was left over after the position reached flat.
    fn close_lots(&mut self, closing: Qty, exit: Px) -> Result<Qty, PositionError> {
        // +1 while long, -1 while short: the direction profit accrues in.
        let sign = if self.qty.to_scaled() > 0 { 1 } else { -1 };
        let mut remaining = closing;

        while remaining.to_scaled() > 0 {
            let Some(lot) = self.lots.front().copied() else {
                break;
            };
            let take = if lot.qty.to_scaled() <= remaining.to_scaled() {
                lot.qty
            } else {
                remaining
            };

            // (exit − entry) × take × sign. Exact: no division anywhere.
            let diff = exit.checked_sub(lot.px)?;
            let signed_take = Qty::from_scaled(
                take.to_scaled()
                    .checked_mul(sign)
                    .ok_or(PositionError::Overflow)?,
            );
            let profit = diff.notional(signed_take, PNL_ROUNDING);
            self.realized = self.realized.checked_add(profit)?;

            remaining = remaining.checked_sub(take)?;
            if take == lot.qty {
                self.lots.pop_front();
            } else {
                let front = self.lots.front_mut().expect("front exists");
                front.qty = front.qty.checked_sub(take)?;
            }
        }
        Ok(remaining)
    }
}

/// A fill quantity with its side's sign applied.
fn signed(side: Side, qty: Qty) -> Result<Qty, PositionError> {
    match side {
        Side::Buy => Ok(qty),
        Side::Sell => qty.checked_neg().map_err(PositionError::from),
    }
}

/// Per-instrument accounting for the whole session.
#[derive(Debug, Clone, Default)]
pub struct Positions {
    positions: Vec<Position>,
}

impl Positions {
    /// Accounting for `count` instruments, each with room for `lot_capacity`
    /// open lots reserved up front.
    pub fn with_instruments(count: usize, lot_capacity: usize) -> Positions {
        Positions {
            positions: (0..count)
                .map(|i| Position::new(InstrumentId::new(i as u32), lot_capacity))
                .collect(),
        }
    }

    /// How many instruments are accounted for.
    #[inline]
    pub fn len(&self) -> usize {
        self.positions.len()
    }

    /// Whether nothing is accounted for.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.positions.is_empty()
    }

    /// The accounting for one instrument.
    #[inline]
    pub fn get(&self, instrument: InstrumentId) -> Option<&Position> {
        self.positions.get(instrument.index())
    }

    /// The accounting for one instrument, mutably.
    #[inline]
    pub fn get_mut(&mut self, instrument: InstrumentId) -> Option<&mut Position> {
        self.positions.get_mut(instrument.index())
    }

    /// Every position, in instrument order.
    ///
    /// Ordered by construction rather than by hash, so two runs iterate
    /// identically (Constitution II).
    pub fn iter(&self) -> impl Iterator<Item = &Position> {
        self.positions.iter()
    }

    /// Whether any instrument's accounting disagrees with the venue's.
    pub fn any_diverged(&self) -> bool {
        self.positions
            .iter()
            .any(|p| matches!(p.recon(), ReconState::Diverged { .. }))
    }

    /// Folds a fill into the right instrument's books.
    pub fn apply_fill(
        &mut self,
        instrument: InstrumentId,
        side: Side,
        qty: Qty,
        px: Px,
        fee: Notional,
    ) -> Result<(), PositionError> {
        self.get_mut(instrument)
            .ok_or(PositionError::UnknownInstrument)?
            .apply_fill(side, qty, px, fee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALE: i64 = types::SCALE;

    /// A whole number of units, at the fixed-point scale.
    const fn px(whole: i64) -> Px {
        Px::from_scaled(whole * SCALE)
    }

    const fn qty(whole: i64) -> Qty {
        Qty::from_scaled(whole * SCALE)
    }

    fn money(whole: i64) -> Notional {
        Notional::from_scaled(whole as i128 * SCALE as i128)
    }

    fn flat() -> Position {
        Position::new(InstrumentId::new(0), 8)
    }

    #[test]
    fn a_fresh_position_is_flat_and_unchecked() {
        let p = flat();
        assert!(p.is_flat());
        assert_eq!(p.qty(), Qty::ZERO);
        assert_eq!(p.cash(), Notional::ZERO);
        assert_eq!(p.realized(), Notional::ZERO);
        assert_eq!(p.recon(), ReconState::NeverChecked);
    }

    #[test]
    fn buying_spends_cash_and_opens_a_lot() {
        let mut p = flat();
        p.apply_fill(Side::Buy, qty(10), px(100), Notional::ZERO)
            .expect("fill");
        assert_eq!(p.qty(), qty(10));
        assert_eq!(p.cash(), money(-1_000));
        assert_eq!(p.realized(), Notional::ZERO);
        assert_eq!(p.open_lots(), 1);
    }

    #[test]
    fn closing_part_of_a_position_realizes_exactly_that_part() {
        let mut p = flat();
        p.apply_fill(Side::Buy, qty(10), px(100), Notional::ZERO)
            .expect("open");
        p.apply_fill(Side::Sell, qty(4), px(110), Notional::ZERO)
            .expect("close");

        assert_eq!(p.qty(), qty(6));
        // Four units closed at ten profit each.
        assert_eq!(p.realized(), money(40));
        assert_eq!(p.cash(), money(-560));
        // The remaining six units are still open at their original price.
        assert_eq!(p.unrealized(px(110)).expect("mark"), money(60));
        assert_eq!(p.total(px(110)).expect("mark"), money(100));
    }

    #[test]
    fn lots_close_oldest_first() {
        let mut p = flat();
        p.apply_fill(Side::Buy, qty(5), px(100), Notional::ZERO)
            .expect("first lot");
        p.apply_fill(Side::Buy, qty(5), px(200), Notional::ZERO)
            .expect("second lot");
        p.apply_fill(Side::Sell, qty(5), px(150), Notional::ZERO)
            .expect("close");

        // FIFO closes the 100 lot: five units at fifty profit each.
        assert_eq!(p.realized(), money(250));
        assert_eq!(p.open_lots(), 1);
    }

    #[test]
    fn a_short_realizes_profit_when_the_price_falls() {
        let mut p = flat();
        p.apply_fill(Side::Sell, qty(10), px(100), Notional::ZERO)
            .expect("open short");
        assert_eq!(p.qty(), qty(-10));
        assert_eq!(p.cash(), money(1_000));

        p.apply_fill(Side::Buy, qty(10), px(90), Notional::ZERO)
            .expect("cover");
        assert_eq!(p.qty(), Qty::ZERO);
        assert_eq!(p.realized(), money(100));
        // Flat: cash and realized must agree.
        assert_eq!(p.cash(), p.realized());
    }

    #[test]
    fn a_fill_that_crosses_through_zero_closes_then_opens() {
        let mut p = flat();
        p.apply_fill(Side::Buy, qty(10), px(100), Notional::ZERO)
            .expect("open long");
        p.apply_fill(Side::Sell, qty(15), px(110), Notional::ZERO)
            .expect("flip short");

        // Ten closed at ten profit; five opened short at 110.
        assert_eq!(p.qty(), qty(-5));
        assert_eq!(p.realized(), money(100));
        assert_eq!(p.open_lots(), 1);
        // The new short lot is at the flip price, so it shows no profit there.
        assert_eq!(p.unrealized(px(110)).expect("mark"), Notional::ZERO);
    }

    #[test]
    fn fees_are_realized_immediately_and_do_not_look_like_price_movement() {
        let mut p = flat();
        p.apply_fill(Side::Buy, qty(10), px(100), money(1))
            .expect("fill");
        assert_eq!(p.fees(), money(1));
        assert_eq!(p.realized(), money(-1));
        assert_eq!(p.cash(), money(-1_001));
        // No price movement, so nothing is unrealized: the fee is already real.
        assert_eq!(p.unrealized(px(100)).expect("mark"), Notional::ZERO);
    }

    #[test]
    fn total_always_equals_realized_plus_unrealized() {
        let mut p = flat();
        p.apply_fill(Side::Buy, qty(7), px(50), money(2))
            .expect("open");
        p.apply_fill(Side::Sell, qty(3), px(65), money(1))
            .expect("partial close");
        let mark = px(70);
        assert_eq!(
            p.total(mark).expect("total"),
            p.realized()
                .checked_add(p.unrealized(mark).expect("unrealized"))
                .expect("sum")
        );
    }

    #[test]
    fn a_zero_or_negative_fill_is_refused() {
        let mut p = flat();
        assert_eq!(
            p.apply_fill(Side::Buy, Qty::ZERO, px(1), Notional::ZERO),
            Err(PositionError::NonPositiveQuantity)
        );
        assert_eq!(
            p.apply_fill(Side::Buy, qty(-1), px(1), Notional::ZERO),
            Err(PositionError::NonPositiveQuantity)
        );
    }

    #[test]
    fn a_matching_venue_report_agrees_and_a_mismatch_diverges() {
        let mut p = flat();
        p.apply_fill(Side::Buy, qty(10), px(100), Notional::ZERO)
            .expect("fill");
        assert_eq!(p.observe_venue(qty(10)), ReconState::Agreed);
        assert_eq!(
            p.observe_venue(qty(9)),
            ReconState::Diverged {
                venue_qty: qty(9),
                own_qty: qty(10),
            }
        );
        // A divergence is recorded, never repaired.
        assert_eq!(p.qty(), qty(10));
    }

    #[test]
    fn divergence_on_any_instrument_is_visible_across_the_book() {
        let mut positions = Positions::with_instruments(3, 4);
        assert!(!positions.any_diverged());
        positions
            .get_mut(InstrumentId::new(2))
            .expect("position")
            .observe_venue(qty(1));
        assert!(positions.any_diverged());
    }

    #[test]
    fn a_fill_in_an_unconfigured_instrument_is_refused() {
        let mut positions = Positions::with_instruments(1, 4);
        assert_eq!(
            positions.apply_fill(
                InstrumentId::new(9),
                Side::Buy,
                qty(1),
                px(1),
                Notional::ZERO
            ),
            Err(PositionError::UnknownInstrument)
        );
    }

    #[test]
    fn cash_rounding_never_flatters_the_books() {
        // A price and quantity whose product does not land on a scale unit:
        // 1.000000001 x 0.000000003 = 3.000000003e-9, which truncates.
        let odd_px = Px::from_scaled(1_000_000_001);
        let odd_qty = Qty::from_scaled(3);
        let mut buyer = flat();
        buyer
            .apply_fill(Side::Buy, odd_qty, odd_px, Notional::ZERO)
            .expect("buy");
        let mut seller = flat();
        seller
            .apply_fill(Side::Sell, odd_qty, odd_px, Notional::ZERO)
            .expect("sell");

        // The buyer pays the rounded-up value and the seller receives the
        // rounded-down one, so neither side is credited a unit that the
        // representation cannot account for.
        assert_eq!(buyer.cash(), Notional::from_scaled(-4));
        assert_eq!(seller.cash(), Notional::from_scaled(3));
    }

    #[test]
    fn the_lot_queue_reports_when_it_would_allocate() {
        let mut p = Position::new(InstrumentId::new(0), 2);
        assert!(!p.lots_would_grow());
        p.apply_fill(Side::Buy, qty(1), px(1), Notional::ZERO)
            .expect("fill");
        p.apply_fill(Side::Buy, qty(1), px(1), Notional::ZERO)
            .expect("fill");
        assert!(p.lots_would_grow());
    }
}
