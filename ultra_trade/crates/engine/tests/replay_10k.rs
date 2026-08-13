//! D0.2: a 10,000-event fixture replays identically.
//!
//! The roadmap's Phase 0 gate. Everything above Phase 0 depends on replay being
//! proven, and proving it over six events proves very little — determinism bugs
//! live at scale, in the paths that only open once a session has run long enough
//! to fill a buffer, cross a limit, or take a state transition it was not
//! expecting.
//!
//! # How the fixture is built
//!
//! From a linear congruential generator with a **fixed constant seed**. That is
//! not ambient randomness: the same constant produces the same 10,000 events on
//! every machine and every run, which is exactly what a replay fixture needs.
//! No `rand`, no clock, no file (Constitution II, VIII).
//!
//! The stream is deliberately hostile. It carries three instruments, quotes and
//! trades interleaved, events that arrive out of order, operator commands that
//! halt and resume and flatten mid-session, and a reconciliation divergence near
//! the end that leaves the last stretch halted. Every one of those is a path
//! whose decisions must reproduce.

use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::{
    Command, CommandEvent, Cursor, EngineState, Inbound, MarketEvent, MarketKind, MemoryLog,
    Outbound, PositionReport, Record, Seq,
};
use marketdata::{Aggregator, BarSpec, BarSubscription};
use oms::VenueAdapter;
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use simkit::ReplayVenue;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use strategy::MovingAverageCrossover;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

/// How many inputs the feed produces. The deliverable's number.
const EVENTS: usize = 10_000;
const INSTRUMENTS: usize = 3;

/// Fixed, so the fixture is the same everywhere. Changing it changes the
/// fixture, which is a deliberate act and not a tuning knob.
const SEED: u64 = 0x5eed_1234_abcd_0001;

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// A linear congruential generator, so the fixture is reproducible without a
/// dependency.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        // The high bits of an LCG are the well-behaved ones.
        self.0 >> 16
    }

    fn below(&mut self, bound: u64) -> u64 {
        self.next() % bound
    }
}

/// Builds the fixture: `EVENTS` inputs across `INSTRUMENTS` instruments.
fn build_fixture() -> Vec<Inbound> {
    let mut rng = Lcg(SEED);
    let mut events = Vec::with_capacity(EVENTS);
    // A bounded random walk per instrument, in whole units.
    let mut mid = [100i64; INSTRUMENTS];
    let mut clock: i64 = 1_000_000;

    for step in 0..EVENTS {
        clock += 1_000 + rng.below(1_000) as i64;
        let which = rng.below(INSTRUMENTS as u64) as usize;
        let instrument = InstrumentId::new(which as u32);

        // Walk the price, keeping it well inside the representable range.
        mid[which] += match rng.below(3) {
            0 => -1,
            1 => 0,
            _ => 1,
        };
        mid[which] = mid[which].clamp(50, 200);
        let m = mid[which];

        // A reconciliation divergence near the end: the last stretch of the
        // session runs halted, and those refusals have to replay too.
        if step == EVENTS * 95 / 100 {
            events.push(Inbound::VenuePosition(PositionReport {
                instrument,
                venue_qty: qty(9_999),
                venue_time: Timestamp::from_nanos(clock),
                receive_time: Timestamp::from_nanos(clock),
            }));
            continue;
        }

        match rng.below(100) {
            // Quotes dominate, as they do in life.
            0..=64 => events.push(Inbound::Market(MarketEvent {
                instrument,
                exchange_time: Timestamp::from_nanos(clock),
                receive_time: Timestamp::from_nanos(clock),
                kind: MarketKind::Quote {
                    bid_px: px(m - 1),
                    bid_qty: qty(50 + rng.below(200) as i64),
                    ask_px: px(m + 1),
                    ask_qty: qty(50 + rng.below(200) as i64),
                },
            })),
            65..=89 => events.push(Inbound::Market(MarketEvent {
                instrument,
                exchange_time: Timestamp::from_nanos(clock),
                receive_time: Timestamp::from_nanos(clock),
                kind: MarketKind::Trade {
                    px: px(m),
                    qty: qty(1 + rng.below(5) as i64),
                    aggressor: if rng.below(2) == 0 {
                        Side::Buy
                    } else {
                        Side::Sell
                    },
                },
            })),
            // Backdated: the book must refuse these rather than move backwards.
            90..=94 => events.push(Inbound::Market(MarketEvent {
                instrument,
                exchange_time: Timestamp::from_nanos(clock - 500_000),
                receive_time: Timestamp::from_nanos(clock),
                kind: MarketKind::Quote {
                    bid_px: px(1),
                    bid_qty: qty(1),
                    ask_px: px(2),
                    ask_qty: qty(1),
                },
            })),
            // Operator commands. Kill is excluded on purpose: it is terminal,
            // so one anywhere in the stream would make the rest of the fixture
            // a test of an engine that refuses everything. It has its own tests.
            _ => {
                let command = match rng.below(3) {
                    0 => Command::Halt,
                    1 => Command::Resume,
                    _ => Command::Flatten,
                };
                events.push(Inbound::Command(CommandEvent {
                    command,
                    receive_time: Timestamp::from_nanos(clock),
                }));
            }
        }
    }
    events
}

