//! Fixed-point money types and source-typed timestamps.
//!
//! This crate is the bottom of the dependency order: everything else in the
//! workspace depends on it and it depends on nothing (Constitution I).
//!
//! Two things here are compile-time guarantees rather than conventions:
//!
//! 1. A price is not a quantity and a quantity is not money. The only operation
//!    relating two of them is [`Px::notional`], which names its rounding
//!    direction. `px + qty` and `px * qty` do not build.
//! 2. An exchange clock is not a receive clock is not a monotonic clock.
//!    Subtracting one from another does not build, and the [`Span`] a
//!    subtraction produces carries the kind it came from.
//!
//! Alongside those it holds the identities and the [`Instrument`] every other
//! crate keys its state by. They live here because the dependency order points
//! one way: a type two crates share has to sit below both of them
//! (Constitution I).
//!
//! The representation is fixed by ADR #4 (`docs/adr/4-fixed-point-representation.md`):
//! [`Px`] and [`Qty`] are `i64` at a `1e-9` scale, [`Notional`] is `i128` at the
//! same scale. The API contracts left open by that record — the rounding
//! contract on the notional product, kind-tagged spans, and what "backing
//! integers are private" forbids — are fixed by ADR #6.
//!
//! Nothing here reads a clock, allocates, or performs I/O.

#![no_std]
#![forbid(unsafe_code)]
#![deny(missing_docs)]

#[cfg(test)]
extern crate std;

mod error;
mod ids;
mod instrument;
mod money;
mod rounding;
mod time;

pub use error::ValueError;
pub use ids::{InstrumentId, OrderId, StrategyId};
pub use instrument::{Instrument, Side};
pub use money::{Notional, Px, Qty};
pub use rounding::RoundDir;
pub use time::{
    ClockKind, Exchange, ExchangeSpan, ExchangeTime, Monotonic, MonotonicSpan, MonotonicTime,
    Receive, ReceiveSpan, ReceiveTime, Span, Timestamp,
};

/// Scale units per whole unit — every value is an exact integer multiple of
/// `1e-9`.
///
/// This is a single source of truth, not a switchability mechanism. ADR #4
/// considered and rejected a `type Backing = i64` alias for the same reason:
/// the cost of changing the scale lives in the durable log format and its
/// fixtures, and a constant has no reach there.
pub const SCALE: i64 = 1_000_000_000;

/// [`SCALE`] widened, for the `i128` arithmetic that the notional product
/// carries.
pub(crate) const SCALE_I128: i128 = SCALE as i128;
