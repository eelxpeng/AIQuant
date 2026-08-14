//! Reading and writing a recorded session on disk.
//!
//! The format is decided in `docs/adr/1-recorded-log-format.md` and encoded by
//! [`crate::codec`]; this module is the file handling around it.
//!
//! # What a reader does with a damaged file
//!
//! It stops at the first record it cannot trust and **reports what it dropped**
//! (ADR, D-7). It never silently returns a short log, because a short log is
//! indistinguishable from a complete one to everything downstream — and "the
//! session ended here" is exactly the wrong conclusion to draw quietly.
//!
//! Three things end a read: a file that stops mid-record, a record whose
//! checksum does not match, and a record whose sequence number is not the one
//! its position implies. The last catches a class the checksum cannot — a record
//! that is internally valid but written to the wrong place.
//!
//! # What this module does not do
//!
//! It writes on the calling thread. The ADR's D-8 puts persistence off the hot
//! path, which needs a handoff this does not have yet, so **this is not yet
//! suitable for a live session's engine thread**. A backtest, a recorder, and
//! every test are fine. The threading and the crash-recovery contract (D4.4)
//! are the next step.

use crate::codec::{CodecError, LogHeader, RECORD_LEN, decode_record, encode_record};
use crate::{Event, EventLog, LogError, Record, Seq};
use std::fmt;
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::Path;

/// Records buffered before a write reaches the file.
///
/// This is the size of the tail a crash can lose, in records. Small enough that
/// the loss is bounded, large enough that a session is not one syscall per
/// event.
const BUFFERED_RECORDS: usize = 256;

/// What file handling refused.
#[derive(Debug)]
pub enum LogFileError {
    /// The filesystem said no.
    Io(io::Error),
    /// The bytes are not a well-formed log.
    Codec(CodecError),
    /// The file already exists.
    ///
    /// Creating a recorder never overwrites: a recorded session is evidence,
    /// and silently replacing one is not a thing this should be able to do by
    /// accident.
    AlreadyExists,
    /// A read asked for a record past the end of the file.
    NoSuchRecord {
        /// What was asked for.
        seq: Seq,
        /// How many intact records the file holds.
        available: u64,
    },
    /// A read found damage and the caller asked for an intact log.
    Damaged(Recovery),
    /// A session's segments skip a number, so a file is missing.
    ///
    /// Reading the segments before the hole would report part of a session as
    /// all of it, which is a conclusion someone acts on (`Segments`).
    MissingSegment {
        /// The segment that is not there.
        missing: u32,
        /// A later segment that is, which is what makes this a hole rather
        /// than the end of the chain.
        found: u32,
    },
}

impl fmt::Display for LogFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogFileError::Io(e) => write!(f, "{e}"),
            LogFileError::Codec(e) => write!(f, "{e}"),
            LogFileError::AlreadyExists => f.write_str("a log already exists at that path"),
            LogFileError::NoSuchRecord { seq, available } => {
                write!(f, "no record at {seq}; the file holds {available}")
            }
            LogFileError::Damaged(recovery) => write!(f, "{recovery}"),
            LogFileError::MissingSegment { missing, found } => write!(
                f,
                "this session is missing segment {missing}, but segment {found} is present"
            ),
        }
    }
}

impl std::error::Error for LogFileError {}

impl From<io::Error> for LogFileError {
    fn from(e: io::Error) -> LogFileError {
        LogFileError::Io(e)
    }
}

impl From<CodecError> for LogFileError {
    fn from(e: CodecError) -> LogFileError {
        LogFileError::Codec(e)
    }
}

/// Why a read stopped before the end of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The file ends part-way through a record, because the process died while
    /// writing one.
    TornTail {
        /// How many bytes of the incomplete record were there.
        partial_bytes: u64,
    },
    /// A record's contents do not match its checksum, or are otherwise not
    /// decodable.
    Corrupt {
        /// Which position in the file.
        index: u64,
        /// What the codec said.
        error: CodecError,
    },
    /// A record's sequence number is not the one its position implies.
    ///
    /// The checksum cannot catch this: the record is internally consistent and
    /// in the wrong place, which is what a partial overwrite or a bad seek
    /// produces.
    SequenceGap {
        /// What the position implies.
        expected: Seq,
        /// What the record says.
        found: Seq,
    },
}

