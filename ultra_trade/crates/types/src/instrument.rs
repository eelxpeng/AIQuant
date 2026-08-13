//! The tradable contract and the direction of a trade.

use crate::{InstrumentId, Px, Qty, RoundDir, ValueError};

/// Which way a trade goes.
///
/// The preferred spelling of a signed quantity is an unsigned [`Qty`] plus one
/// of these (`CONTEXT.md`), because a bare sign is ambiguous about whether it
/// means direction or a position that has already been netted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Side {
    /// Lifting the offer: position increases.
    Buy,
    /// Hitting the bid: position decreases.
    Sell,
}

impl Side {
    /// `+1` for a buy, `-1` for a sell.
    ///
    /// The one place direction becomes arithmetic. Position accounting
    /// multiplies a fill quantity by this rather than branching on the side, so
    /// there is a single definition of what a side means to the books.
    #[inline]
    pub const fn sign(self) -> i64 {
        match self {
            Side::Buy => 1,
            Side::Sell => -1,
        }
    }

    /// The other side.
    #[inline]
    pub const fn opposite(self) -> Side {
        match self {
            Side::Buy => Side::Sell,
            Side::Sell => Side::Buy,
        }
    }

    /// The side that reduces a position of the given signed quantity.
    ///
    /// Returns `None` for a flat position: there is nothing to reduce, so there
    /// is no answer rather than an arbitrary one. A flatten that treated flat as
    /// "sell" would open a short.
    #[inline]
    pub const fn reducing(position: Qty) -> Option<Side> {
        if position.to_scaled() > 0 {
            Some(Side::Sell)
        } else if position.to_scaled() < 0 {
            Some(Side::Buy)
        } else {
            None
        }
    }

    /// The direction a price must be rounded to stay passive on this side.
    ///
    /// A resting buy rounds down and a resting sell rounds up, so rounding
    /// never pushes a quote through the price the caller asked for. Stated here
    /// once rather than at each venue boundary, so no call site has to
    /// re-derive it.
    #[inline]
    pub const fn passive_rounding(self) -> RoundDir {
        match self {
            Side::Buy => RoundDir::Down,
            Side::Sell => RoundDir::Up,
        }
    }
}

/// The tradable contract at a specific venue.
///
/// Carries the tick size and lot size that every rounding decision at that
/// venue's boundary uses (Constitution VII). Constructed once at session start
/// and then read-only, so the hot path never validates one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Instrument {
    id: InstrumentId,
    tick: Px,
    lot: Qty,
    min_qty: Qty,
}

impl Instrument {
    /// Builds an instrument, checking the conventions that later rounding
    /// depends on.
    ///
    /// Fails closed rather than defaulting: a zero tick would make
    /// [`round_price`] meaningless, and a negative one would round the wrong
    /// way. This is the only validation point, which is why the accessors
    /// below can be infallible.
    ///
    /// [`round_price`]: Instrument::round_price
    pub const fn new(
        id: InstrumentId,
        tick: Px,
        lot: Qty,
        min_qty: Qty,
    ) -> Result<Instrument, ValueError> {
        if tick.to_scaled() <= 0 || lot.to_scaled() <= 0 {
            return Err(ValueError::InvalidStep);
        }
        if min_qty.to_scaled() < 0 {
            return Err(ValueError::OutOfRange);
        }
        Ok(Instrument {
            id,
            tick,
            lot,
            min_qty,
        })
    }

    /// Which instrument this is.
    #[inline]
    pub const fn id(self) -> InstrumentId {
        self.id
    }

    /// The venue's price increment.
    #[inline]
    pub const fn tick(self) -> Px {
        self.tick
    }

    /// The venue's quantity increment.
    #[inline]
    pub const fn lot(self) -> Qty {
        self.lot
    }

    /// The smallest quantity the venue accepts.
    #[inline]
    pub const fn min_qty(self) -> Qty {
        self.min_qty
    }

    /// Rounds a price to this venue's tick in the named direction.
    ///
    /// Every price crossing the venue boundary goes through here, and the
    /// caller states the direction — there is no default (Constitution VII).
    #[inline]
    pub fn round_price(self, px: Px, dir: RoundDir) -> Result<Px, ValueError> {
        px.round_to_tick(self.tick, dir)
    }

