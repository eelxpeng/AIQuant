//! The durable log, its sequencing, and the replay cursor.
//!
//! This is the replay boundary (`docs/ARCHITECTURE.md` seam 1). Everything that
//! influences an order is a record here; if a decision depended on something
//! that is not, replay is a lie.

use crate::{Event, Inbound, Outbound};
use core::fmt;

/// The record format written by this build.
///
/// Stamped on every record from the first release, so that changing the value
/// representation later is a supported migration rather than a break. ADR #4
/// raised this; retrofitting it after logs exist is the expensive order.
/// Version 2 added the book-level records that carry depth. Version 1, which
/// carried top of book only, stays readable: the reader refuses a version
/// *greater* than it knows and no lower one, and `log_file.rs` has a test
/// that says so — "old recordings still read" is exactly the claim that rots
/// without one.
pub const FORMAT_VERSION: u16 = 2;

/// A record's position in the log.
///
/// Monotonic and gap-free, assigned by the log and never by a caller. It is the
/// engine's only ordering key: not exchange time, which can tie or go backwards
/// across venues, and not receive time, which is a different clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Seq(u64);

impl Seq {
    /// The first sequence number a log assigns.
    pub const FIRST: Seq = Seq(0);

    /// Builds a sequence number from its raw position.
    #[inline]
    pub const fn new(raw: u64) -> Seq {
        Seq(raw)
    }

    /// The underlying position.
    #[inline]
    pub const fn raw(self) -> u64 {
        self.0
    }

    /// The next position.
    #[inline]
    const fn next(self) -> Seq {
        Seq(self.0 + 1)
    }
}

impl fmt::Display for Seq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// One entry in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Record {
    /// Where it sits.
    pub seq: Seq,
    /// The format this record was written in.
    pub version: u16,
    /// What happened.
    pub event: Event,
}

/// Why an append failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LogError {
    /// The log has no room for another record.
    Full,
    /// The underlying store refused the write.
    WriteFailed,
}

impl fmt::Display for LogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            LogError::Full => "event log is full",
            LogError::WriteFailed => "event log write failed",
        })
    }
}

impl core::error::Error for LogError {}

/// Somewhere records can be appended.
///
/// `append` is fallible on purpose. A decision that is not in the log is not
/// replayable, so a failed write is not a logging problem to be swallowed — it
/// is a reason to stop trading (Constitution II, V). The engine treats an error
/// here as a halt condition.
pub trait EventLog {
    /// Appends a record and returns the sequence number it was given.
    fn append(&mut self, event: Event) -> Result<Seq, LogError>;

    /// How many records are in the log.
    fn len(&self) -> usize;

    /// Whether the log is empty.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// An in-memory log.
///
/// Backed by a `Vec`, so a push past the reserved capacity allocates. Build it
/// with [`with_capacity`] sized for the session and the hot path never
/// allocates; [`would_grow`] reports whether the next append would
/// (Constitution VI).
///
/// Durability is deliberately not solved here — persisting this to disk is an
/// open architecture decision, and the piece that decision needs from the
/// format, [`FORMAT_VERSION`], is already on every record.
///
/// [`with_capacity`]: MemoryLog::with_capacity
/// [`would_grow`]: MemoryLog::would_grow
#[derive(Debug, Default, Clone)]
pub struct MemoryLog {
    records: Vec<Record>,
    next: u64,
}

impl MemoryLog {
    /// An empty log that will allocate on first append.
    pub fn new() -> MemoryLog {
        MemoryLog {
            records: Vec::new(),
            next: 0,
        }
    }

    /// An empty log with room for `capacity` records reserved up front.
    pub fn with_capacity(capacity: usize) -> MemoryLog {
        MemoryLog {
            records: Vec::with_capacity(capacity),
            next: 0,
        }
    }

    /// Whether the next append would grow the backing store, and so allocate.
    ///
    /// The hot path must not allocate. This is how a caller checks that its
    /// reservation still holds rather than assuming it does.
    #[inline]
    pub fn would_grow(&self) -> bool {
        self.records.len() == self.records.capacity()
    }

    /// Every record, in order.
    #[inline]
    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// A cursor over the whole log, positioned at the start.
    pub fn cursor(&self) -> Cursor<'_> {
        Cursor {
            records: &self.records,
            next: 0,
        }
    }

    /// Just the decisions, in order.
    ///
    /// This is the sequence a replay must reproduce byte-for-byte.
    pub fn outbound(&self) -> impl Iterator<Item = &Outbound> {
        self.records.iter().filter_map(|r| match &r.event {
            Event::Out(o) => Some(o),
            Event::In(_) => None,
        })
    }

