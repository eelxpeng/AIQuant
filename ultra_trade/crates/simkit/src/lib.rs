//! Scripted feeds and fixtures. Test scaffolding only.
//!
//! The line between this and `adapters/sim`: **an adapter is something you
//! would run a real backtest through; simkit is scaffolding.** A scripted feed
//! of six hand-written quotes is scaffolding. A simulated venue with a fill
//! model is not (`docs/ARCHITECTURE.md`).
//!
//! Nothing here reads a clock, opens a socket, or touches the filesystem
//! (Constitution VIII).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use engine::FeedAdapter;
use event::{Command, CommandEvent, Inbound, MarketEvent, MarketKind, PositionReport, TimerEvent};
use oms::{Order, VenueAdapter, VenueError};
use std::collections::VecDeque;
use strategy::TimerRequest;
use types::{ExchangeTime, InstrumentId, OrderId, Px, Qty, ReceiveTime, SCALE, Side, Timestamp};

/// A venue that accepts everything and reports nothing.
///
/// This is what a **replay** is bound to. Replaying a session means feeding the
/// recorded inputs back through a fresh engine — and the venue's reports are
/// among those recorded inputs, so a venue that generated its own would deliver
/// every fill twice.
///
/// It records what it was asked to do, so a replay can also check that the same
/// orders were sent, not merely that the same records were written.
#[derive(Debug, Clone, Default)]
pub struct ReplayVenue {
    submitted: Vec<OrderId>,
    cancelled: Vec<OrderId>,
}

impl ReplayVenue {
    /// A venue that has been asked for nothing.
    pub fn new() -> ReplayVenue {
        ReplayVenue::default()
    }

    /// Orders it was asked to send, in order.
    #[inline]
    pub fn submitted(&self) -> &[OrderId] {
        &self.submitted
    }

    /// Orders it was asked to cancel, in order.
    #[inline]
    pub fn cancelled(&self) -> &[OrderId] {
        &self.cancelled
    }
}

impl VenueAdapter for ReplayVenue {
    fn submit(&mut self, order: &Order) -> Result<(), VenueError> {
        self.submitted.push(order.id());
        Ok(())
    }

    fn cancel(&mut self, id: OrderId) -> Result<(), VenueError> {
        self.cancelled.push(id);
        Ok(())
    }

    fn drain(&mut self, _out: &mut Vec<Inbound>) {}
}

/// A feed that replays a fixed list of inputs, interleaving timers by time.
#[derive(Debug, Clone, Default)]
pub struct ScriptedFeed {
    events: VecDeque<Inbound>,
    /// Kept sorted by `(at, strategy, token)`. A total order, not just a
    /// time order: two timers due at the same instant must fire in the same
    /// sequence on every run (Constitution II).
    timers: Vec<TimerRequest>,
}

impl ScriptedFeed {
    /// A feed over these inputs, in this order.
    pub fn new(events: Vec<Inbound>) -> ScriptedFeed {
        ScriptedFeed {
            events: events.into(),
            timers: Vec::new(),
        }
    }

    /// How many scripted inputs are left, not counting timers.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.events.len()
    }

    /// How many timers are waiting to fire.
    #[inline]
    pub fn pending_timers(&self) -> usize {
        self.timers.len()
    }

    /// The earliest timer, unconditionally.
    fn take_first(&mut self) -> Option<TimerRequest> {
        if self.timers.is_empty() {
            None
        } else {
            Some(self.timers.remove(0))
        }
    }

    /// The earliest timer, if it is due at or before `limit`.
    fn take_due_by(&mut self, limit: ExchangeTime) -> Option<TimerRequest> {
        // A timer due at exactly the next event's timestamp fires first.
        // Either order is defensible; what matters is that it is the same
        // order every run, so it is stated here rather than emergent.
        if self.timers.first()?.at.to_nanos() > limit.to_nanos() {
            return None;
        }
        Some(self.timers.remove(0))
    }
}

impl FeedAdapter for ScriptedFeed {
    fn next_event(&mut self) -> Option<Inbound> {
        let due = match self.events.front() {
            // The script is spent, so nothing can come before a timer.
            None => self.take_first(),
            Some(next) => match next.exchange_time() {
                Some(limit) => self.take_due_by(limit),
                // An operator command carries no exchange time, so no timer
                // can be shown to be due before it. The command goes first.
                None => None,
            },
        };

        if let Some(due) = due {
            return Some(Inbound::Timer(TimerEvent {
                strategy: due.strategy,
                token: due.token,
                fires_at: due.at,
                // In a backtest there is no network between the timer and the
                // process, so the two clocks coincide. They are still
                // different kinds and are never subtracted from each other.
                receive_time: Timestamp::from_nanos(due.at.to_nanos()),
            }));
        }
        self.events.pop_front()
    }

    fn schedule_timer(&mut self, request: TimerRequest) {
        self.timers.push(request);
        self.timers
            .sort_by_key(|t| (t.at.to_nanos(), t.strategy.raw(), t.token.raw()));
    }
}

/// Builds a scripted feed in readable whole units.
///
/// Prices and quantities are given as whole units and scaled here, so a fixture
/// reads `quote(100, 1, 101, 1)` rather than carrying nine zeroes per number.
#[derive(Debug, Clone)]
pub struct Script {
    instrument: InstrumentId,
    events: Vec<Inbound>,
}

impl Script {
    /// A script for one instrument.
    pub fn new(instrument: InstrumentId) -> Script {
        Script {
            instrument,
            events: Vec::new(),
        }
    }

