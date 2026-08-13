//! Pre-trade checks, limits, and the kill switch.
//!
//! [`RiskGate::check`] is the only path from an intent to an order. There is no
//! bypass, no "internal" path, and no test-only shortcut that also exists in
//! the shipped binary (Constitution V).
//!
//! Two properties are worth stating up front:
//!
//! - **It fails closed.** Unknown instrument, missing limit config, stale or
//!   absent market data, an unreconciled position, or a bound that cannot be
//!   evaluated all refuse the order. Nothing here assumes flat and nothing
//!   substitutes a last known value.
//! - **It reaches nothing.** Everything it needs arrives in [`GateInput`], so
//!   this crate depends only on the event alphabet and every check is testable
//!   without a book, a position, or a venue.
//!
//! The gate is also the only place that applies venue conventions: by the time
//! an order exists, its price is on tick and its quantity is on lot, each
//! rounded in a direction this crate names (Constitution VII).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod gate;
mod limits;

pub use gate::{Approved, Decision, GateInput, RiskGate};
pub use limits::{LimitBook, Limits, LimitsError};