/// What a read salvaged, and what it could not.
///
/// `#[must_use]` because ignoring it is the mistake this type exists to
/// prevent: a caller that drops it has a log that may be short and does not
/// know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use]
pub struct Recovery {
    /// Records read successfully.
    pub records: u64,
    /// Bytes after the last intact record that were not read.
    pub discarded_bytes: u64,
    /// Why the read stopped, if it stopped early.
    pub stopped: Option<StopReason>,
}

impl Recovery {
    /// Whether the whole file was read with nothing dropped.
    #[inline]
    pub const fn is_clean(&self) -> bool {
        self.stopped.is_none() && self.discarded_bytes == 0
    }
}

impl fmt::Display for Recovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_clean() {
            return write!(f, "{} records, intact", self.records);
        }
        write!(
            f,
            "{} records read, {} bytes discarded",
            self.records, self.discarded_bytes
        )?;
        match self.stopped {
            Some(StopReason::TornTail { partial_bytes }) => {
                write!(f, "; the file ends {partial_bytes} bytes into a record")
            }
            Some(StopReason::Corrupt { index, error }) => {
                write!(f, "; record {index} did not decode: {error}")
            }
            Some(StopReason::SequenceGap { expected, found }) => {
                write!(f, "; expected {expected} at that position, found {found}")
            }
            None => Ok(()),
        }
    }
}

/// Writes a session to a file.
///
/// Buffered: [`append`] encodes into memory and the bytes reach the file when
/// the buffer fills or [`flush`] is called. A crash therefore loses at most
/// [`unflushed`] records, which is the bounded tail loss D-8 chose.
///
/// [`append`]: LogWriter::append
/// [`flush`]: LogWriter::flush
/// [`unflushed`]: LogWriter::unflushed
#[derive(Debug)]
pub struct LogWriter {
    file: File,
    header: LogHeader,
    next: u64,
    buffer: Vec<u8>,
    last_error: Option<io::Error>,
}

impl LogWriter {
    /// Creates a new log, failing if one already exists at that path.
    pub fn create(path: impl AsRef<Path>, header: LogHeader) -> Result<LogWriter, LogFileError> {
        LogWriter::create_at(path, header, Seq::FIRST)
    }

    /// Creates a log whose first record carries `first`.
    ///
    /// A session continued after a crash starts part-way through its own
    /// sequence (`Segments`, and the recovery contract's D-1). Sequence
    /// numbers are the engine's only ordering key, so a continued segment that
    /// restarted at zero would not be the same session — it would be a second
    /// one wearing the first one's name.
    pub fn create_at(
        path: impl AsRef<Path>,
        header: LogHeader,
        first: Seq,
    ) -> Result<LogWriter, LogFileError> {
        let mut file = match OpenOptions::new()
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

        let mut bytes = Vec::with_capacity(header.encoded_len());
        header.encode(&mut bytes);
        file.write_all(&bytes)?;

        Ok(LogWriter {
            file,
            header,
            next: first.raw(),
            buffer: Vec::with_capacity(BUFFERED_RECORDS * RECORD_LEN),
            last_error: None,
        })
    }

    /// What the session was recorded under.
    #[inline]
    pub fn header(&self) -> &LogHeader {
        &self.header
    }

    /// How many records have been handed to this writer.
    #[inline]
    pub fn records(&self) -> u64 {
        self.next
    }

    /// How many records are buffered and not yet in the file.
    ///
    /// The size of the tail a crash would lose right now.
    #[inline]
    pub fn unflushed(&self) -> usize {
        self.buffer.len() / RECORD_LEN
    }

