//! Order lifecycle, position accounting, and the venue seam.
//!
//! Three things live here, and they are separate on purpose:
//!
//! - [`Orders`] tracks each instruction through its state machine, where the
//!   binding rule is that a terminal state is never left.
//! - [`Positions`] derives what is held, and what it earned, from fills alone —
//!   with no division anywhere, so realized PnL is exact.
//! - [`VenueAdapter`] is the edge. Binding a different one is what makes a run
//!   a backtest or a live session, and nothing below the trait can tell which
//!   (Constitution III).
//!
//! This crate knows nothing about strategies.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod order;
mod position;
mod venue;

pub use order::{OmsError, Order, OrderState, Orders};
pub use position::{Position, PositionError, Positions, ReconState};
pub use venue::{VenueAdapter, VenueError};
