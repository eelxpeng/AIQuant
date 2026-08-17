//! Writing a session to a file and reading it back, including every way the
//! file can be damaged.
//!
//! The recovery paths matter more than the happy one. A writer that is wrong
//! produces files nothing can read, which is loud. A reader that quietly
//! returns a short log says "the session ended here", which is a conclusion
//! someone will act on.

use event::codec::{InstrumentEntry, LogHeader, RECORD_LEN};
use event::{
    Command, CommandEvent, Event, Inbound, LogFileError, LogReader, LogWriter, MarketEvent,
    MarketKind, Recovery, Seq, StopReason,
};
use std::fs::OpenOptions;
use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use types::{ExchangeTime, Instrument, InstrumentId, OrderId, Px, Qty, Timestamp};

const I: InstrumentId = InstrumentId::new(0);

/// A path in the system temp directory that deletes itself.
///
/// Unique per process and per call, so tests running in parallel do not share
/// a file. No randomness and no clock: a counter is enough and stays
/// reproducible.
struct TempLog(PathBuf);

impl TempLog {
    fn new(name: &str) -> TempLog {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "ultra_trade-{}-{}-{}.log",
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

fn instrument(id: u32) -> Instrument {
    Instrument::new(
        InstrumentId::new(id),
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
        vec![
            InstrumentEntry::new(instrument(0), "AAPL").expect("entry"),
            InstrumentEntry::new(instrument(1), "MSFT").expect("entry"),
        ],
    )
}

fn quote(at: i64, bid: i64) -> Event {
    Event::In(Inbound::Market(MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::Quote {
            bid_px: Px::from_scaled(bid),
            bid_qty: Qty::from_scaled(1),
            ask_px: Px::from_scaled(bid + 1),
            ask_qty: Qty::from_scaled(1),
        },
    }))
}

/// Writes `count` quotes and returns the header the file carries.
fn write_session(path: &Path, count: i64) -> LogHeader {
    let h = header();
    let mut writer = LogWriter::create(path, h.clone()).expect("create");
    for n in 0..count {
        writer
            .append_record(quote(1_000 + n, 100 + n))
            .expect("append");
    }
    writer.sync().expect("sync");
    h
}

/// Overwrites bytes at an absolute offset in an existing file.
fn poke(path: &Path, offset: u64, bytes: &[u8]) {
    let mut file = OpenOptions::new().write(true).open(path).expect("open");
    file.seek(SeekFrom::Start(offset)).expect("seek");
    file.write_all(bytes).expect("write");
}

// ---- the happy path ------------------------------------------------------

#[test]
fn a_session_written_to_a_file_reads_back_identically() {
    let temp = TempLog::new("roundtrip");
    let written = write_session(temp.path(), 50);

    let mut reader = LogReader::open(temp.path()).expect("open");
    assert_eq!(reader.header(), &written);

    let records = reader.read_all_intact().expect("intact");
    assert_eq!(records.len(), 50);
    for (index, record) in records.iter().enumerate() {
        assert_eq!(record.seq, Seq::new(index as u64));
        assert_eq!(
            record.event,
            quote(1_000 + index as i64, 100 + index as i64)
        );
    }
}

#[test]
fn a_session_larger_than_the_write_buffer_round_trips() {
    // The buffer holds 256 records, so this crosses it several times and
    // exercises the flush-when-full path rather than only the final flush.
    let temp = TempLog::new("large");
    write_session(temp.path(), 1_000);

    let mut reader = LogReader::open(temp.path()).expect("open");
    assert_eq!(reader.record_capacity(), 1_000);
    let records = reader.read_all_intact().expect("intact");
    assert_eq!(records.len(), 1_000);
    assert_eq!(records[999].seq, Seq::new(999));
}

#[test]
fn a_log_with_no_records_is_clean_rather_than_damaged() {
    // The degenerate case: a session that recorded its header and then nothing.
    let temp = TempLog::new("empty");
    let h = header();
    LogWriter::create(temp.path(), h)
        .expect("create")
        .sync()
        .expect("sync");

    let mut reader = LogReader::open(temp.path()).expect("open");
    let (records, recovery) = reader.read_all().expect("read");
    assert!(records.is_empty());
    assert!(recovery.is_clean());
    assert_eq!(recovery.records, 0);
}

#[test]
fn a_record_can_be_read_by_its_sequence_number_without_reading_the_rest() {
    // The reason the format uses a fixed record size: a seek and one read.
    let temp = TempLog::new("seek");
    write_session(temp.path(), 500);

    let mut reader = LogReader::open(temp.path()).expect("open");
    for seq in [0u64, 1, 250, 499] {
        let record = reader.read_at(Seq::new(seq)).expect("read_at");
        assert_eq!(record.seq, Seq::new(seq));
        assert_eq!(record.event, quote(1_000 + seq as i64, 100 + seq as i64));
    }
}

#[test]
fn reading_past_the_end_says_what_is_there() {
    let temp = TempLog::new("past-end");
    write_session(temp.path(), 3);

    let mut reader = LogReader::open(temp.path()).expect("open");
    match reader.read_at(Seq::new(3)) {
        Err(LogFileError::NoSuchRecord { seq, available }) => {
            assert_eq!(seq, Seq::new(3));
            assert_eq!(available, 3);
        }
        other => panic!("expected NoSuchRecord, got {other:?}"),
    }
}

#[test]
fn creating_a_log_never_overwrites_one() {
    // A recorded session is evidence. Replacing one by accident is not
    // something this should be able to do.
    let temp = TempLog::new("no-clobber");
    write_session(temp.path(), 5);
    assert!(matches!(
        LogWriter::create(temp.path(), header()),
        Err(LogFileError::AlreadyExists)
    ));

    // And the original is untouched.
    let mut reader = LogReader::open(temp.path()).expect("open");
    assert_eq!(reader.read_all_intact().expect("intact").len(), 5);
}

// ---- damage --------------------------------------------------------------

#[test]
fn a_file_that_ends_mid_record_keeps_what_came_before_it() {
    let temp = TempLog::new("torn");
    let h = write_session(temp.path(), 10);

    // Cut 30 bytes into what would have been record 10.
    let cut = h.offset_of(10) + 30;
    let file = OpenOptions::new()
        .write(true)
        .open(temp.path())
        .expect("open");
    file.set_len(cut).expect("truncate");

    let mut reader = LogReader::open(temp.path()).expect("open");
    let (records, recovery) = reader.read_all().expect("read");
    assert_eq!(records.len(), 10, "the intact records must survive");
    assert_eq!(
        recovery.stopped,
        Some(StopReason::TornTail { partial_bytes: 30 })
    );
    assert_eq!(recovery.discarded_bytes, 30);
    assert!(!recovery.is_clean());
}

#[test]
fn a_corrupt_record_stops_the_read_and_names_itself() {
    let temp = TempLog::new("corrupt");
    let h = write_session(temp.path(), 20);

    // Flip a bit inside record 7's payload.
    poke(temp.path(), h.offset_of(7) + 20, &[0xFF]);

    let mut reader = LogReader::open(temp.path()).expect("open");
    let (records, recovery) = reader.read_all().expect("read");
    assert_eq!(records.len(), 7, "records before the damage must survive");
    assert!(matches!(
        recovery.stopped,
        Some(StopReason::Corrupt { index: 7, .. })
    ));
    // Thirteen records from the corrupt one onward.
    assert_eq!(recovery.discarded_bytes, 13 * RECORD_LEN as u64);
}

#[test]
fn a_record_in_the_wrong_place_is_caught_even_though_its_checksum_is_valid() {
    // The checksum cannot see this: the record is internally consistent and
    // sitting at the wrong offset, which is what a partial overwrite or a bad
    // seek produces.
    let temp = TempLog::new("misplaced");
    let h = write_session(temp.path(), 20);

    // Copy record 15 over record 5, checksum and all.
    let bytes = std::fs::read(temp.path()).expect("read");
    let from = h.offset_of(15) as usize;
    let record_15 = bytes[from..from + RECORD_LEN].to_vec();
    poke(temp.path(), h.offset_of(5), &record_15);

    let mut reader = LogReader::open(temp.path()).expect("open");
    let (records, recovery) = reader.read_all().expect("read");
    assert_eq!(records.len(), 5);
    assert_eq!(
        recovery.stopped,
        Some(StopReason::SequenceGap {
            expected: Seq::new(5),
            found: Seq::new(15),
        })
    );
}

#[test]
fn an_intact_read_refuses_a_damaged_file_rather_than_returning_a_short_log() {
    let temp = TempLog::new("intact-refuses");
    let h = write_session(temp.path(), 10);
    let file = OpenOptions::new()
        .write(true)
        .open(temp.path())
        .expect("open");
    file.set_len(h.offset_of(9) + 1).expect("truncate");

    let mut reader = LogReader::open(temp.path()).expect("open");
    match reader.read_all_intact() {
        Err(LogFileError::Damaged(recovery)) => {
            assert_eq!(recovery.records, 9);
            assert!(!recovery.is_clean());
            // The report reads as a sentence, because someone will read it at
            // three in the morning.
            let text = recovery.to_string();
            assert!(text.contains("9 records read"), "{text}");
            assert!(text.contains("ends 1 bytes into a record"), "{text}");
        }
        other => panic!("expected Damaged, got {other:?}"),
    }
}

#[test]
fn a_file_that_is_not_a_log_is_refused_on_open() {
    let temp = TempLog::new("not-a-log");
    std::fs::write(
        temp.path(),
        b"this is not an ultra_trade log at all, really",
    )
    .expect("write");
    assert!(matches!(
        LogReader::open(temp.path()),
        Err(LogFileError::Codec(_))
    ));
}

#[test]
fn a_damaged_header_is_refused_before_any_record_is_read() {
    let temp = TempLog::new("bad-header");
    write_session(temp.path(), 5);
    poke(temp.path(), 0, b"X");
    assert!(matches!(
        LogReader::open(temp.path()),
        Err(LogFileError::Codec(_))
    ));
}

// ---- the bounded tail ----------------------------------------------------

#[test]
fn a_writer_reports_how_much_a_crash_would_lose() {
    let temp = TempLog::new("unflushed");
    let mut writer = LogWriter::create(temp.path(), header()).expect("create");
    assert_eq!(writer.unflushed(), 0);

    for n in 0..10 {
        writer.append_record(quote(n, n)).expect("append");
    }
    assert_eq!(writer.unflushed(), 10, "these ten are only in memory");
    assert_eq!(writer.records(), 10);

    writer.flush().expect("flush");
    assert_eq!(writer.unflushed(), 0);
}

#[test]
fn the_buffer_flushes_itself_before_it_grows_without_bound() {
    let temp = TempLog::new("autoflush");
    let mut writer = LogWriter::create(temp.path(), header()).expect("create");
    for n in 0..1_000 {
        writer.append_record(quote(n, n)).expect("append");
    }
    // 1000 records through a 256-record buffer: at most a partial buffer is
    // still in memory.
    assert!(
        writer.unflushed() < 256,
        "{} records buffered — the flush threshold did not fire",
        writer.unflushed()
    );
}

#[test]
fn dropping_a_writer_still_flushes_what_it_holds() {
    let temp = TempLog::new("drop-flush");
    {
        let mut writer = LogWriter::create(temp.path(), header()).expect("create");
        for n in 0..5 {
            writer.append_record(quote(n, n)).expect("append");
        }
        assert_eq!(writer.unflushed(), 5);
        // No explicit flush: the drop has to do it.
    }
    let mut reader = LogReader::open(temp.path()).expect("open");
    assert_eq!(reader.read_all_intact().expect("intact").len(), 5);
}

#[test]
fn a_writer_records_operator_commands_like_anything_else() {
    // Commands are inputs, so they are in the file with everything else. A
    // format that could not hold one would make replay a lie.
    let temp = TempLog::new("commands");
    {
        let mut writer = LogWriter::create(temp.path(), header()).expect("create");
        writer.append_record(quote(1, 1)).expect("append");
        writer
            .append_record(Event::In(Inbound::Command(CommandEvent {
                command: Command::Kill,
                receive_time: Timestamp::from_nanos(2),
            })))
            .expect("append");
        writer.sync().expect("sync");
    }
    let mut reader = LogReader::open(temp.path()).expect("open");
    let records = reader.read_all_intact().expect("intact");
    assert!(matches!(
        records[1].event,
        Event::In(Inbound::Command(CommandEvent {
            command: Command::Kill,
            ..
        }))
    ));
}

#[test]
fn the_header_records_what_the_session_could_trade() {
    let temp = TempLog::new("header");
    write_session(temp.path(), 1);
    let reader = LogReader::open(temp.path()).expect("open");

    let h = reader.header();
    assert_eq!(h.session_id, 42);
    assert_eq!(h.session_start, ExchangeTime::from_nanos(1_000));
    assert_eq!(h.instruments.len(), 2);
    assert_eq!(h.instruments[0].symbol_str(), Some("AAPL"));
    assert_eq!(h.instruments[1].symbol_str(), Some("MSFT"));
    // And it still refuses a session configured differently.
    assert!(h.check_against(&[instrument(0)]).is_err());
    assert!(h.check_against(&[instrument(0), instrument(1)]).is_ok());
}

#[test]
fn a_clean_recovery_says_so_plainly() {
    let temp = TempLog::new("clean-report");
    write_session(temp.path(), 4);
    let mut reader = LogReader::open(temp.path()).expect("open");
    let (_, recovery) = reader.read_all().expect("read");
    assert_eq!(
        recovery,
        Recovery {
            records: 4,
            discarded_bytes: 0,
            stopped: None
        }
    );
    assert_eq!(recovery.to_string(), "4 records, intact");
}

#[test]
fn a_recording_written_before_depth_existed_still_reads() {
    // Format 2 added the book-level records. The reader refuses only a version
    // *greater* than it knows, so version-1 files replay unchanged — and this
    // is the claim that would otherwise rot silently the next time the format
    // moves (ADR, order-book depth D-4).
    let temp = TempLog::new("v1-compat");
    write_session(temp.path(), 5);

    // Rewrite the header's version field to 1, exactly as an older build wrote
    // it. The records themselves are byte-identical: nothing in a quote or a
    // trade changed between the versions. The field sits straight after the
    // eight-byte magic.
    poke(temp.path(), 8, &1u16.to_le_bytes());

    let mut reader = LogReader::open(temp.path()).expect("a v1 log must still open");
    assert_eq!(reader.header().format_version, 1);
    let records = reader.read_all_intact().expect("and still read");
    assert_eq!(records.len(), 5);
}
