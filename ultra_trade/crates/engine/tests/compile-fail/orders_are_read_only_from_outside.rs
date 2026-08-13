//! D2.2: the order store is readable but not writable from outside. An order
//! that the gate never saw cannot be pushed into the session's books.

use engine::{Engine, EngineConfig};
use event::{MemoryLog, VenueKind};
use risk::LimitBook;
use simkit::ReplayVenue;
use types::{OrderId, Timestamp};

fn main() {
    let engine = Engine::new(
        EngineConfig::new(
            Vec::new(),
            LimitBook::with_instruments(0),
            OrderId::new(0),
            Timestamp::from_nanos(0),
        ),
        ReplayVenue::new(),
        MemoryLog::new(),
    );

    // Legal: the store is readable.
    let _ = engine.orders().len();

    let _ = engine
        .orders()
        .apply(OrderId::new(0), VenueKind::Accepted, Timestamp::from_nanos(0));
}
