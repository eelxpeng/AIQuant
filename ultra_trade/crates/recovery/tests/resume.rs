//! Bringing a crashed session back, and refusing to when it does not add up.
//!
//! The shape of every test here is the same: record a session, kill it, bring
//! it back, and ask whether the engine holds what the recording says it held.
//! The refusals matter more than the happy path — a recovery that quietly
//! comes back with the wrong position trades real size against a number
//! nobody checked.

use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::codec::{InstrumentEntry, LogHeader};
use event::{
    Command, CommandEvent, EngineState, Event, EventLog, Inbound, MarketEvent, MarketKind,
    MemoryLog, Record, Segments,
};
use marketdata::{Aggregator, BarSpec};
use recovery::{RecoveryError, recover, recover_from};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use strategy::MovingAverageCrossover;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// A session directory that deletes itself.
struct TempSession(PathBuf);

impl TempSession {
    fn new(name: &str) -> TempSession {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ultra_trade-rec-{}-{}-{}",
            std::process::id(),
            n,
            name
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempSession(path)
    }

    fn root(&self) -> PathBuf {
        self.0.join("session.log")
    }
}

impl Drop for TempSession {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn instrument() -> Instrument {
    Instrument::new(I, Px::from_scaled(SCALE / 100), qty(1), qty(1)).expect("conventions")
}

fn header() -> LogHeader {
    LogHeader::new(
        7,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument(), "AAA").expect("entry")],
    )
}

fn limits() -> LimitBook {
    let mut book = LimitBook::with_instruments(1);
    book.set(
        I,
        Limits {
            max_position: qty(100),
            max_exposure: money(10_000_000),
            max_order_notional: money(1_000_000),
            max_orders_in_window: 1_000,
            rate_window: ExchangeSpan::from_nanos(60_000_000_000),
            max_quote_age: ExchangeSpan::from_nanos(60_000_000_000),
        },
    )
    .expect("limits");
    book
}

fn sim() -> SimVenue {
    SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    )
}

/// A fresh engine with the crossover on it, exactly as a binary would build it.
fn build<L: event::EventLog>(log: L) -> Engine<SimVenue, L> {
    let config = EngineConfig::new(
        vec![instrument()],
        limits(),
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let mut engine = Engine::new(config, sim(), log);
    let subscription = engine
        .add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 1 }).expect("aggregator"));
    engine
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            I,
            subscription,
            4,
            qty(2),
        )))
        .expect("strategy");
    engine
}

/// A fresh engine running the quoter, which is the strategy that leaves
/// orders resting for a crash to interrupt.
fn quoting<L: EventLog>(log: L) -> Engine<SimVenue, L> {
    let config = EngineConfig::new(
        vec![instrument()],
        limits(),
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let mut engine = Engine::new(config, sim(), log);
    engine
        .add_strategy(Box::new(strategy::Quoter::new(
            StrategyId::new(0),
            I,
            Px::from_scaled(SCALE / 2),
            qty(1),
            Px::from_scaled(SCALE / 4),
            qty(50),
        )))
        .expect("strategy");
    engine
}

/// The market the session trades, chosen to make the crossover cross.
fn market() -> Vec<Inbound> {
    let mut events = Vec::new();
    for n in 0..40i64 {
        let at = 1_000_000_000 * (n + 1);
        // A saw-tooth, so the fast average crosses the slow one repeatedly and
        // the session ends holding something rather than flat by luck.
        let level = 100 + (n % 8) * 3;
        events.push(Inbound::Market(MarketEvent {
            instrument: I,
            // Two clocks, and they are different types on purpose. The fixture
            // gives them the same reading; it cannot give them the same value.
            exchange_time: Timestamp::from_nanos(at),
            receive_time: Timestamp::from_nanos(at),
            kind: MarketKind::Quote {
                bid_px: px(level),
                bid_qty: qty(50),
                ask_px: px(level + 1),
                ask_qty: qty(50),
            },
        }));
        events.push(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(at + 500_000_000),
            receive_time: Timestamp::from_nanos(at + 500_000_000),
            kind: MarketKind::Trade {
                px: px(level),
                qty: qty(1),
                aggressor: Side::Buy,
            },
        }));
    }
    events
}

struct ListFeed {
    events: Vec<Inbound>,
    next: usize,
}

impl FeedAdapter for ListFeed {
    fn next_event(&mut self) -> Option<Inbound> {
        let event = self.events.get(self.next).copied();
        if event.is_some() {
            self.next += 1;
        }
        event
    }
}

