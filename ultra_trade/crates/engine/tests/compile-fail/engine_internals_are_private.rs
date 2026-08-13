//! D2.2: the gate, the venue, and the order store are the engine's own. There
//! is no field to reach around the single path through.

use engine::{Engine, EngineConfig};
use event::MemoryLog;
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

    // Legal: where the engine is, is public.
    let _ = engine.state();

    let _ = engine.gate;
}
