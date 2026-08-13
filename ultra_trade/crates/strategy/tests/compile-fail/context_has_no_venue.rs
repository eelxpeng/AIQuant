//! D3.1: a strategy reaches nothing. There is no venue on the other side of it.

use event::Intent;
use strategy::{Context, TimerRequest};
use types::{StrategyId, Timestamp};

fn main() {
    let mut intents: Vec<Intent> = Vec::new();
    let mut timers: Vec<TimerRequest> = Vec::new();
    let mut ctx = Context::new(
        StrategyId::new(0),
        Timestamp::from_nanos(0),
        &mut intents,
        &mut timers,
    );

    // Legal, and here so this case cannot pass merely because the context does
    // not exist: a strategy may read the current event's time.
    let _ = ctx.now();

    let _ = ctx.venue();
}
