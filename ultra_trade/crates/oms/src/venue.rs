//! The venue seam.
//!
//! This trait is one of the two edges where backtest, paper, and live differ
//! (`docs/ARCHITECTURE.md` seam 2). Nothing below it knows which is bound, and
//! nothing above it may ask (Constitution III).

use crate::Order;
use event::{Inbound, RejectReason};
use types::OrderId;

/// Why a venue would not take a request.
///
/// Distinct from [`RejectReason`], which is what a venue says *about an order*
/// it accepted the request for. This is the request never landing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VenueError {
    /// There is no working connection.
    Disconnected,
    /// The venue's outbound queue is full and the request was not sent.
    ///
    /// Named rather than emergent: what happens when a consumer falls behind
    /// is a design decision (Constitution VI). The engine's answer is to
    /// refuse the order, not to queue it silently.
    Backpressure,
    /// The request was malformed for this venue and never went on the wire.
    Malformed(RejectReason),
}

/// Somewhere orders can be sent.
///
/// Requests go out through [`submit`] and [`cancel`]; everything the venue says
/// back comes through [`drain`] as ordinary inbound events, which is what puts
/// venue reports in the log and therefore into replay (Constitution II).
///
/// Deliberately not async. A single-threaded loop with an explicit drain is
/// reproducible; a future's completion order is not.
///
/// [`submit`]: VenueAdapter::submit
/// [`cancel`]: VenueAdapter::cancel
/// [`drain`]: VenueAdapter::drain
pub trait VenueAdapter {
    /// Sends an order.
    ///
    /// Returning `Ok` means the request left this process, not that the venue
    /// accepted it. Acceptance arrives later through [`drain`].
    ///
    /// [`drain`]: VenueAdapter::drain
    fn submit(&mut self, order: &Order) -> Result<(), VenueError>;

    /// Asks the venue to cancel an order.
    fn cancel(&mut self, id: OrderId) -> Result<(), VenueError>;

    /// Moves everything the venue has said since the last call onto `out`.
    ///
    /// The caller owns and reuses the buffer, so a warmed session does not
    /// allocate here.
    fn drain(&mut self, out: &mut Vec<Inbound>);

    /// Shows the venue the same market data the engine just saw.
    ///
    /// A simulated venue needs a view of the book to fill against. A real one
    /// has its own connection and ignores this, which is why the default does
    /// nothing.
    ///
    /// The engine calls it unconditionally. That is what keeps the mode
    /// difference at the edge: there is no branch anywhere asking whether the
    /// bound venue happens to be simulated (Constitution III).
    fn observe_market(&mut self, _market: &event::MarketEvent) {}
}