    /// Appends a record, returning the sequence number it was given.
    pub fn append_record(&mut self, event: Event) -> Result<Seq, LogFileError> {
        let seq = Seq::new(self.next);
        let record = Record {
            seq,
            version: crate::FORMAT_VERSION,
            event,
        };
        let mut encoded = [0u8; RECORD_LEN];
        encode_record(&record, &mut encoded);
        self.buffer.extend_from_slice(&encoded);
        self.next += 1;

        if self.buffer.len() >= BUFFERED_RECORDS * RECORD_LEN {
            self.flush()?;
        }
        Ok(seq)
    }

    /// Writes everything buffered to the file.
    ///
    /// Does not ask the filesystem to persist it — that is [`sync`].
    ///
    /// [`sync`]: LogWriter::sync
    pub fn flush(&mut self) -> Result<(), LogFileError> {
        if self.buffer.is_empty() {
            return Ok(());
        }
        match self.file.write_all(&self.buffer) {
            Ok(()) => {
                self.buffer.clear();
                Ok(())
            }
            Err(e) => {
                // The buffer is deliberately left alone. A caller that retries
                // still has the records; a caller that gives up has not been
                // told they were written when they were not.
                let kind = e.kind();
                self.last_error = Some(e);
                Err(LogFileError::Io(io::Error::from(kind)))
            }
        }
    }

    /// Flushes, then asks the filesystem to persist what was written.
    pub fn sync(&mut self) -> Result<(), LogFileError> {
        self.flush()?;
        self.file.sync_all()?;
        Ok(())
    }

    /// The last I/O error this writer saw, if any.
    ///
    /// The [`EventLog`] impl flattens every failure to `WriteFailed`, because
    /// the engine's response is the same either way — halt. This is how a
    /// caller finds out what actually happened.
    #[inline]
    pub fn last_error(&self) -> Option<&io::Error> {
        self.last_error.as_ref()
    }
}

impl EventLog for LogWriter {
    fn append(&mut self, event: Event) -> Result<Seq, LogError> {
        self.append_record(event).map_err(|_| LogError::WriteFailed)
    }

    fn len(&self) -> usize {
        self.next as usize
    }
}

impl Drop for LogWriter {
    /// A best-effort flush, so a writer that goes out of scope without an
    /// explicit flush does not silently drop its tail.
    ///
    /// Best-effort is the honest word: a failure here cannot be reported, which
    /// is exactly why [`sync`] exists and why a session that cares calls it.
    ///
    /// [`sync`]: LogWriter::sync
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

/// Reads a recorded session back.
#[derive(Debug)]
pub struct LogReader {
    file: File,
    header: LogHeader,
    /// Records the file has room for, ignoring any partial tail.
    whole_records: u64,
    /// Bytes of an incomplete final record, if the file was torn.
    partial_bytes: u64,
    /// The sequence number this file's first record carries.
    ///
    /// Zero for a session that never crashed, and something else for a
    /// segment continuing one that did (`Segments`). Read from the first
    /// record rather than from a header field, which is what lets a chain
    /// work without a format change (recovery contract, D-1).
    ///
    /// `None` when the file holds no whole record, where there is no honest
    /// answer and nothing that needs one.
    base: Option<Seq>,
}

impl LogReader {
    /// Opens a log and reads its header.
    pub fn open(path: impl AsRef<Path>) -> Result<LogReader, LogFileError> {
        let mut file = File::open(path.as_ref())?;
        let file_len = file.metadata()?.len();

        // Read a probe big enough for almost any header, and let the decoder
        // say if it needs more. A header grows with the instrument table, so
        // its size is not known before it is read.
        let mut probe = vec![0u8; file_len.min(4_096) as usize];
        file.read_exact(&mut probe)?;
        let header = match LogHeader::decode(&probe) {
            Ok(header) => header,
            Err(CodecError::ShortBuffer { needed, .. }) if needed as u64 <= file_len => {
                let mut full = vec![0u8; needed];
                file.seek(SeekFrom::Start(0))?;
                file.read_exact(&mut full)?;
                LogHeader::decode(&full)?
            }
            Err(e) => return Err(LogFileError::Codec(e)),
        };

        let header_len = header.encoded_len() as u64;
        let body = file_len - header_len;
        let whole_records = body / RECORD_LEN as u64;

        // Where this file's sequence starts. One record read at open, off any
        // hot path, and it is what lets every later offset be arithmetic.
        // A first record that will not decode leaves the base unknown; the
        // damage is then reported by `read_all` rather than guessed at here.
        let base = if whole_records > 0 {
            let mut first = [0u8; RECORD_LEN];
            file.seek(SeekFrom::Start(header.encoded_len() as u64))?;
            file.read_exact(&mut first)?;
            decode_record(&first).ok().map(|record| record.seq)
        } else {
            None
        };

        Ok(LogReader {
            file,
            header,
            whole_records,
            partial_bytes: body % RECORD_LEN as u64,
            base,
        })
    }