/// A feed over a fixed list. No timers: this fixture does not use them, and a
/// feed that silently dropped them would be worse than one that has none.
struct FixtureFeed {
    events: Vec<Inbound>,
    next: usize,
}

impl FeedAdapter for FixtureFeed {
    fn next_event(&mut self) -> Option<Inbound> {
        let event = self.events.get(self.next).copied()?;
        self.next += 1;
        Some(event)
    }
}

fn config() -> EngineConfig {
    let instruments: Vec<Instrument> = (0..INSTRUMENTS)
        .map(|i| {
            // Tick 0.01, lot 1, minimum 1.
            Instrument::new(
                InstrumentId::new(i as u32),
                Px::from_scaled(10_000_000),
                qty(1),
                qty(1),
            )
            .expect("conventions")
        })
        .collect();

    let mut limits = LimitBook::with_instruments(INSTRUMENTS);
    for i in 0..INSTRUMENTS {
        limits
            .set(
                InstrumentId::new(i as u32),
                Limits {
                    max_position: qty(200),
                    max_exposure: money(1_000_000),
                    max_order_notional: money(500_000),
                    max_orders_in_window: 25,
                    rate_window: ExchangeSpan::from_nanos(50_000),
                    max_quote_age: ExchangeSpan::from_nanos(10_000_000),
                },
            )
            .expect("limits");
    }

    let mut config = EngineConfig::new(
        instruments,
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    config.capacity.log = 1 << 17;
    config.capacity.orders = 1 << 14;
    config
}

/// Wires three crossovers, one per instrument, each on its own bar stream.
fn wire<V: VenueAdapter>(venue: V) -> Engine<V, MemoryLog> {
    let mut engine = Engine::new(config(), venue, MemoryLog::with_capacity(1 << 17));
    for i in 0..INSTRUMENTS {
        let instrument = InstrumentId::new(i as u32);
        let subscription = engine.add_aggregator(
            Aggregator::new(instrument, BarSpec::Tick { threshold: 3 }).expect("spec"),
        );
        assert_eq!(subscription, BarSubscription::from_index(i as u16));
        engine
            .add_strategy(Box::new(MovingAverageCrossover::new(
                StrategyId::new(i as u16),
                instrument,
                subscription,
                8,
                qty(20),
            )))
            .expect("strategy");
    }
    engine
}

/// A digest of the whole record sequence.
///
/// Not a stable checksum — `DefaultHasher` is not guaranteed across Rust
/// versions — so it is only ever compared against another digest computed in
/// the same process. It exists to make "the two logs are the same" a single
/// number a PR body can quote, alongside the field-by-field assertion that is
/// the actual check.
fn digest(records: &[Record]) -> u64 {
    let mut hasher = DefaultHasher::new();
    records.len().hash(&mut hasher);
    for record in records {
        record.hash(&mut hasher);
    }
    hasher.finish()
}

fn run_live(events: Vec<Inbound>) -> Engine<SimVenue, MemoryLog> {
    let venue = SimVenue::new(
        INSTRUMENTS,
        FillModel::TouchDisplayed,
        Fees {
            maker: Px::ZERO,
            taker: Px::from_scaled(SCALE / 100),
        },
        ExchangeSpan::from_nanos(0),
    );
    let mut engine = wire(venue);
    let mut feed = FixtureFeed { events, next: 0 };
    run(&mut feed, &mut engine).expect("the fixture session must not error");
    engine
}

#[test]
fn a_ten_thousand_event_session_replays_identically() {
    let fixture = build_fixture();
    assert_eq!(fixture.len(), EVENTS);

    let live = run_live(fixture.clone());
    let live_records = live.log().records().to_vec();

    // The session has to have actually done something, or this proves nothing.
    let orders = live
        .log()
        .outbound()
        .filter(|o| matches!(o, Outbound::OrderSubmitted { .. }))
        .count();
    let refusals = live
        .log()
        .outbound()
        .filter(|o| matches!(o, Outbound::IntentRejected { .. }))
        .count();
    assert!(
        orders > 100,
        "only {orders} orders — the fixture is too tame"
    );
    assert!(refusals > 0, "no refusals — the limits never bound");
    assert!(
        live.books().total_out_of_order() > 100,
        "the backdated events were not exercised"
    );
    assert_eq!(
        live.state(),
        EngineState::Halted,
        "the divergence should halt"
    );
    // Printed rather than asserted exactly: the shape is what makes this
    // fixture worth running, and a reader of a failing run wants it. Visible
    // with `--nocapture`.
    println!(
        "fixture: {EVENTS} inputs -> {} records, {orders} orders, {refusals} refusals, \
         {} refused as out of order, ending {:?}",
        live_records.len(),
        live.books().total_out_of_order(),
        live.state(),
    );

    // Replay binds a venue that says nothing: the venue's reports are already
    // among the recorded inputs.
    let mut replayed = wire(ReplayVenue::new());
    for record in &live_records {
        if let Some(inbound) = record.event.as_inbound() {
            replayed.on_inbound(*inbound).expect("replay");
        }
    }

    let replay_records = replayed.log().records();
    assert_eq!(
        replay_records.len(),
        live_records.len(),
        "replay produced a different number of records"
    );
    // Field by field, so a mismatch names the record rather than a hash.
    for (a, b) in live_records.iter().zip(replay_records) {
        assert_eq!(a, b, "records diverge at {}", a.seq);
    }
    assert_eq!(digest(&live_records), digest(replay_records));

    // The derived state has to match too, not just the log.
    assert_eq!(replayed.state(), live.state());
    assert_eq!(
        replayed.books().total_out_of_order(),
        live.books().total_out_of_order()
    );
    for i in 0..INSTRUMENTS {
        let id = InstrumentId::new(i as u32);
        let a = live.positions().get(id).expect("position");
        let b = replayed.positions().get(id).expect("position");
        assert_eq!(a.qty(), b.qty(), "position diverges on {id}");
        assert_eq!(a.realized(), b.realized(), "realized diverges on {id}");
        assert_eq!(a.cash(), b.cash(), "cash diverges on {id}");
    }
}

#[test]
fn two_live_runs_over_the_fixture_agree() {
    // Determinism of the forward path, independent of replay: if this fails but
    // the replay test passes, the fixture generator is the thing that moved.
    let first = run_live(build_fixture());
    let second = run_live(build_fixture());
    assert_eq!(
        digest(first.log().records()),
        digest(second.log().records())
    );
}

#[test]
fn the_cursor_walks_seeks_and_exhausts_a_large_log() {
    let live = run_live(build_fixture());
    let log = live.log();
    let total = log.records().len();
    assert!(total > EVENTS, "the log holds decisions as well as inputs");

    // A full walk visits every record once, in sequence order.
    let mut cursor: Cursor<'_> = log.cursor();
    let mut seen = 0usize;
    let mut expected = Seq::FIRST;
    while let Some(record) = cursor.next_record() {
        assert_eq!(record.seq, expected);
        expected = Seq::new(expected.raw() + 1);
        seen += 1;
    }
    assert_eq!(seen, total);
    assert!(cursor.is_exhausted());

    // Seeking into the middle resumes from exactly there.
    let middle = Seq::new((total / 2) as u64);
    let mut cursor = log.cursor();
    assert!(cursor.seek(middle));
    assert_eq!(cursor.position(), middle);
    assert_eq!(cursor.next_record().expect("record").seq, middle);

    // Seeking to the end is legal and leaves nothing to read; past it is not.
    let mut cursor = log.cursor();
    assert!(cursor.seek(Seq::new(total as u64)));
    assert!(cursor.is_exhausted());
    assert!(!cursor.seek(Seq::new(total as u64 + 1)));
}

#[test]
fn a_ten_thousand_event_session_stays_within_its_reservations() {
    // Sized once at startup and never grown. If this fails, the session
    // allocated on the hot path and the capacity numbers need revisiting —
    // which is a finding, not a reason to widen the assertion.
    let live = run_live(build_fixture());
    assert!(
        !live.would_allocate(),
        "a buffer filled: orders={} log={}",
        live.orders().len(),
        live.log().records().len()
    );
    assert!(!live.log().would_grow());
}
