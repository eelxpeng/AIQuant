//! The clock seam.
//!
//! Time is injected (`docs/ARCHITECTURE.md` seam 4). Nothing below the edge
//! calls ambient time — not the engine, and certainly not a strategy, which is
//! handed the timestamp of the event it is processing and nothing else.
//!
//! This trait is what the edge implements. A live feed reads a real clock to
//! stamp when the process saw something; a backtest reads timestamps out of a
//! file. Both hand the same two kinds downstream, and nothing downstream can
//! tell which produced them (Constitution III).
//!
//! There is no implementation here on purpose. `types` is `no_std` and reaches
//! nothing, so the one that calls the operating system lives in the adapter
//! that needs it.

use crate::{MonotonicTime, ReceiveTime};

/// Where the edge gets the time.
///
/// Two kinds, because they answer different questions and are never
/// interchangeable (`CONTEXT.md`):
///
/// - [`receive_time`] is wall-clock, and it is what stamps *when this process
///   observed* an event. It can jump backwards when the machine's clock is
///   corrected, which is exactly why it may not be used for elapsed time.
/// - [`monotonic`] never goes backwards, and it is what elapsed time and
///   timeouts are measured with. It has no meaning as an absolute instant.
///
/// Neither is the exchange clock. That one belongs to the venue and arrives
/// with the data.
///
/// [`receive_time`]: Clock::receive_time
/// [`monotonic`]: Clock::monotonic
pub trait Clock {
    /// The local wall-clock instant, for stamping when an event was observed.
    fn receive_time(&self) -> ReceiveTime;

    /// The local monotonic instant, for elapsed time and timeouts.
    fn monotonic(&self) -> MonotonicTime;
}