    /// Adds a top-of-book update.
    pub fn quote(mut self, at: i64, bid: i64, bid_qty: i64, ask: i64, ask_qty: i64) -> Script {
        self.events.push(Inbound::Market(MarketEvent {
            instrument: self.instrument,
            exchange_time: Timestamp::from_nanos(at),
            receive_time: Timestamp::from_nanos(at),
            kind: MarketKind::Quote {
                bid_px: px(bid),
                bid_qty: qty(bid_qty),
                ask_px: px(ask),
                ask_qty: qty(ask_qty),
            },
        }));
        self
    }

    /// Adds a trade print.
    pub fn trade(mut self, at: i64, price: i64, size: i64, aggressor: Side) -> Script {
        self.events.push(Inbound::Market(MarketEvent {
            instrument: self.instrument,
            exchange_time: Timestamp::from_nanos(at),
            receive_time: Timestamp::from_nanos(at),
            kind: MarketKind::Trade {
                px: px(price),
                qty: qty(size),
                aggressor,
            },
        }));
        self
    }

    /// Adds an operator command.
    pub fn command(mut self, at: i64, command: Command) -> Script {
        self.events.push(Inbound::Command(CommandEvent {
            command,
            receive_time: Timestamp::from_nanos(at),
        }));
        self
    }

    /// Adds a venue position report, for reconciliation.
    pub fn venue_position(mut self, at: i64, venue_qty: i64) -> Script {
        self.events.push(Inbound::VenuePosition(PositionReport {
            instrument: self.instrument,
            venue_qty: qty(venue_qty),
            venue_time: Timestamp::from_nanos(at),
            receive_time: Timestamp::from_nanos(at),
        }));
        self
    }

    /// Adds an already-built input, for cases the helpers do not cover.
    pub fn raw(mut self, event: Inbound) -> Script {
        self.events.push(event);
        self
    }

    /// The inputs, in order.
    pub fn events(&self) -> &[Inbound] {
        &self.events
    }

    /// Turns the script into a feed.
    pub fn build(self) -> ScriptedFeed {
        ScriptedFeed::new(self.events)
    }
}

/// A price given in whole units.
#[inline]
pub const fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

/// A quantity given in whole units.
#[inline]
pub const fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

/// An exchange timestamp, in nanoseconds.
#[inline]
pub const fn exchange_time(nanos: i64) -> ExchangeTime {
    Timestamp::from_nanos(nanos)
}

/// A receive timestamp, in nanoseconds.
#[inline]
pub const fn receive_time(nanos: i64) -> ReceiveTime {
    Timestamp::from_nanos(nanos)
}

#[cfg(test)]
mod tests {
    use super::*;
    use event::TimerToken;
    use types::StrategyId;

    const I: InstrumentId = InstrumentId::new(0);

    fn request(at: i64, token: u64) -> TimerRequest {
        TimerRequest {
            strategy: StrategyId::new(0),
            at: exchange_time(at),
            token: TimerToken::new(token),
        }
    }

    #[test]
    fn a_script_replays_its_events_in_order() {
        let mut feed = Script::new(I)
            .quote(10, 100, 1, 101, 1)
            .trade(20, 100, 1, Side::Sell)
            .build();
        assert_eq!(
            feed.next_event().unwrap().exchange_time(),
            Some(exchange_time(10))
        );
        assert_eq!(
            feed.next_event().unwrap().exchange_time(),
            Some(exchange_time(20))
        );
        assert_eq!(feed.next_event(), None);
    }

    #[test]
    fn a_timer_fires_before_the_event_that_comes_after_it() {
        let mut feed = Script::new(I).quote(100, 1, 1, 2, 1).build();
        feed.schedule_timer(request(50, 7));
        let first = feed.next_event().expect("timer");
        assert!(matches!(first, Inbound::Timer(t) if t.token == TimerToken::new(7)));
        assert_eq!(
            feed.next_event().unwrap().exchange_time(),
            Some(exchange_time(100))
        );
    }

    #[test]
    fn a_timer_due_later_than_the_next_event_waits_its_turn() {
        let mut feed = Script::new(I).quote(100, 1, 1, 2, 1).build();
        feed.schedule_timer(request(500, 1));
        assert_eq!(
            feed.next_event().unwrap().exchange_time(),
            Some(exchange_time(100))
        );
        // With the script exhausted, the timer still fires.
        assert!(matches!(feed.next_event(), Some(Inbound::Timer(_))));
        assert_eq!(feed.next_event(), None);
    }

    #[test]
    fn timers_due_at_the_same_instant_fire_in_a_stated_order() {
        let mut feed = ScriptedFeed::default();
        feed.schedule_timer(request(10, 9));
        feed.schedule_timer(request(10, 2));
        feed.schedule_timer(request(5, 5));
        let tokens: Vec<u64> = std::iter::from_fn(|| feed.next_event())
            .filter_map(|e| match e {
                Inbound::Timer(t) => Some(t.token.raw()),
                _ => None,
            })
            .collect();
        // Earliest first, then by strategy and token — a total order, so this
        // sequence is the same on every run.
        assert_eq!(tokens, vec![5, 2, 9]);
    }

    #[test]
    fn a_command_has_no_exchange_time_and_still_takes_its_turn() {
        let mut feed = Script::new(I)
            .command(1, Command::Halt)
            .quote(100, 1, 1, 2, 1)
            .build();
        feed.schedule_timer(request(50, 1));
        // The command cannot be compared against the timer, so it goes first.
        assert!(matches!(feed.next_event(), Some(Inbound::Command(_))));
        assert!(matches!(feed.next_event(), Some(Inbound::Timer(_))));
    }

    #[test]
    fn whole_unit_helpers_scale_to_the_fixed_point_representation() {
        assert_eq!(px(100).to_scaled(), 100 * SCALE);
        assert_eq!(qty(3).to_scaled(), 3 * SCALE);
    }
}
