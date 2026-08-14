//! A session recorded across more than one file.
//!
//! A crash damages the file that was open at the time. The recovery contract
//! (`docs/adr/1-crash-recovery-contract.md`, D-1) says that file is left
//! exactly as the crash left it and the session continues in a new one whose
//! first record carries the next sequence number. The chain, not any single
//! file, is the session.
//!
//! What these tests are really protecting is the sequence. Sequence numbers
//! are the engine's only ordering key, so a chain that restarts at zero, skips
//! a number, or repeats one is not a session that can be replayed — it is two
//! sessions in a trench coat.

use event::codec::{InstrumentEntry, LogHeader, RECORD_LEN};
use event::{
    Event, Inbound, LogFileError, LogWriter, MarketEvent, MarketKind, Segments, Seq, StopReason,
};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use types::{Instrument, InstrumentId, OrderId, Px, Qty, Timestamp};

const I: InstrumentId = InstrumentId::new(0);

/// A directory in the system temp directory that deletes itself.
///
/// A whole directory rather than a file, because a chain is several files and
/// the point of the test is often what else is or is not next to them.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ultra_trade-seg-{}-{}-{}",
            std::process::id(),
            n,
            name
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("temp dir");
        TempDir(path)
    }

    /// The session root — the name a person passes on the command line.
    fn root(&self) -> PathBuf {
        self.0.join("session.log")
    }

    fn join(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
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
        42,
        Timestamp::from_nanos(1_000),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument(), "AAPL").expect("entry")],
    )
}

fn quote(bid: i64) -> Event {
    Event::In(Inbound::Market(MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(1_000 + bid),
        receive_time: Timestamp::from_nanos(1_000 + bid),
        kind: MarketKind::Quote {
            bid_px: Px::from_scaled(bid),
            bid_qty: Qty::from_scaled(1),
            ask_px: Px::from_scaled(bid + 1),
            ask_qty: Qty::from_scaled(1),
        },
    }))
}

/// Writes `count` records into a fresh chain and leaves it closed cleanly.
fn write_first(root: &Path, count: i64) {
    let mut writer = Segments::create(root, header()).expect("create");
    for n in 0..count {
        writer.append_record(quote(n)).expect("append");
    }
    writer.sync().expect("sync");
}

/// Cuts `bytes` off the end of a file, which is what a torn tail looks like.
fn tear(path: &Path, bytes: u64) {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open");
    let len = file.metadata().expect("metadata").len();
    file.set_len(len - bytes).expect("truncate");
}

// ---- naming --------------------------------------------------------------

#[test]
fn the_first_segment_is_numbered_rather_than_bare() {
    // `session.log` names the session; the file is `session.0.log`. Numbering
    // from the start means a recovery does not have to rename an existing
    // file — renaming the evidence is exactly what D-1 refuses to do.
    let temp = TempDir::new("naming");
    write_first(&temp.root(), 3);

    assert!(temp.join("session.0.log").exists());
    assert!(
        !temp.root().exists(),
        "the bare root should name the session, not hold records"
    );
}

#[test]
fn resuming_opens_the_next_segment_and_leaves_the_previous_one_alone() {
    let temp = TempDir::new("resume-next");
    write_first(&temp.root(), 4);

    let before = std::fs::read(temp.join("session.0.log")).expect("read");

    let mut writer = Segments::resume(&temp.root()).expect("resume");
    writer.append_record(quote(99)).expect("append");
    writer.sync().expect("sync");

    assert!(temp.join("session.1.log").exists());
    let after = std::fs::read(temp.join("session.0.log")).expect("read");
    assert_eq!(
        before, after,
        "the segment a crash damaged must never be written to again"
    );
}

// ---- the sequence --------------------------------------------------------

#[test]
fn the_new_segment_continues_the_sequence_rather_than_restarting() {
    // The whole point. Sequence numbers are the engine's only ordering key.
    let temp = TempDir::new("continues");
    write_first(&temp.root(), 4); // seq 0..=3

    let mut writer = Segments::resume(&temp.root()).expect("resume");
    let seq = writer.append_record(quote(99)).expect("append");
    assert_eq!(seq, Seq::new(4));
    writer.sync().expect("sync");

    let (records, _) = Segments::read_all(&temp.root()).expect("read");
    let seqs: Vec<u64> = records.iter().map(|r| r.seq.raw()).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3, 4]);
}