    /// What the session was recorded under.
    #[inline]
    pub fn header(&self) -> &LogHeader {
        &self.header
    }

    /// How many whole records the file holds, before any are validated.
    #[inline]
    pub fn record_capacity(&self) -> u64 {
        self.whole_records
    }

    /// Reads one record by sequence number.
    ///
    /// A seek and one read: the whole reason the format uses a fixed record
    /// size (ADR, D-2).
    pub fn read_at(&mut self, seq: Seq) -> Result<Record, LogFileError> {
        let base = self.base.map(Seq::raw).unwrap_or(0);
        let Some(index) = seq
            .raw()
            .checked_sub(base)
            .filter(|i| *i < self.whole_records)
        else {
            return Err(LogFileError::NoSuchRecord {
                seq,
                available: self.whole_records,
            });
        };
        let mut bytes = [0u8; RECORD_LEN];
        self.file
            .seek(SeekFrom::Start(self.header.offset_of(index)))?;
        self.file.read_exact(&mut bytes)?;
        Ok(decode_record(&bytes)?)
    }

    /// Reads every record, stopping at the first one that cannot be trusted.
    ///
    /// Returns what was salvaged alongside a [`Recovery`] describing what was
    /// not. The caller must look at it — a short log that says nothing about
    /// being short is the failure this whole module is shaped to avoid.
    pub fn read_all(&mut self) -> Result<(Vec<Record>, Recovery), LogFileError> {
        self.file
            .seek(SeekFrom::Start(self.header.encoded_len() as u64))?;

        let mut records = Vec::with_capacity(self.whole_records as usize);
        let mut bytes = [0u8; RECORD_LEN];
        let mut stopped = None;

        // A segment continuing a crashed session starts part-way through the
        // sequence, so what is checked is that records are contiguous from
        // wherever this file begins — not that they begin at zero.
        let base = self.base.map(Seq::raw).unwrap_or(0);
        for index in 0..self.whole_records {
            self.file.read_exact(&mut bytes)?;
            match decode_record(&bytes) {
                Ok(record) => {
                    let expected = Seq::new(base + index);
                    if record.seq != expected {
                        stopped = Some(StopReason::SequenceGap {
                            expected,
                            found: record.seq,
                        });
                        break;
                    }
                    records.push(record);
                }
                Err(error) => {
                    stopped = Some(StopReason::Corrupt { index, error });
                    break;
                }
            }
        }

        if stopped.is_none() && self.partial_bytes > 0 {
            stopped = Some(StopReason::TornTail {
                partial_bytes: self.partial_bytes,
            });
        }

        let read = records.len() as u64;
        let discarded = (self.whole_records - read) * RECORD_LEN as u64 + self.partial_bytes;
        Ok((
            records,
            Recovery {
                records: read,
                discarded_bytes: discarded,
                stopped,
            },
        ))
    }

    /// Reads every record, failing if anything had to be dropped.
    ///
    /// What most callers want: a log, or a refusal. Use [`read_all`] when the
    /// point is to salvage a damaged file.
    ///
    /// [`read_all`]: LogReader::read_all
    pub fn read_all_intact(&mut self) -> Result<Vec<Record>, LogFileError> {
        let (records, recovery) = self.read_all()?;
        if recovery.is_clean() {
            Ok(records)
        } else {
            Err(LogFileError::Damaged(recovery))
        }
    }
}
