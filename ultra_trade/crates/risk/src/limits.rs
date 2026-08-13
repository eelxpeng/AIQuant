//! Configured bounds, per instrument.

use types::{ExchangeSpan, InstrumentId, Notional, Qty};

/// The bounds the gate enforces for one instrument.
///
/// Every field names its unit and, where it needs one, its evaluation window
/// (`CONTEXT.md`, "Limit"). There is no default and no partial configuration:
/// an instrument either has a complete set of limits or cannot be traded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Limits {
    /// Largest `|position|` the system may hold.
    pub max_position: Qty,
    /// Largest `|position × mark|` the system may hold.
    pub max_exposure: Notional,
    /// Largest notional a single order may carry.
    pub max_order_notional: Notional,
    /// Most orders that may be sent within [`rate_window`].
    ///
    /// [`rate_window`]: Limits::rate_window
    pub max_orders_in_window: u32,
    /// The window the order-rate bound counts over, on the exchange clock.
    pub rate_window: ExchangeSpan,
    /// How old the top of book may be before an order is refused.
    pub max_quote_age: ExchangeSpan,
}

/// Why a set of limits could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LimitsError {
    /// A bound was zero or negative, which would refuse every order or none.
    NotPositive,
    /// The instrument id is outside the configured range.
    UnknownInstrument,
}

impl Limits {
    /// Checks that every bound is usable.
    ///
    /// A zero window or a zero maximum is refused here rather than at the
    /// gate, because "the config is unreachable" is a startup failure and not
    /// something to discover one order at a time (Constitution V).
    pub const fn validate(&self) -> Result<(), LimitsError> {
        if self.max_position.to_scaled() <= 0
            || self.max_exposure.to_scaled() <= 0
            || self.max_order_notional.to_scaled() <= 0
            || self.max_orders_in_window == 0
            || self.rate_window.to_nanos() <= 0
            || self.max_quote_age.to_nanos() <= 0
        {
            return Err(LimitsError::NotPositive);
        }
        Ok(())
    }
}

/// Limits for every configured instrument.
///
/// An instrument with no entry cannot be traded. That is the fail-closed
/// reading of "unreachable limit config" and it is why the storage is
/// `Option` rather than a default (Constitution V).
#[derive(Debug, Clone, Default)]
pub struct LimitBook {
    limits: Vec<Option<Limits>>,
}

impl LimitBook {
    /// Room for `count` instruments, none of them yet tradable.
    pub fn with_instruments(count: usize) -> LimitBook {
        LimitBook {
            limits: vec![None; count],
        }
    }

    /// Configures one instrument, validating the bounds.
    pub fn set(&mut self, instrument: InstrumentId, limits: Limits) -> Result<(), LimitsError> {
        limits.validate()?;
        let slot = self
            .limits
            .get_mut(instrument.index())
            .ok_or(LimitsError::UnknownInstrument)?;
        *slot = Some(limits);
        Ok(())
    }

    /// The bounds for one instrument, if it is tradable.
    #[inline]
    pub fn get(&self, instrument: InstrumentId) -> Option<&Limits> {
        self.limits.get(instrument.index())?.as_ref()
    }

    /// How many instruments this was built for.
    #[inline]
    pub fn len(&self) -> usize {
        self.limits.len()
    }

    /// Whether it was built for none.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.limits.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sane() -> Limits {
        Limits {
            max_position: Qty::from_scaled(1_000),
            max_exposure: Notional::from_scaled(1_000_000),
            max_order_notional: Notional::from_scaled(100_000),
            max_orders_in_window: 10,
            rate_window: ExchangeSpan::from_nanos(1_000_000_000),
            max_quote_age: ExchangeSpan::from_nanos(500_000_000),
        }
    }

    #[test]
    fn a_complete_set_of_bounds_validates() {
        assert_eq!(sane().validate(), Ok(()));
    }

    #[test]
    fn every_bound_must_be_positive() {
        let cases: [(&str, Limits); 6] = [
            (
                "position",
                Limits {
                    max_position: Qty::ZERO,
                    ..sane()
                },
            ),
            (
                "exposure",
                Limits {
                    max_exposure: Notional::ZERO,
                    ..sane()
                },
            ),
            (
                "order notional",
                Limits {
                    max_order_notional: Notional::ZERO,
                    ..sane()
                },
            ),
            (
                "rate count",
                Limits {
                    max_orders_in_window: 0,
                    ..sane()
                },
            ),
            (
                "rate window",
                Limits {
                    rate_window: ExchangeSpan::ZERO,
                    ..sane()
                },
            ),
            (
                "quote age",
                Limits {
                    max_quote_age: ExchangeSpan::ZERO,
                    ..sane()
                },
            ),
        ];
        for (name, limits) in cases {
            assert_eq!(
                limits.validate(),
                Err(LimitsError::NotPositive),
                "{name} should be refused at zero"
            );
        }
    }

    #[test]
    fn an_unconfigured_instrument_has_no_limits_and_so_cannot_be_traded() {
        let book = LimitBook::with_instruments(2);
        assert!(book.get(InstrumentId::new(0)).is_none());
        assert!(book.get(InstrumentId::new(1)).is_none());
    }

    #[test]
    fn configuring_an_instrument_outside_the_range_is_refused() {
        let mut book = LimitBook::with_instruments(1);
        assert_eq!(
            book.set(InstrumentId::new(5), sane()),
            Err(LimitsError::UnknownInstrument)
        );
    }

    #[test]
    fn invalid_bounds_are_refused_before_they_reach_storage() {
        let mut book = LimitBook::with_instruments(1);
        let bad = Limits {
            max_position: Qty::ZERO,
            ..sane()
        };
        assert_eq!(
            book.set(InstrumentId::new(0), bad),
            Err(LimitsError::NotPositive)
        );
        assert!(book.get(InstrumentId::new(0)).is_none());
    }
}
