//! Backtesting and replaying a recorded session.
//!
//! The headline is **D1.2**: book building reproduces a recorded session's
//! top-of-book exactly on replay. Not the final book — every top-of-book the
//! strategy was shown, in order.

use engine::{Engine, EngineConfig, FeedAdapter, run};
use event::codec::{InstrumentEntry, LogHeader};
use event::{
    Command, CommandEvent, Event, Inbound, LogFileError, LogWriter, MarketEvent, MarketKind,
    MemoryLog, Outbound, Record, StopReason,
};
use historical::{HistoricalFeed, Replaying};
use marketdata::{Aggregator, BarSpec, BarSubscription, TopOfBook};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use std::cell::RefCell;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicU32, Ordering};
use strategy::{Context, MovingAverageCrossover, Strategy, StrategyEvent};
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);
const STEPS: i64 = 300;

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}
fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}
fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

struct TempLog(PathBuf);

impl TempLog {
    fn new(name: &str) -> TempLog {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ultra_trade-hist-{}-{}-{}.log",
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

fn instruments() -> Vec<Instrument> {
    vec![instrument()]
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
        instruments(),
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    )
}

fn log_header() -> LogHeader {
    LogHeader::new(
        7,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument(), "TEST").expect("entry")],
    )
}

fn sim() -> SimVenue {
    SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    )
}

/// Records every top-of-book it is shown, in order.
#[derive(Debug)]
struct BookWatcher {
    inner: MovingAverageCrossover,
    seen: Rc<RefCell<Vec<TopOfBook>>>,
}

impl Strategy for BookWatcher {
    fn id(&self) -> StrategyId {
        self.inner.id()
    }

    fn on_event(&mut self, event: &StrategyEvent<'_>, ctx: &mut Context<'_>) {
        if let StrategyEvent::Quote { top, .. } = event {
            self.seen.borrow_mut().push(**top);
        }
        self.inner.on_event(event, ctx);
    }
}

fn watcher(seen: &Rc<RefCell<Vec<TopOfBook>>>) -> Box<BookWatcher> {
    Box::new(BookWatcher {
        inner: MovingAverageCrossover::new(
            StrategyId::new(0),
            I,
            BarSubscription::from_index(0),
            6,
            qty(10),
        ),
        seen: Rc::clone(seen),
    })
}

/// A deterministic market. `commands` adds an operator halt and resume, so a
/// test can choose whether the recording carries operator actions.
fn market(commands: bool) -> Vec<Inbound> {
    let mut events = Vec::new();
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
        if commands && step == 150 {
            events.push(Inbound::Command(CommandEvent {
                command: Command::Halt,
                receive_time: Timestamp::from_nanos(clock),
            }));
        }
        if commands && step == 180 {
            events.push(Inbound::Command(CommandEvent {
                command: Command::Resume,
                receive_time: Timestamp::from_nanos(clock),
            }));
        }
    }
    events
}

struct ListFeed {
    events: Vec<Inbound>,
    next: usize,
}

impl FeedAdapter for ListFeed {
    fn next_event(&mut self) -> Option<Inbound> {
        let e = self.events.get(self.next).copied()?;
        self.next += 1;
        Some(e)
    }
}

fn wire<V: oms::VenueAdapter, L: event::EventLog>(
    venue: V,
    log: L,
    seen: &Rc<RefCell<Vec<TopOfBook>>>,
) -> Engine<V, L> {
    let mut engine = Engine::new(config(), venue, log);
    engine.add_aggregator(Aggregator::new(I, BarSpec::Tick { threshold: 2 }).expect("spec"));
    engine.add_strategy(watcher(seen)).expect("strategy");
    engine
}

/// Records a session to `path` and returns the books its strategy saw.
fn record(path: &Path, commands: bool) -> (Vec<TopOfBook>, Vec<Record>) {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let writer = LogWriter::create(path, log_header()).expect("create");
    let mut engine = wire(sim(), writer, &seen);
    let mut feed = ListFeed {
        events: market(commands),
        next: 0,
    };
    run(&mut feed, &mut engine).expect("session");
    drop(engine);

    let mut reader = event::LogReader::open(path).expect("open");
    let records = reader.read_all_intact().expect("intact");
    let books = seen.borrow().clone();
    (books, records)
}

// ---- D1.2 ----------------------------------------------------------------

#[test]
fn book_building_reproduces_a_recorded_sessions_top_of_book_exactly() {
    let temp = TempLog::new("d1-2");
    let (recorded_books, _) = record(temp.path(), true);
    assert!(
        recorded_books.len() >= STEPS as usize,
        "only {} books observed",
        recorded_books.len()
    );

    let replayed_books = Rc::new(RefCell::new(Vec::new()));
    let mut feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly).expect("open");
    let mut engine = wire(sim(), MemoryLog::with_capacity(1 << 13), &replayed_books);
    run(&mut feed, &mut engine).expect("backtest");

    let replayed = replayed_books.borrow();
    assert_eq!(
        replayed.len(),
        recorded_books.len(),
        "a different number of book updates reached the strategy"
    );
    for (index, (a, b)) in recorded_books.iter().zip(replayed.iter()).enumerate() {
        assert_eq!(a, b, "top of book diverges at update {index}");
    }
}

