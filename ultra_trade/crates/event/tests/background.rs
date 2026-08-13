//! Recording off the hot path: the happy path, and both ways it fails closed.

use event::codec::{InstrumentEntry, LogHeader};
use event::{
    BackgroundLog, Event, EventLog, Inbound, LogError, LogReader, LogSink, MarketEvent, MarketKind,
    Seq,
};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use types::{Instrument, InstrumentId, OrderId, Px, Qty, Timestamp};

const I: InstrumentId = InstrumentId::new(0);

struct TempLog(PathBuf);

impl TempLog {
    fn new(name: &str) -> TempLog {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ultra_trade-bg-{}-{}-{}.log",
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
    Instrument::new(
        I,
        Px::from_scaled(10_000_000),
        Qty::from_scaled(1_000_000_000),
        Qty::from_scaled(1_000_000_000),
    )
    .expect("conventions")
}

fn header() -> LogHeader {
    LogHeader::new(
        99,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument(), "BG").expect("entry")],
    )
}

fn quote(at: i64) -> Event {
    Event::In(Inbound::Market(MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::Quote {
            bid_px: Px::from_scaled(at),
            bid_qty: Qty::from_scaled(1),
            ask_px: Px::from_scaled(at + 1),
            ask_qty: Qty::from_scaled(1),
        },
    }))
}

/// Waits for a condition the writer thread is responsible for.
///
/// A synchronisation wait, not a race: the condition becomes true and stays
/// true, so the only thing the timeout guards against is a writer that never
/// gets there at all.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if done() {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("timed out waiting for {what}");
}

// ---- the happy path ------------------------------------------------------

#[test]
fn records_handed_to_the_ring_reach_the_file() {
    let temp = TempLog::new("roundtrip");
    let mut log = BackgroundLog::create(temp.path(), header(), 1_024).expect("create");
    for n in 0..500 {
        assert_eq!(log.append(quote(n)), Ok(Seq::new(n as u64)));
    }
    let report = log.shutdown().expect("shutdown");
    assert_eq!(report.handed_off, 500);
    assert_eq!(report.written, 500);

    let mut reader = LogReader::open(temp.path()).expect("open");
    let records = reader.read_all_intact().expect("intact");
    assert_eq!(records.len(), 500);
    for (n, record) in records.iter().enumerate() {
        assert_eq!(record.seq, Seq::new(n as u64));
        assert_eq!(record.event, quote(n as i64));
    }
}

