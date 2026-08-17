//! A session recorded straight to a file, then read back.
//!
//! This is the chain the whole format exists for: engine → codec → file →
//! reader. Each link has its own tests; this is the one that proves they meet.
//!
//! It also produces the artifact `adapters/historical` will consume, so if this
//! passes, a backtest over recorded rather than synthetic data has an input.

use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::codec::{InstrumentEntry, LogHeader};
use event::{
    Command, CommandEvent, Event, Inbound, LogReader, LogWriter, MarketEvent, MarketKind,
    MemoryLog, Outbound, Seq,
};
use marketdata::{Aggregator, BarSpec, BarSubscription};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, Queue, SimVenue};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use strategy::MovingAverageCrossover;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);
const SESSION_ID: u64 = 0xA11CE;
const STEPS: i64 = 400;

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

/// A self-deleting path in the system temp directory.
struct TempLog(PathBuf);

impl TempLog {
    fn new(name: &str) -> TempLog {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ultra_trade-session-{}-{}-{}.log",
            std::process::id(),
            n,
            name
        ));
        let _ = std::fs::remove_file(&path);
        TempLog(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempLog {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn instrument() -> Instrument {
    Instrument::new(I, Px::from_scaled(10_000_000), qty(1), qty(1)).expect("conventions")
}

fn config() -> EngineConfig {
    let mut limits = LimitBook::with_instruments(1);
    limits
        .set(
            I,
            Limits {
                max_position: qty(100),
                max_exposure: money(1_000_000),
                max_order_notional: money(100_000),
                max_orders_in_window: 50,
                rate_window: ExchangeSpan::from_nanos(10_000_000),
                max_quote_age: ExchangeSpan::from_nanos(100_000_000),
            },
        )
        .expect("limits");
    EngineConfig::new(
        vec![instrument()],
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    )
}

fn log_header() -> LogHeader {
    LogHeader::new(
        SESSION_ID,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument(), "TEST").expect("entry")],
    )
}

/// A deterministic triangular path, plus an operator halt and resume so the
/// recorded session carries commands and refusals as well as market data.
fn stream() -> Vec<Inbound> {
    let mut events = Vec::with_capacity(STEPS as usize * 2 + 2);
    let mut clock = 0i64;
    for step in 0..STEPS {
        clock += 1_000_000;
        let phase = step.rem_euclid(40);
        let mid = if phase < 20 {
            90 + phase
        } else {
            110 - (phase - 20)
        };
        events.push(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(clock),
            receive_time: Timestamp::from_nanos(clock),
            kind: MarketKind::Quote {
                bid_px: px(mid - 1),
                bid_qty: qty(200),
                ask_px: px(mid + 1),
                ask_qty: qty(200),
            },
        }));
        clock += 500_000;
        events.push(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(clock),
            receive_time: Timestamp::from_nanos(clock),
            kind: MarketKind::Trade {
                px: px(mid),
                qty: qty(1),
                aggressor: Side::Buy,
            },
        }));
        if step == 200 {
            events.push(Inbound::Command(CommandEvent {
                command: Command::Halt,
                receive_time: Timestamp::from_nanos(clock),
            }));
        }
        if step == 250 {
            events.push(Inbound::Command(CommandEvent {
                command: Command::Resume,
                receive_time: Timestamp::from_nanos(clock),
            }));
        }
    }
    events
}

struct Feed {
    events: Vec<Inbound>,
    next: usize,
}

impl FeedAdapter for Feed {
    fn next_event(&mut self) -> Option<Inbound> {
        let event = self.events.get(self.next).copied()?;
        self.next += 1;
        Some(event)
    }
}

fn sim() -> SimVenue {
    SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    )
}

/// Runs the session, logging wherever the caller says.
fn run_session<L: event::EventLog>(log: L) -> Engine<SimVenue, L> {
    let mut engine = Engine::new(config(), sim(), log);
    let subscription =
        engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 2 }).expect("spec"));
    assert_eq!(subscription, BarSubscription::from_index(0));
    engine
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            I,
            subscription,
            6,
            qty(10),
        )))
        .expect("strategy");
    let mut feed = Feed {
        events: stream(),
        next: 0,
    };
    run(&mut feed, &mut engine).expect("session");
    engine
}