#[test]
fn a_backtest_with_the_same_strategy_reproduces_the_recorded_decisions() {
    // No operator actions in this recording. Same market, same strategy, same
    // venue — so the decisions must come out identical even though the venue
    // reports were regenerated by the simulated venue rather than replayed.
    // That is backtest/live parity with the seam moved to the file.
    let temp = TempLog::new("same-decisions");
    let (_, recorded) = record(temp.path(), false);
    let recorded_decisions: Vec<Outbound> = recorded
        .iter()
        .filter_map(|r| r.event.as_outbound().copied())
        .collect();
    assert!(
        recorded_decisions.len() > 10,
        "only {} decisions — the recording is too tame to prove anything",
        recorded_decisions.len()
    );

    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly).expect("open");
    let mut engine = wire(sim(), MemoryLog::with_capacity(1 << 13), &seen);
    run(&mut feed, &mut engine).expect("backtest");

    let fresh: Vec<Outbound> = engine.log().outbound().copied().collect();
    assert_eq!(fresh, recorded_decisions);
}

#[test]
fn a_backtest_over_a_session_that_had_operator_actions_diverges_from_it() {
    // The semantics of MarketDataOnly, made concrete. The recorded operator
    // halted trading; this run never sees that command, so it keeps trading
    // through the window the original sat out.
    //
    // This is correct and it is surprising, which is why it is pinned. Anyone
    // who later "fixes" the divergence by replaying commands will fail here and
    // have to argue for it: replaying an operator's response to a state this
    // session was never in is not a backtest of anything.
    let temp = TempLog::new("diverges");
    let (_, recorded) = record(temp.path(), true);
    let recorded_decisions: Vec<Outbound> = recorded
        .iter()
        .filter_map(|r| r.event.as_outbound().copied())
        .collect();

    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly).expect("open");
    let mut engine = wire(sim(), MemoryLog::with_capacity(1 << 13), &seen);
    run(&mut feed, &mut engine).expect("backtest");
    let fresh: Vec<Outbound> = engine.log().outbound().copied().collect();

    assert_ne!(fresh, recorded_decisions);
    // And it diverges exactly where the operator acted: the recorded session's
    // next decision is the halt, and this one's is an order it went on to place.
    let at = fresh
        .iter()
        .zip(&recorded_decisions)
        .position(|(a, b)| a != b)
        .expect("they must differ somewhere");
    assert!(
        matches!(
            recorded_decisions[at],
            Outbound::StateChanged {
                to: event::EngineState::Halted,
                reason: event::StateReason::OperatorCommand,
                ..
            }
        ),
        "expected the divergence to start at the operator's halt, got {:?}",
        recorded_decisions[at]
    );
    // A replay, by contrast, does reproduce it — that is the other reading.
    let mut replay =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::EveryInput).expect("open");
    let seen2 = Rc::new(RefCell::new(Vec::new()));
    let mut replayed = wire(
        simkit::ReplayVenue::new(),
        MemoryLog::with_capacity(1 << 13),
        &seen2,
    );
    run(&mut replay, &mut replayed).expect("replay");
    let replayed_decisions: Vec<Outbound> = replayed.log().outbound().copied().collect();
    assert_eq!(replayed_decisions, recorded_decisions);
}

// ---- the two readings ----------------------------------------------------

#[test]
fn a_backtest_replays_market_data_and_nothing_else() {
    let temp = TempLog::new("market-only");
    let (_, recorded) = record(temp.path(), true);

    let feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly).expect("open");
    let market_records = recorded
        .iter()
        .filter(|r| matches!(r.event, Event::In(Inbound::Market(_))))
        .count();
    assert_eq!(feed.len(), market_records);

    // The recorded session definitely held other inputs, or this proves nothing.
    let other = recorded
        .iter()
        .filter(|r| matches!(r.event, Event::In(i) if !matches!(i, Inbound::Market(_))))
        .count();
    assert!(other > 0, "the recording had no venue reports or commands");
}

#[test]
fn a_replay_feeds_back_every_input() {
    let temp = TempLog::new("every-input");
    let (_, recorded) = record(temp.path(), true);
    let inputs = recorded
        .iter()
        .filter(|r| r.event.as_inbound().is_some())
        .count();

    let feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::EveryInput).expect("open");
    assert_eq!(feed.len(), inputs);
}

