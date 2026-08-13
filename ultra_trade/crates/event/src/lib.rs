//! The event alphabet, the durable log, and the replay cursor.
//!
//! This crate is the replay boundary (`docs/ARCHITECTURE.md` seam 1).
//! Everything that influences an order is a record here, which is what makes
//! Constitution II testable: feed the [`Inbound`] records of a session back
//! through a fresh engine and the [`Outbound`] records must come out identical.
//!
//! It is also where the shared vocabulary lives. [`Intent`], [`RiskReason`],
//! and [`EngineState`] are produced by crates above this one but *recorded*
//! here, and a type two crates share has to sit below both of them
//! (Constitution I). Pushing them down to the log's own alphabet is the
//! prescribed fix, not a back-reference.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod codec;
mod inbound;
mod log;
mod outbound;

pub use inbound::{
    Command, CommandEvent, Inbound, MarketEvent, MarketKind, PositionReport, RejectReason,
    TimerEvent, TimerToken, VenueEvent, VenueKind,
};
pub use log::{Cursor, EventLog, FORMAT_VERSION, LogError, MemoryLog, Record, Seq};
pub use outbound::{EngineState, Intent, OrderKind, Outbound, RiskReason, StateReason};

/// One entry in the log: either something that happened to the system, or
/// something the system decided.
///
/// The split is what makes replay mechanical. Inputs are fed back in; decisions
/// are compared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Event {
    /// Something happened to the system.
    In(Inbound),
    /// The system decided something.
    Out(Outbound),
}

impl Event {
    /// The input, if this is one.
    #[inline]
    pub const fn as_inbound(&self) -> Option<&Inbound> {
        match self {
            Event::In(i) => Some(i),
            Event::Out(_) => None,
        }
    }

    /// The decision, if this is one.
    #[inline]
    pub const fn as_outbound(&self) -> Option<&Outbound> {
        match self {
            Event::Out(o) => Some(o),
            Event::In(_) => None,
        }
    }
}

impl From<Inbound> for Event {
    fn from(inbound: Inbound) -> Event {
        Event::In(inbound)
    }
}

impl From<Outbound> for Event {
    fn from(outbound: Outbound) -> Event {
        Event::Out(outbound)
    }
}
