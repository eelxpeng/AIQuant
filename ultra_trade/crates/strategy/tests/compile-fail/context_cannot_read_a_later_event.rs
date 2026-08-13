//! D3.4: a strategy cannot observe a time later than the event in hand. There
//! is no way to ask for the next one.

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

    // Legal: the event in hand.
    let _ = ctx.now();

    let _ = ctx.peek_next_event();
}