    /// Just the inputs, in order.
    ///
    /// This is what a replay feeds back through a fresh engine.
    pub fn inbound(&self) -> impl Iterator<Item = &Inbound> {
        self.records.iter().filter_map(|r| match &r.event {
            Event::In(i) => Some(i),
            Event::Out(_) => None,
        })
    }
}

impl EventLog for MemoryLog {
    fn append(&mut self, event: Event) -> Result<Seq, LogError> {
        let seq = Seq(self.next);
        self.records.push(Record {
            seq,
            version: FORMAT_VERSION,
            event,
        });
        self.next = seq.next().0;
        Ok(seq)
    }

    fn len(&self) -> usize {
        self.records.len()
    }
}

/// A read position in a log.
///
/// Borrows the records rather than copying them, so replaying a session costs
/// nothing beyond the walk itself.
#[derive(Debug, Clone)]
pub struct Cursor<'a> {
    records: &'a [Record],
    next: usize,
}

impl<'a> Cursor<'a> {
    /// The next record, advancing the cursor.
    pub fn next_record(&mut self) -> Option<&'a Record> {
        let record = self.records.get(self.next)?;
        self.next += 1;
        Some(record)
    }

    /// The next record without advancing.
    pub fn peek(&self) -> Option<&'a Record> {
        self.records.get(self.next)
    }

    /// Moves the cursor to the given sequence number.
    ///
    /// Returns whether the position exists. A caller that ignores the answer
    /// and reads on would silently start from the wrong place, so this is not
    /// `()`.
    pub fn seek(&mut self, seq: Seq) -> bool {
        let index = seq.raw() as usize;
        if index <= self.records.len() {
            self.next = index;
            true
        } else {
            false
        }
    }

    /// The sequence number the next read would return.
    pub fn position(&self) -> Seq {
        Seq(self.next as u64)
    }

    /// Whether every record has been read.
    pub fn is_exhausted(&self) -> bool {
        self.next >= self.records.len()
    }
}

impl<'a> Iterator for Cursor<'a> {
    type Item = &'a Record;

    fn next(&mut self) -> Option<&'a Record> {
        self.next_record()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Command, CommandEvent, Inbound};
    use types::ReceiveTime;

    fn command(n: i64) -> Event {
        Event::In(Inbound::Command(CommandEvent {
            command: Command::Halt,
            receive_time: ReceiveTime::from_nanos(n),
        }))
    }

    #[test]
    fn sequence_numbers_start_at_zero_and_are_gap_free() {
        let mut log = MemoryLog::new();
        assert_eq!(log.append(command(1)), Ok(Seq::FIRST));
        assert_eq!(log.append(command(2)), Ok(Seq::new(1)));
        assert_eq!(log.append(command(3)), Ok(Seq::new(2)));
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn an_empty_log_reads_as_empty() {
        let log = MemoryLog::new();
        assert!(log.is_empty());
        assert!(log.cursor().is_exhausted());
        assert_eq!(log.cursor().peek(), None);
    }

    #[test]
    fn every_record_carries_the_format_version() {
        let mut log = MemoryLog::new();
        log.append(command(1)).expect("append");
        assert_eq!(log.records()[0].version, FORMAT_VERSION);
    }

    #[test]
    fn a_cursor_walks_records_in_order() {
        let mut log = MemoryLog::new();
        for n in 0..4 {
            log.append(command(n)).expect("append");
        }
        let seen: Vec<Seq> = log.cursor().map(|r| r.seq).collect();
        assert_eq!(
            seen,
            vec![Seq::new(0), Seq::new(1), Seq::new(2), Seq::new(3)]
        );
    }

    #[test]
    fn seeking_past_the_end_fails_rather_than_silently_landing_elsewhere() {
        let mut log = MemoryLog::new();
        log.append(command(1)).expect("append");
        let mut cursor = log.cursor();
        assert!(cursor.seek(Seq::new(1)));
        assert!(cursor.is_exhausted());
        assert!(!cursor.seek(Seq::new(9)));
        // The failed seek left the position untouched.
        assert_eq!(cursor.position(), Seq::new(1));
    }

    #[test]
    fn a_reserved_log_reports_when_the_next_append_would_allocate() {
        let mut log = MemoryLog::with_capacity(2);
        assert!(!log.would_grow());
        log.append(command(1)).expect("append");
        log.append(command(2)).expect("append");
        assert!(log.would_grow());
    }

    #[test]
    fn inputs_and_decisions_can_be_read_separately() {
        use crate::{EngineState, Outbound, StateReason};
        let mut log = MemoryLog::new();
        log.append(command(1)).expect("append");
        log.append(Event::Out(Outbound::StateChanged {
            caused_by: Seq::FIRST,
            from: EngineState::Running,
            to: EngineState::Halted,
            reason: StateReason::OperatorCommand,
        }))
        .expect("append");
        assert_eq!(log.inbound().count(), 1);
        assert_eq!(log.outbound().count(), 1);
    }
}