    /// Rounds a quantity to this venue's lot in the named direction.
    #[inline]
    pub fn round_qty(self, qty: Qty, dir: RoundDir) -> Result<Qty, ValueError> {
        qty.round_to_lot(self.lot, dir)
    }

    /// Whether a quantity is large enough for the venue to accept.
    ///
    /// Compared exactly, never with a tolerance (Constitution VII).
    #[inline]
    pub const fn meets_min_qty(self, qty: Qty) -> bool {
        let magnitude = if qty.to_scaled() < 0 {
            // `i64::MIN` has no positive counterpart, so widen before negating.
            -(qty.to_scaled() as i128)
        } else {
            qty.to_scaled() as i128
        };
        magnitude >= self.min_qty.to_scaled() as i128
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn px(units: i64) -> Px {
        Px::from_scaled(units)
    }

    const fn qty(units: i64) -> Qty {
        Qty::from_scaled(units)
    }

    fn instrument() -> Instrument {
        // Tick 0.01, lot 0.001, minimum 0.005.
        Instrument::new(
            InstrumentId::new(0),
            px(10_000_000),
            qty(1_000_000),
            qty(5_000_000),
        )
        .expect("valid conventions")
    }

    #[test]
    fn a_buy_is_positive_and_a_sell_is_negative() {
        assert_eq!(Side::Buy.sign(), 1);
        assert_eq!(Side::Sell.sign(), -1);
        assert_eq!(Side::Buy.opposite(), Side::Sell);
    }

    #[test]
    fn reducing_a_long_sells_and_reducing_a_short_buys() {
        assert_eq!(Side::reducing(qty(5)), Some(Side::Sell));
        assert_eq!(Side::reducing(qty(-5)), Some(Side::Buy));
    }

    #[test]
    fn a_flat_position_has_no_reducing_side() {
        // The degenerate case: answering "sell" here would open a short.
        assert_eq!(Side::reducing(Qty::ZERO), None);
    }

    #[test]
    fn passive_rounding_never_crosses_the_requested_price() {
        assert_eq!(Side::Buy.passive_rounding(), RoundDir::Down);
        assert_eq!(Side::Sell.passive_rounding(), RoundDir::Up);
    }

    #[test]
    fn a_zero_or_negative_tick_is_rejected_at_construction() {
        let id = InstrumentId::new(0);
        assert_eq!(
            Instrument::new(id, Px::ZERO, qty(1), Qty::ZERO),
            Err(ValueError::InvalidStep)
        );
        assert_eq!(
            Instrument::new(id, px(-1), qty(1), Qty::ZERO),
            Err(ValueError::InvalidStep)
        );
        assert_eq!(
            Instrument::new(id, px(1), Qty::ZERO, Qty::ZERO),
            Err(ValueError::InvalidStep)
        );
    }

    #[test]
    fn a_negative_minimum_quantity_is_rejected() {
        assert_eq!(
            Instrument::new(InstrumentId::new(0), px(1), qty(1), qty(-1)),
            Err(ValueError::OutOfRange)
        );
    }

    #[test]
    fn rounding_goes_to_the_venue_conventions_in_the_named_direction() {
        let i = instrument();
        // 10.004 -> 10.00 down, 10.01 up.
        assert_eq!(
            i.round_price(px(10_004_000_000), RoundDir::Down),
            Ok(px(10_000_000_000))
        );
        assert_eq!(
            i.round_price(px(10_004_000_000), RoundDir::Up),
            Ok(px(10_010_000_000))
        );
        assert_eq!(
            i.round_qty(qty(1_500_000), RoundDir::Down),
            Ok(qty(1_000_000))
        );
    }

    #[test]
    fn minimum_quantity_is_compared_on_magnitude() {
        let i = instrument();
        assert!(i.meets_min_qty(qty(5_000_000)));
        assert!(i.meets_min_qty(qty(-5_000_000)));
        assert!(!i.meets_min_qty(qty(4_999_999)));
    }

    #[test]
    fn the_minimum_quantity_check_survives_the_widest_negative_quantity() {
        // `Qty::MIN.to_scaled()` is `i64::MIN`, which cannot be negated in i64.
        assert!(instrument().meets_min_qty(Qty::MIN));
    }
}