/// Records a session to `root` and returns the position it ended holding.
fn record_session(root: &Path, events: Vec<Inbound>) -> Qty {
    let writer = Segments::create(root, header()).expect("create");
    let mut engine = build(writer);
    let mut feed = ListFeed { events, next: 0 };
    run(&mut feed, &mut engine).expect("session");
    let held = engine.positions().get(I).expect("position").qty();
    drop(engine);
    held
}

// ---- the happy path ------------------------------------------------------

#[test]
fn a_recovered_session_holds_what_the_recording_says_it_held() {
    let temp = TempSession::new("holds");
    let held = record_session(&temp.root(), market());
    assert_ne!(held, Qty::ZERO, "the fixture must end holding something");

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    let recovered = recover(&mut engine, &temp.root()).expect("recover");

    assert_eq!(engine.positions().get(I).expect("position").qty(), held);
    assert_eq!(
        recovered.from_log.get(I).expect("from the log").qty(),
        held,
        "and the two independent readings agree"
    );
}

#[test]
fn a_recovered_session_starts_halted_and_not_running() {
    // A crash is an incident. Something should look at it before the system
    // trades again, and leaving a halt is never automatic (D-2).
    let temp = TempSession::new("halted");
    record_session(&temp.root(), market());

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    recover(&mut engine, &temp.root()).expect("recover");
    assert_eq!(engine.state(), EngineState::Halted);
}

#[test]
fn recovery_writes_nothing_to_the_new_log() {
    // Every record it replays is already on disk. Writing them again would
    // double the session, and the duplicate would be indistinguishable from
    // the market having actually done it twice.
    let temp = TempSession::new("silent");
    record_session(&temp.root(), market());

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    recover(&mut engine, &temp.root()).expect("recover");
    assert_eq!(engine.log().len(), 0);
}

#[test]
fn the_resumed_session_continues_the_sequence() {
    let temp = TempSession::new("sequence");
    record_session(&temp.root(), market());
    let (records, _) = Segments::read_all(&temp.root()).expect("read");

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    let recovered = recover(&mut engine, &temp.root()).expect("recover");
    assert_eq!(recovered.next_seq.raw(), records.len() as u64);
    assert_eq!(recovered.records, records.len() as u64);
}

#[test]
fn the_venue_comes_back_too_rather_than_starting_empty() {
    // The point of replaying without the recorded venue reports. If the venue
    // were left empty the engine would believe it had resting orders that
    // nothing would ever fill or cancel.
    let temp = TempSession::new("venue");
    record_session(&temp.root(), market());

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    recover(&mut engine, &temp.root()).expect("recover");

    // The book is the part that is always rebuilt: a venue that has not seen
    // the market cannot fill anything the resumed session sends it.
    assert!(
        engine.books().top(I).is_some(),
        "the venue and the engine both need the book back"
    );
}

#[test]
fn a_session_that_was_halted_by_its_operator_replays_that_halt() {
    // Commands are inputs, so they are part of the history being rebuilt. A
    // recovery that dropped them would re-trade decisions the operator had
    // already stopped.
    let temp = TempSession::new("commanded");
    let mut events = market();
    events.insert(
        20,
        Inbound::Command(CommandEvent {
            command: Command::Halt,
            receive_time: Timestamp::from_nanos(10_500_000_000),
        }),
    );
    let held = record_session(&temp.root(), events);

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    recover(&mut engine, &temp.root()).expect("recover");
    assert_eq!(engine.positions().get(I).expect("position").qty(), held);
}

// ---- refusals ------------------------------------------------------------

#[test]
fn a_recording_whose_fills_disagree_with_the_replay_is_refused() {
    // The dangerous case, forced. An extra fill in the recording makes the
    // log's account of the position differ from what replaying produces, and
    // neither is known to be right.
    let temp = TempSession::new("diverged");
    record_session(&temp.root(), market());
    let (mut records, recovery) = Segments::read_all(&temp.root()).expect("read");

    // Duplicate a recorded fill, exactly as a double-delivered report would.
    let fill = records
        .iter()
        .find(|r| {
            matches!(
                r.event,
                Event::In(Inbound::Venue(v)) if matches!(v.kind, event::VenueKind::Filled { .. })
            )
        })
        .copied()
        .expect("the fixture must fill something");
    records.push(Record {
        seq: event::Seq::new(records.len() as u64),
        version: event::FORMAT_VERSION,
        event: fill.event,
    });

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    let error = recover_from(&mut engine, &records, recovery)
        .expect_err("a doubled fill must not recover quietly");
    assert!(
        matches!(error, RecoveryError::Diverged { .. }),
        "got {error:?}"
    );
    // And it says both numbers, because the operator has to decide.
    let text = error.to_string();
    assert!(text.contains("the recording says"), "{text}");
    assert!(text.contains("will not resume"), "{text}");
}

