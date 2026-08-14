//! D3.4: a strategy cannot peek at market state. It sees the events it is
//! given, and nothing it was not given.

use event::Intent;
use strategy::{Context, TimerRequest};
use types::{StrategyId, Timestamp};

fn main() {
    let mut intents: Vec<Intent> = Vec::new();
    let mut timers: Vec<TimerRequest> = Vec::new();
    let mut cancels = Vec::new();
    let mut ctx = Context::new(
        StrategyId::new(0),
        Timestamp::from_nanos(0),
        &mut intents,
        &mut timers,
        &mut cancels,
    );

    // Legal: the context exists and answers what time the current event is.
    let _ = ctx.now();

    let _ = ctx.books();
}
