//! D3.1: the context's collecting buffers are its own. A strategy cannot reach
//! past the interface into what the engine is accumulating.

use event::Intent;
use strategy::{Context, TimerRequest};
use types::{StrategyId, Timestamp};

fn main() {
    let mut intents: Vec<Intent> = Vec::new();
    let mut timers: Vec<TimerRequest> = Vec::new();
    let ctx = Context::new(
        StrategyId::new(0),
        Timestamp::from_nanos(0),
        &mut intents,
        &mut timers,
    );

    // Legal: how many intents this dispatch raised is public.
    let _ = ctx.intent_count();

    let _ = ctx.intents;
}
