//! D3.1: the only clock a strategy sees is the current event's exchange time.
//! Not a wall clock, not a monotonic one — those are different kinds and it is
//! handed neither.

use event::Intent;
use strategy::{Context, TimerRequest};
use types::{ExchangeTime, MonotonicTime, StrategyId, Timestamp};

fn main() {
    let mut intents: Vec<Intent> = Vec::new();
    let mut timers: Vec<TimerRequest> = Vec::new();
    let ctx = Context::new(
        StrategyId::new(0),
        Timestamp::from_nanos(0),
        &mut intents,
        &mut timers,
    );

    // Legal: current-event time is an exchange timestamp.
    let _: ExchangeTime = ctx.now();

    let _: MonotonicTime = ctx.now();
}