#[test]
fn a_torn_tail_is_dropped_and_the_next_segment_starts_after_what_survived() {
    // The expensive case. The crash lost record 3, so the next segment must
    // start at 3 — not at 4, which would leave a hole nothing can explain.
    let temp = TempDir::new("torn");
    write_first(&temp.root(), 4); // seq 0..=3
    tear(&temp.join("session.0.log"), (RECORD_LEN / 2) as u64);

    let mut writer = Segments::resume(&temp.root()).expect("resume");
    assert_eq!(
        writer.append_record(quote(99)).expect("append"),
        Seq::new(3)
    );
    writer.sync().expect("sync");

    let (records, recovery) = Segments::read_all(&temp.root()).expect("read");
    let seqs: Vec<u64> = records.iter().map(|r| r.seq.raw()).collect();
    assert_eq!(seqs, vec![0, 1, 2, 3]);
    assert!(
        !recovery.is_clean(),
        "the damage is still reported; continuing a session does not repair it"
    );
}

#[test]
fn a_chain_of_several_segments_reads_as_one_gap_free_session() {
    // A crash loop makes one segment per attempt (D-1, window 5).
    let temp = TempDir::new("many");
    write_first(&temp.root(), 2);
    for round in 0..3 {
        let mut writer = Segments::resume(&temp.root()).expect("resume");
        writer.append_record(quote(100 + round)).expect("append");
        writer.append_record(quote(200 + round)).expect("append");
        writer.sync().expect("sync");
    }

    assert!(temp.join("session.3.log").exists());
    let (records, recovery) = Segments::read_all(&temp.root()).expect("read");
    assert!(recovery.is_clean());
    let seqs: Vec<u64> = records.iter().map(|r| r.seq.raw()).collect();
    assert_eq!(seqs, (0..8).collect::<Vec<_>>());
}

// ---- refusals ------------------------------------------------------------

#[test]
fn creating_a_session_that_already_exists_is_refused() {
    // A recorded session is evidence. Overwriting one is not something this
    // should be able to do by accident.
    let temp = TempDir::new("exists");
    write_first(&temp.root(), 1);
    assert!(matches!(
        Segments::create(&temp.root(), header()),
        Err(LogFileError::AlreadyExists)
    ));
}

#[test]
fn resuming_a_session_that_was_never_started_is_refused() {
    // Not "silently start a new one". A resume that quietly becomes a fresh
    // session loses the position the operator was trying to recover.
    let temp = TempDir::new("missing");
    assert!(matches!(
        Segments::resume(&temp.root()),
        Err(LogFileError::Io(_))
    ));
}

#[test]
fn a_resumed_segment_carries_the_original_header() {
    // Including the instrument table. A segment that re-derived its own
    // instruments could disagree with the first, and then `InstrumentId(0)`
    // would mean a different contract either side of a restart.
    let temp = TempDir::new("header");
    write_first(&temp.root(), 2);

    let mut writer = Segments::resume(&temp.root()).expect("resume");
    assert_eq!(writer.header(), &header());
    writer.append_record(quote(9)).expect("append");
    writer.sync().expect("sync");

    let (_, recovery) = Segments::read_all(&temp.root()).expect("read");
    assert!(recovery.is_clean());
}

#[test]
fn damage_in_a_middle_segment_stops_the_read_there() {
    // A hole in the middle is worse than a short tail: every record after it
    // has an unexplained gap before it. Stopping is the honest answer.
    let temp = TempDir::new("middle");
    write_first(&temp.root(), 3);
    let mut writer = Segments::resume(&temp.root()).expect("resume");
    writer.append_record(quote(50)).expect("append");
    writer.sync().expect("sync");
    drop(writer);

    tear(&temp.join("session.0.log"), (RECORD_LEN / 2) as u64);

    let (records, recovery) = Segments::read_all(&temp.root()).expect("read");
    assert_eq!(records.len(), 2, "stopped at the damage in segment 0");
    // The tear took record 2, and segment 1 opens at 3 because it was opened
    // before the tear. The honest report is the hole in the chain, not the
    // tear that caused it: the gap is what makes the rest unreadable.
    assert_eq!(
        recovery.stopped,
        Some(StopReason::SequenceGap {
            expected: Seq::new(2),
            found: Seq::new(3),
        })
    );
}

#[test]
fn the_writer_a_resume_returns_is_an_ordinary_log_writer() {
    // A resumed session must be indistinguishable downstream from a fresh
    // one, or every caller grows a branch asking which it got.
    let temp = TempDir::new("plain");
    write_first(&temp.root(), 1);
    let writer: LogWriter = Segments::resume(&temp.root()).expect("resume");
    assert_eq!(writer.records(), 1, "the sequence carries over");
}