#[test]
fn a_session_recorded_to_a_file_reads_back_exactly() {
    let temp = TempLog::new("engine");

    // The same session twice: once to memory, once straight to the file.
    let in_memory = run_session(MemoryLog::with_capacity(1 << 13));
    let expected = in_memory.log().records().to_vec();

    {
        let writer = LogWriter::create(temp.path(), log_header()).expect("create");
        let recorded = run_session(writer);
        // The engine drove the writer through the EventLog trait, so it never
        // knew it was writing to a file rather than to memory.
        assert_eq!(recorded.log().records(), expected.len() as u64);
    }

    let mut reader = LogReader::open(temp.path()).expect("open");
    let from_disk = reader.read_all_intact().expect("intact");

    assert_eq!(from_disk.len(), expected.len());
    for (a, b) in expected.iter().zip(&from_disk) {
        assert_eq!(a, b, "record {} changed on the way to disk", a.seq);
    }

    // The session did enough for this to mean something.
    let orders = expected
        .iter()
        .filter(|r| matches!(r.event, Event::Out(Outbound::OrderSubmitted { .. })))
        .count();
    assert!(orders > 10, "only {orders} orders in the recorded session");
    assert!(expected.len() > 800, "only {} records", expected.len());
}

#[test]
fn a_recorded_session_carries_its_instrument_table() {
    let temp = TempLog::new("instruments");
    {
        let writer = LogWriter::create(temp.path(), log_header()).expect("create");
        run_session(writer);
    }

    let reader = LogReader::open(temp.path()).expect("open");
    let header = reader.header();
    assert_eq!(header.session_id, SESSION_ID);
    assert_eq!(header.instruments[0].symbol_str(), Some("TEST"));

    // The check that a historical feed must make before replaying this: the
    // recorded conventions have to be the ones the session is configured with,
    // or InstrumentId(0) means a different contract.
    assert!(header.check_against(&[instrument()]).is_ok());
    let different = Instrument::new(
        I,
        Px::from_scaled(50_000_000), // a different tick
        qty(1),
        qty(1),
    )
    .expect("conventions");
    assert!(header.check_against(&[different]).is_err());
}

#[test]
fn a_recorded_session_can_be_replayed_from_the_file() {
    // The point of the exercise: read the inputs back off disk, feed them to a
    // fresh engine, and get the same decisions. This is what
    // `adapters/historical` will do.
    let temp = TempLog::new("replay");
    {
        let writer = LogWriter::create(temp.path(), log_header()).expect("create");
        run_session(writer);
    }

    let mut reader = LogReader::open(temp.path()).expect("open");
    let recorded = reader.read_all_intact().expect("intact");

    let mut replayed = Engine::new(
        config(),
        simkit::ReplayVenue::new(),
        MemoryLog::with_capacity(1 << 13),
    );
    let subscription =
        replayed.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 2 }).expect("spec"));
    replayed
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            I,
            subscription,
            6,
            qty(10),
        )))
        .expect("strategy");

    for record in &recorded {
        if let Some(inbound) = record.event.as_inbound() {
            replayed.on_inbound(*inbound).expect("replay");
        }
    }

    assert_eq!(replayed.log().records(), recorded.as_slice());
}

#[test]
fn a_recorded_session_can_be_read_one_record_at_a_time() {
    let temp = TempLog::new("seek");
    {
        let writer = LogWriter::create(temp.path(), log_header()).expect("create");
        run_session(writer);
    }

    let mut reader = LogReader::open(temp.path()).expect("open");
    let total = reader.record_capacity();
    assert!(total > 800);

    // Seeking straight to the end without walking there.
    let last = reader.read_at(Seq::new(total - 1)).expect("read_at");
    assert_eq!(last.seq, Seq::new(total - 1));
    let first = reader.read_at(Seq::FIRST).expect("read_at");
    assert_eq!(first.seq, Seq::FIRST);
}
