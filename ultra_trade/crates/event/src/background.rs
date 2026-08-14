//! Recording a session without touching the hot path.
//!
//! [`LogWriter`] writes on the calling thread, which is fine for a backtest and
//! not fine for a live session: the hot path admits no syscall, and a write is
//! one (Constitution VI). This is ADR D-8 — the engine hands records to a ring,
//! and a writer thread drains it.
//!
//! # What the engine thread does
//!
//! Encodes eighty bytes into a stack array and stores ten words into a ring.
//! No allocation, no lock, no syscall. The ring is a single-producer,
//! single-consumer queue built from atomics, with no `unsafe` anywhere — this
//! crate forbids it — which is why the slots are [`AtomicU64`] rather than raw
//! memory.
//!
//! # Backpressure, named rather than emergent
//!
//! Constitution VI requires the answer to "what happens when the consumer falls
//! behind" to be a decision. It is:
//!
//! - **Ring full** → [`append`] returns [`LogError::Full`], and the engine
//!   halts. Not dropped, because a decision that is not in the log makes replay
//!   a lie; not blocked, because blocking is a stall on the hot path.
//! - **The writer thread hit an I/O error** → the next [`append`] returns
//!   [`LogError::WriteFailed`], and the engine halts. A disk that has started
//!   refusing writes must stop the session, not be discovered at shutdown.
//!
//! Both fail closed into the halt the engine already performs when a log append
//! fails.
//!
//! # The honest cost
//!
//! This is the one place where I/O timing can reach the engine's behaviour. If
//! the ring fills, the session halts, and whether it fills depends on how fast
//! the disk is. The alternative — dropping records — is worse, and blocking is
//! not available on the hot path. Size the ring so it cannot happen, and watch
//! [`high_water`] to know how close it came.
//!
//! # What a crash loses
//!
//! Every record that reached the writer thread has been handed to the kernel by
//! `write`, so a **process** crash loses only what is still in the ring. A
//! **machine** crash additionally loses whatever the operating system has not
//! flushed, because this does not fsync per record — [`shutdown`] persists once
//! at the end. Whether that is enough, and what a lost tail means for position,
//! is the crash-recovery contract (D4.4) and is not settled here.
//!
//! [`append`]: EventLog::append
//! [`high_water`]: BackgroundLog::high_water
//! [`shutdown`]: BackgroundLog::shutdown

use crate::codec::{LogHeader, RECORD_LEN, encode_record};
use crate::file::LogFileError;
use crate::segments::Segments;
use crate::{Event, EventLog, LogError, Record, Seq};
use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

/// The ring stores records as `u64` words, so the record size must divide by
/// eight. It is 80; this is here so that a future format change cannot quietly
/// break the ring.
const _: () = assert!(
    RECORD_LEN.is_multiple_of(8),
    "the ring stores records as u64 words, so RECORD_LEN must be a multiple of 8"
);

/// Words per record in the ring.
const WORDS: usize = RECORD_LEN / 8;

/// The smallest ring worth having.
const MIN_CAPACITY: usize = 64;

/// How long the writer sleeps when the ring is empty.
///
/// Long enough not to spin a core, short enough that the tail a crash could
/// lose stays small.
const IDLE: Duration = Duration::from_micros(200);