#[test]
fn a_session_longer_than_the_ring_still_arrives_in_order() {
    // The ring wraps many times over, so this exercises the masking rather than
    // only the first lap.
    let temp = TempLog::new("wraps");
    let mut log = BackgroundLog::create(temp.path(), header(), 64).expect("create");
    let mut handed = 0i64;
    while handed < 5_000 {
        match log.append(quote(handed)) {
            Ok(_) => handed += 1,
            // The ring filled: wait for the writer rather than dropping it.
            Err(LogError::Full) => std::thread::sleep(Duration::from_micros(100)),
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
    let report = log.shutdown().expect("shutdown");
    assert_eq!(report.written, 5_000);
    assert!(report.capacity >= 64);

    let mut reader = LogReader::open(temp.path()).expect("open");
    let records = reader.read_all_intact().expect("intact");
    assert_eq!(records.len(), 5_000);
    for (n, record) in records.iter().enumerate() {
        assert_eq!(record.seq, Seq::new(n as u64), "out of order at {n}");
    }
}

#[test]
fn dropping_a_log_without_shutting_it_down_still_writes_what_it_has() {
    let temp = TempLog::new("drop");
    {
        let mut log = BackgroundLog::create(temp.path(), header(), 256).expect("create");
        for n in 0..50 {
            log.append(quote(n)).expect("append");
        }
        // No shutdown: the drop has to stop the writer and persist.
    }
    let mut reader = LogReader::open(temp.path()).expect("open");
    assert_eq!(reader.read_all_intact().expect("intact").len(), 50);
}

#[test]
fn creating_a_recording_never_overwrites_one() {
    let temp = TempLog::new("no-clobber");
    BackgroundLog::create(temp.path(), header(), 64)
        .expect("create")
        .shutdown()
        .expect("shutdown");
    assert!(matches!(
        BackgroundLog::create(temp.path(), header(), 64),
        Err(event::LogFileError::AlreadyExists)
    ));
}

#[test]
fn the_report_says_how_close_the_ring_came_to_filling() {
    let temp = TempLog::new("high-water");
    let mut log = BackgroundLog::create(temp.path(), header(), 512).expect("create");
    assert_eq!(log.high_water(), 0);
    for n in 0..200 {
        log.append(quote(n)).expect("append");
    }
    let report = log.shutdown().expect("shutdown");
    assert!(
        report.high_water > 0,
        "the ring was never observed occupied"
    );
    assert!(
        report.high_water <= report.capacity,
        "{} > {}",
        report.high_water,
        report.capacity
    );
}

// ---- backpressure --------------------------------------------------------

/// Blocks in `write` until it is released.
struct Blocks {
    go: Arc<AtomicBool>,
    written: Arc<AtomicU64>,
}

impl Write for Blocks {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        while !self.go.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        self.written.fetch_add(buf.len() as u64, Ordering::Relaxed);
        Ok(buf.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LogSink for Blocks {}

#[test]
fn a_full_ring_refuses_the_record_rather_than_dropping_it() {
    // Constitution VI wants the answer to "what happens when the consumer falls
    // behind" to be a decision. It is: refuse, so the engine halts. Not drop,
    // because a decision that is not in the log makes replay a lie.
    let go = Arc::new(AtomicBool::new(false));
    let sink = Blocks {
        go: Arc::clone(&go),
        written: Arc::new(AtomicU64::new(0)),
    };
    // The header goes through before the block takes effect.
    go.store(true, Ordering::Release);
    let mut log = BackgroundLog::spawn(sink, header(), 64).expect("spawn");
    let capacity = log.capacity();
    go.store(false, Ordering::Release);

    let mut accepted = 0usize;
    let mut refused = false;
    for n in 0..(capacity * 4) {
        match log.append(quote(n as i64)) {
            Ok(_) => accepted += 1,
            Err(LogError::Full) => {
                refused = true;
                break;
            }
            Err(e) => panic!("unexpected {e:?}"),
        }
    }
    assert!(refused, "the ring never reported full");
    // At most one record is in flight in the writer's hands, so the ring holds
    // its capacity and possibly one more that was already taken.
    assert!(
        (capacity..=capacity + 1).contains(&accepted),
        "accepted {accepted} with a capacity of {capacity}"
    );

    go.store(true, Ordering::Release);
    log.shutdown().expect("shutdown");
}

// ---- a failing disk ------------------------------------------------------

/// Takes a fixed number of bytes and then refuses everything.
struct FailsAfter {
    budget: usize,
}

impl Write for FailsAfter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.budget == 0 {
            return Err(io::Error::new(io::ErrorKind::StorageFull, "no space left"));
        }
        let take = buf.len().min(self.budget);
        self.budget -= take;
        Ok(take)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LogSink for FailsAfter {}

#[test]
fn a_writer_that_hits_an_io_error_stops_the_session() {
    // A disk that has started refusing writes must stop trading, not be
    // discovered at shutdown.
    let header = header();
    let budget = header.encoded_len() + event::codec::RECORD_LEN * 3;
    let mut log = BackgroundLog::spawn(FailsAfter { budget }, header, 256).expect("spawn");

    for n in 0..20 {
        // Some of these succeed; the point is that failure arrives, not when.
        let _ = log.append(quote(n));
    }
    wait_until("the writer to report a failure", || log.failed());

    // Every append from here on refuses, so the engine halts on its next
    // record rather than trading on unlogged.
    assert_eq!(log.append(quote(999)), Err(LogError::WriteFailed));
    assert_eq!(log.append(quote(1_000)), Err(LogError::WriteFailed));

    // And shutdown says what happened rather than reporting success.
    let outcome = log.shutdown();
    assert!(
        outcome.is_err(),
        "shutdown reported success after a failure"
    );
}

#[test]
fn a_sink_that_cannot_take_the_header_fails_immediately() {
    // Written on the calling thread on purpose, so a bad path is a startup
    // failure rather than something discovered at the first append.
    let outcome = BackgroundLog::spawn(FailsAfter { budget: 4 }, header(), 64);
    assert!(matches!(outcome, Err(event::LogFileError::Io(_))));
}
