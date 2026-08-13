//! D2.2: every order reaches the venue through the risk gate. Nothing outside
//! the engine can send one, because the engine owns its venue and only ever
//! lends it out immutably — and every method that talks to a venue needs `&mut`.

use engine::{Engine, EngineConfig};
use event::{Inbound, MemoryLog};
use oms::VenueAdapter;
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

    // Legal: the venue is readable, so this case is not passing because the
    // accessor is missing.
    let _ = engine.venue().submitted();

    let mut reports: Vec<Inbound> = Vec::new();
    engine.venue().drain(&mut reports);
}