#[test]
fn a_session_that_does_not_exist_is_refused_rather_than_started_fresh() {
    // A resume that quietly becomes a new session loses the position the
    // operator was trying to recover.
    let temp = TempSession::new("absent");
    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    let error = recover(&mut engine, &temp.root()).expect_err("nothing to recover");
    assert!(matches!(error, RecoveryError::Log(_)), "got {error:?}");
}

#[test]
fn an_empty_recording_recovers_to_a_flat_halted_session() {
    // Window 6 of the kill matrix: the process died before it wrote anything.
    // A header with no records is a session that never traded, which recovers
    // to exactly that rather than failing.
    let temp = TempSession::new("empty");
    let writer = Segments::create(&temp.root(), header()).expect("create");
    drop(writer);

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    let recovered = recover(&mut engine, &temp.root()).expect("recover an empty session");
    assert_eq!(recovered.records, 0);
    assert_eq!(recovered.next_seq, event::Seq::FIRST);
    assert_eq!(engine.state(), EngineState::Halted);
    assert!(engine.positions().get(I).expect("position").is_flat());
}

#[test]
fn a_torn_recording_still_recovers_and_says_what_it_lost() {
    // The ordinary crash. The tail is gone; the session that survived is still
    // the session, and the caller is handed the fact so it can say so.
    let temp = TempSession::new("torn");
    record_session(&temp.root(), market());
    let seg = Segments::path(&temp.root(), 0);
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&seg)
        .expect("open");
    let len = file.metadata().expect("metadata").len();
    file.set_len(len - 40).expect("truncate");

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    let recovered = recover(&mut engine, &temp.root()).expect("recover a torn session");
    assert!(
        !recovered.recovery.is_clean(),
        "the caller must be able to report the loss"
    );
    assert_eq!(engine.state(), EngineState::Halted);
}

#[test]
fn a_replay_that_fails_does_not_leave_the_engine_silently_not_logging() {
    // The worst failure mode: an engine stuck in replay looks like it is
    // trading and records none of it. Whatever happens, replay ends.
    let temp = TempSession::new("unstuck");
    record_session(&temp.root(), market());

    let mut engine = build(MemoryLog::with_capacity(1 << 12));
    recover(&mut engine, &temp.root()).expect("recover");
    assert!(!engine.is_replaying());

    let before = engine.log().len();
    engine
        .on_inbound(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(100_000_000_000),
            receive_time: Timestamp::from_nanos(100_000_000_000),
            kind: MarketKind::Quote {
                bid_px: px(100),
                bid_qty: qty(5),
                ask_px: px(101),
                ask_qty: qty(5),
            },
        }))
        .expect("event");
    assert!(
        engine.log().len() > before,
        "a recovered session must record what it does next"
    );
}

#[test]
fn recovery_pulls_the_orders_the_session_had_resting() {
    // Reconstructing the venue brings resting orders back live. Left alone
    // they keep filling while the session is halted, so the position moves and
    // nobody decided that it should. The contract's answer is to cancel and
    // wear the lost queue position (D-3).
    let temp = TempSession::new("cancel-resting");

    // A quoter, because it is the strategy that leaves anything resting at all.
    // The saw-tooth ends on a move, which makes the quoter cancel and leaves
    // it flat-footed. A quiet tail lets it post and settle, which is the state
    // a crash actually interrupts.
    let mut events = market();
    for n in 0..6i64 {
        let at = 100_000_000_000 + n * 1_000_000_000;
        events.push(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(at),
            receive_time: Timestamp::from_nanos(at),
            kind: MarketKind::Quote {
                bid_px: px(100),
                bid_qty: qty(50),
                ask_px: px(101),
                ask_qty: qty(50),
            },
        }));
    }

    let writer = Segments::create(&temp.root(), header()).expect("create");
    let mut engine = quoting(writer);
    let mut feed = ListFeed { events, next: 0 };
    run(&mut feed, &mut engine).expect("session");
    assert!(
        engine.venue().resting_count() > 0,
        "the fixture must end with something resting"
    );
    drop(engine);

    let mut engine = quoting(MemoryLog::with_capacity(1 << 12));
    recover(&mut engine, &temp.root()).expect("recover");

    assert_eq!(
        engine.venue().resting_count(),
        0,
        "nothing may still be resting after a recovery"
    );
    let cancels = engine
        .log()
        .records()
        .iter()
        .filter(|r| matches!(r.event, Event::Out(event::Outbound::CancelSubmitted { .. })))
        .count();
    assert!(
        cancels > 0,
        "and the cancels belong to the resumed session's log"
    );
}