/// Somewhere a recording can be written.
///
/// Separate from [`std::io::Write`] only for [`persist`]: durability is not
/// something `Write` expresses, and "the bytes reached the kernel" and "the
/// bytes reached the disk" are different guarantees that a trading system has
/// to be able to tell apart.
///
/// [`persist`]: LogSink::persist
pub trait LogSink: Write + Send {
    /// Asks the underlying store to persist what has been written.
    ///
    /// The default does nothing, which is right for a sink with no durability
    /// to offer.
    fn persist(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl LogSink for File {
    fn persist(&mut self) -> io::Result<()> {
        self.sync_all()
    }
}

/// A single-producer, single-consumer ring of encoded records.
///
/// The producer's `Release` store to `head` synchronises with the consumer's
/// `Acquire` load of it, which is what makes the relaxed word stores visible.
/// No `unsafe`, because this crate forbids it and because a hand-rolled
/// lock-free queue is not the place to start needing it.
#[derive(Debug)]
struct Ring {
    slots: Box<[AtomicU64]>,
    /// `capacity - 1`; capacity is always a power of two.
    mask: u64,
    /// Next slot the producer will write.
    head: AtomicU64,
    /// Next slot the consumer will read.
    tail: AtomicU64,
    /// Set by the owner to ask the writer to finish.
    stop: AtomicBool,
    /// Set by the writer when it hits an I/O error.
    failed: AtomicBool,
    /// Records the writer has handed to the sink.
    written: AtomicU64,
}

impl Ring {
    fn new(capacity: usize) -> Ring {
        let capacity = capacity.max(MIN_CAPACITY).next_power_of_two();
        let slots = (0..capacity * WORDS)
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Ring {
            slots,
            mask: capacity as u64 - 1,
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
            stop: AtomicBool::new(false),
            failed: AtomicBool::new(false),
            written: AtomicU64::new(0),
        }
    }

    fn capacity(&self) -> usize {
        self.mask as usize + 1
    }

    fn pending(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        (head - tail) as usize
    }

    /// Producer side. `false` means the ring is full.
    fn push(&self, bytes: &[u8; RECORD_LEN]) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head - tail > self.mask {
            return false;
        }
        let base = ((head & self.mask) as usize) * WORDS;
        for (index, chunk) in bytes.chunks_exact(8).enumerate() {
            let word = u64::from_le_bytes(chunk.try_into().expect("eight bytes"));
            self.slots[base + index].store(word, Ordering::Relaxed);
        }
        // Release: everything above becomes visible to whoever acquires `head`.
        self.head.store(head + 1, Ordering::Release);
        true
    }

    /// Consumer side. `false` means the ring is empty.
    fn pop(&self, out: &mut [u8; RECORD_LEN]) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail == head {
            return false;
        }
        let base = ((tail & self.mask) as usize) * WORDS;
        for index in 0..WORDS {
            let word = self.slots[base + index].load(Ordering::Relaxed);
            out[index * 8..(index + 1) * 8].copy_from_slice(&word.to_le_bytes());
        }
        self.tail.store(tail + 1, Ordering::Release);
        true
    }
}

/// What a finished recording did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriterReport {
    /// Records the engine handed to the ring.
    pub handed_off: u64,
    /// Records the writer handed to the sink.
    pub written: u64,
    /// The deepest the ring ever got.
    ///
    /// Compare it against [`capacity`]: a run that came close was one slow disk
    /// away from halting.
    ///
    /// [`capacity`]: WriterReport::capacity
    pub high_water: usize,
    /// How many records the ring holds.
    pub capacity: usize,
}

/// A log that records off the hot path.
#[derive(Debug)]
pub struct BackgroundLog {
    ring: Arc<Ring>,
    writer: Option<JoinHandle<Result<(), LogFileError>>>,
    next: u64,
    high_water: usize,
}