#[test]
fn a_replay_against_a_silent_venue_reproduces_the_whole_log() {
    let temp = TempLog::new("replay-log");
    let (_, recorded) = record(temp.path(), true);

    let seen = Rc::new(RefCell::new(Vec::new()));
    let mut feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::EveryInput).expect("open");
    let mut engine = wire(
        simkit::ReplayVenue::new(),
        MemoryLog::with_capacity(1 << 13),
        &seen,
    );
    run(&mut feed, &mut engine).expect("replay");

    assert_eq!(engine.log().records(), recorded.as_slice());
}

#[test]
fn the_recorded_decisions_are_kept_but_never_replayed() {
    let temp = TempLog::new("decisions");
    let (_, recorded) = record(temp.path(), true);
    let expected = recorded
        .iter()
        .filter(|r| r.event.as_outbound().is_some())
        .count();

    let feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly).expect("open");
    assert_eq!(feed.recorded_decisions().len(), expected);
    // And none of them are among what the feed will hand out.
    let mut feed = feed;
    while let Some(event) = feed.next_event() {
        assert!(matches!(event, Inbound::Market(_)));
    }
}

// ---- refusals ------------------------------------------------------------

#[test]
fn a_session_recorded_under_different_conventions_is_refused() {
    // The check that matters. Without it, InstrumentId(0) in the file silently
    // means whatever instrument 0 is now.
    let temp = TempLog::new("mismatch");
    record(temp.path(), true);

    let different =
        vec![Instrument::new(I, Px::from_scaled(50_000_000), qty(1), qty(1)).expect("conventions")];
    assert!(matches!(
        HistoricalFeed::open(temp.path(), &different, Replaying::MarketDataOnly),
        Err(LogFileError::Codec(_))
    ));
}

#[test]
fn a_damaged_recording_is_refused_unless_the_caller_asks_to_salvage_it() {
    let temp = TempLog::new("damaged");
    let (_, recorded) = record(temp.path(), true);
    let header = log_header();

    // Cut the file part-way through the last record.
    let keep = header.offset_of(recorded.len() as u64 - 1) + 20;
    OpenOptions::new()
        .write(true)
        .open(temp.path())
        .expect("open")
        .set_len(keep)
        .expect("truncate");

    assert!(matches!(
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly),
        Err(LogFileError::Damaged(_))
    ));

    let salvaged =
        HistoricalFeed::open_salvaged(temp.path(), &instruments(), Replaying::MarketDataOnly)
            .expect("salvage");
    let recovery = salvaged.recovery();
    assert!(!recovery.is_clean(), "the caller must be able to see this");
    assert!(matches!(
        recovery.stopped,
        Some(StopReason::TornTail { partial_bytes: 20 })
    ));
    assert_eq!(recovery.records, recorded.len() as u64 - 1);
}

#[test]
fn a_file_that_is_not_a_recording_is_refused() {
    let temp = TempLog::new("garbage");
    std::fs::write(temp.path(), b"not a log, just some bytes sitting in a file").expect("write");
    assert!(matches!(
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly),
        Err(LogFileError::Codec(_))
    ));
}

// ---- timers --------------------------------------------------------------

#[test]
fn a_backtest_fires_the_timers_a_strategy_asks_for() {
    // A strategy whose timers silently never fired in backtest would behave
    // differently in live, which is the class of difference Constitution III
    // exists to prevent.
    let temp = TempLog::new("timers");
    record(temp.path(), true);

    let mut feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly).expect("open");
    let first = feed.next_event().expect("an input");
    let after = first.exchange_time().expect("market data has one");

    feed.schedule_timer(strategy::TimerRequest {
        strategy: StrategyId::new(0),
        at: Timestamp::from_nanos(after.to_nanos() + 1),
        token: event::TimerToken::new(9),
    });

    let next = feed.next_event().expect("an input");
    assert!(
        matches!(next, Inbound::Timer(t) if t.token == event::TimerToken::new(9)),
        "the timer should have fired before the next market event, got {next:?}"
    );
}

#[test]
fn a_replay_ignores_timer_requests_because_the_firings_are_already_recorded() {
    let temp = TempLog::new("replay-timers");
    record(temp.path(), true);

    let mut feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::EveryInput).expect("open");
    let before = feed.remaining();
    feed.schedule_timer(strategy::TimerRequest {
        strategy: StrategyId::new(0),
        at: Timestamp::from_nanos(1),
        token: event::TimerToken::new(1),
    });
    // Nothing was added, and the first input is still a recorded one.
    assert_eq!(feed.remaining(), before);
    assert!(!matches!(feed.next_event(), Some(Inbound::Timer(_))));
}

#[test]
fn the_header_travels_with_the_feed() {
    let temp = TempLog::new("header");
    record(temp.path(), true);
    let feed =
        HistoricalFeed::open(temp.path(), &instruments(), Replaying::MarketDataOnly).expect("open");
    assert_eq!(feed.header().session_id, 7);
    assert_eq!(feed.header().instruments[0].symbol_str(), Some("TEST"));
    assert_eq!(feed.mode(), Replaying::MarketDataOnly);
}