impl BackgroundLog {
    /// Records to a file, failing if one already exists at that path.
    pub fn create(
        path: impl AsRef<Path>,
        header: LogHeader,
        capacity: usize,
    ) -> Result<BackgroundLog, LogFileError> {
        let file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path.as_ref())
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(LogFileError::AlreadyExists);
            }
            Err(e) => return Err(LogFileError::Io(e)),
        };
        BackgroundLog::spawn(file, header, capacity)
    }

    /// Continues a crashed session, recording into its next segment.
    ///
    /// The damaged segment is read to find where the sequence got to, and
    /// never written (`Segments`, contract D-1). The header comes from the
    /// session rather than being rebuilt, so `InstrumentId(0)` cannot mean a
    /// different contract either side of a restart.
    pub fn resume(root: &Path, capacity: usize) -> Result<BackgroundLog, LogFileError> {
        let (records, _) = Segments::read_all(root)?;
        let next = records.last().map(|r| r.seq.raw() + 1).unwrap_or(0);
        let header = Segments::header(root)?;

        let segment = Segments::next_path(root)?;
        let file = match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&segment)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                return Err(LogFileError::AlreadyExists);
            }
            Err(e) => return Err(LogFileError::Io(e)),
        };
        let mut log = BackgroundLog::spawn(file, header, capacity)?;
        log.next = next;
        Ok(log)
    }

    /// Records to any sink.
    ///
    /// The header is written here rather than on the writer thread, so a sink
    /// that cannot take it fails now instead of at the first append.
    ///
    /// `capacity` is in records, rounded up to a power of two, and is the tail a
    /// process crash could lose.
    pub fn spawn<S: LogSink + 'static>(
        mut sink: S,
        header: LogHeader,
        capacity: usize,
    ) -> Result<BackgroundLog, LogFileError> {
        let mut bytes = Vec::with_capacity(header.encoded_len());
        header.encode(&mut bytes);
        sink.write_all(&bytes)?;

        let ring = Arc::new(Ring::new(capacity));
        let theirs = Arc::clone(&ring);
        let writer = thread::Builder::new()
            .name("ultra_trade-log".into())
            .spawn(move || drain(theirs, sink))
            .map_err(LogFileError::Io)?;

        Ok(BackgroundLog {
            ring,
            writer: Some(writer),
            next: 0,
            high_water: 0,
        })
    }

    /// Records handed to the ring.
    #[inline]
    pub fn records(&self) -> u64 {
        self.next
    }

    /// Records in the ring that the writer has not taken yet.
    ///
    /// The tail a process crash would lose right now.
    #[inline]
    pub fn pending(&self) -> usize {
        self.ring.pending()
    }

    /// The deepest the ring has been.
    #[inline]
    pub fn high_water(&self) -> usize {
        self.high_water
    }

    /// How many records the ring holds.
    #[inline]
    pub fn capacity(&self) -> usize {
        self.ring.capacity()
    }

    /// Whether the writer has hit an I/O error.
    ///
    /// Once this is true every [`append`] fails, so the engine halts on its next
    /// record rather than trading on against a disk that is refusing writes.
    ///
    /// [`append`]: EventLog::append
    #[inline]
    pub fn failed(&self) -> bool {
        self.ring.failed.load(Ordering::Acquire)
    }

    /// Stops the writer, waits for it to finish, and persists.
    ///
    /// Consuming, because a recording that is still being written is not a
    /// recording anyone should be reading.
    pub fn shutdown(mut self) -> Result<WriterReport, LogFileError> {
        let outcome = self.finish();
        let report = WriterReport {
            handed_off: self.next,
            written: self.ring.written.load(Ordering::Acquire),
            high_water: self.high_water,
            capacity: self.ring.capacity(),
        };
        outcome?;
        Ok(report)
    }

    /// Signals the writer and joins it. Safe to call twice.
    fn finish(&mut self) -> Result<(), LogFileError> {
        self.ring.stop.store(true, Ordering::Release);
        match self.writer.take() {
            None => Ok(()),
            Some(handle) => match handle.join() {
                Ok(result) => result,
                // The writer panicked. Nothing it wrote after that point is
                // trustworthy, and saying so is better than reporting success.
                Err(_) => Err(LogFileError::Io(io::Error::other(
                    "the log writer thread panicked",
                ))),
            },
        }
    }
}

impl EventLog for BackgroundLog {
    /// Encodes and hands off. No allocation, no lock, no syscall.
    fn append(&mut self, event: Event) -> Result<Seq, LogError> {
        // Checked first: once the writer has failed, nothing after this point
        // reaches the file, and continuing to trade would be trading unlogged.
        if self.ring.failed.load(Ordering::Relaxed) {
            return Err(LogError::WriteFailed);
        }

        let seq = Seq::new(self.next);
        let record = Record {
            seq,
            version: crate::FORMAT_VERSION,
            event,
        };
        let mut bytes = [0u8; RECORD_LEN];
        encode_record(&record, &mut bytes);

        if !self.ring.push(&bytes) {
            return Err(LogError::Full);
        }
        self.next += 1;

        let pending = self.ring.pending();
        if pending > self.high_water {
            self.high_water = pending;
        }
        Ok(seq)
    }

    fn len(&self) -> usize {
        self.next as usize
    }
}

impl Drop for BackgroundLog {
    /// Best effort, so a log dropped without [`shutdown`] still stops its
    /// thread and persists what it has.
    ///
    /// Best effort is the honest word: a failure here cannot be reported, which
    /// is why `shutdown` exists and why a session that cares calls it.
    ///
    /// [`shutdown`]: BackgroundLog::shutdown
    fn drop(&mut self) {
        let _ = self.finish();
    }
}

/// The writer thread.
fn drain<S: LogSink>(ring: Arc<Ring>, mut sink: S) -> Result<(), LogFileError> {
    let mut buffer = [0u8; RECORD_LEN];
    loop {
        let mut moved = false;
        while ring.pop(&mut buffer) {
            if let Err(e) = sink.write_all(&buffer) {
                // Tell the producer before returning: the engine's next append
                // has to fail, or it trades on unlogged.
                ring.failed.store(true, Ordering::Release);
                return Err(LogFileError::Io(e));
            }
            ring.written.fetch_add(1, Ordering::Relaxed);
            moved = true;
        }

        if moved {
            if let Err(e) = sink.flush() {
                ring.failed.store(true, Ordering::Release);
                return Err(LogFileError::Io(e));
            }
        } else {
            if ring.stop.load(Ordering::Acquire) {
                break;
            }
            thread::sleep(IDLE);
        }
    }

    sink.persist().map_err(LogFileError::Io)
}
