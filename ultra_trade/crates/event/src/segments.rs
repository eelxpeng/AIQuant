//! A session recorded across one or more files.
//!
//! A crash damages whichever file was open at the time. The recovery contract
//! (`docs/adr/1-crash-recovery-contract.md`, D-1) says that file is left
//! exactly as the crash left it, and the session continues in a new one whose
//! first record carries the next sequence number. **The chain is the session**;
//! no single file is.
//!
//! # Why the first segment is numbered
//!
//! A session at `session.log` is stored as `session.0.log`, `session.1.log`,
//! and so on. Nothing is ever written to the bare path.
//!
//! The alternative — the first segment being the bare `session.log` and only
//! later ones numbered — costs less churn and was rejected anyway. Under it, a
//! tool that opens `session.log` and forgets to look for `session.1.log` reads
//! part of a session and reports it as the whole thing. That is the failure
//! this codebase least wants: a short read is a conclusion someone acts on.
//! With every segment numbered, the same tool opens a path that does not exist
//! and fails loudly instead.
//!
//! # What this does not do
//!
//! Deciding *whether it is safe to trade* after a restart is the rest of the
//! contract (D-2 through D-4) and is not here. This is the format capability
//! underneath it: continue a session's records without touching its evidence.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::codec::LogHeader;
use crate::file::{LogFileError, LogReader, LogWriter, Recovery, StopReason};
use crate::log::{Record, Seq};

/// The segments of one session.
///
/// Not a value — every method takes the session root, because a chain lives on
/// disk and holding a stale list of it in memory is how a resumed session
/// misses the segment another process just wrote.
#[derive(Debug)]
pub struct Segments;

impl Segments {
    /// The path of segment `index` for a session rooted at `root`.
    ///
    /// `/data/session.log` and `2` give `/data/session.2.log`. A root with no
    /// extension gives `/data/session.2`.
    pub fn path(root: &Path, index: u32) -> PathBuf {
        let stem = root
            .file_stem()
            .map(OsStr::to_os_string)
            .unwrap_or_default();
        let mut name = stem;
        name.push(format!(".{index}"));
        if let Some(extension) = root.extension() {
            name.push(".");
            name.push(extension);
        }
        root.with_file_name(name)
    }

    /// Every segment of the session, oldest first.
    ///
    /// Empty when the session does not exist. A **gap** in the numbering is an
    /// error rather than a stopping point: segments 0 and 2 with no 1 means a
    /// file was lost, and reading 0 alone would silently report a third of a
    /// session as all of it.
    pub fn paths(root: &Path) -> Result<Vec<PathBuf>, LogFileError> {
        let mut found = Vec::new();
        for index in 0.. {
            let path = Segments::path(root, index);
            if path.exists() {
                found.push(path);
            } else {
                // One miss is the end of the chain — unless a later segment
                // exists, which means the miss is a hole.
                if let Some(hole) = Segments::first_beyond(root, index)? {
                    return Err(LogFileError::MissingSegment {
                        missing: index,
                        found: hole,
                    });
                }
                break;
            }
        }
        Ok(found)
    }

    /// Looks a little past `index` for a segment that should not be there.
    ///
    /// Bounded rather than unbounded: a chain with a hole is already broken,
    /// and the answer to "how far past the hole do we look" only changes the
    /// error message, never whether it is an error.
    fn first_beyond(root: &Path, index: u32) -> Result<Option<u32>, LogFileError> {
        for ahead in index + 1..index + 16 {
            if Segments::path(root, ahead).exists() {
                return Ok(Some(ahead));
            }
        }
        Ok(None)
    }

    /// The header the session was recorded under.
    pub fn header(root: &Path) -> Result<LogHeader, LogFileError> {
        let paths = Segments::paths(root)?;
        let first = paths.first().ok_or_else(|| {
            LogFileError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("no session at {}", root.display()),
            ))
        })?;
        Ok(LogReader::open(first)?.header().clone())
    }

    /// Where a fresh session's first segment goes.
    ///
    /// Refuses if any segment already exists. A recorded session is evidence,
    /// and appending to one by accident is not a thing this can do.
    ///
    /// Separate from [`create`] because the background writer opens its own
    /// file rather than going through a [`LogWriter`], and it must land on the
    /// same path under the same refusal.
    ///
    /// [`create`]: Segments::create
    pub fn create_path(root: &Path) -> Result<PathBuf, LogFileError> {
        if !Segments::paths(root)?.is_empty() {
            return Err(LogFileError::AlreadyExists);
        }
        Ok(Segments::path(root, 0))
    }

    /// Creates a fresh session and returns a writer for its first segment.
    pub fn create(root: &Path, header: LogHeader) -> Result<LogWriter, LogFileError> {
        LogWriter::create_at(Segments::create_path(root)?, header, Seq::FIRST)
    }

    /// Opens the next segment of an existing session, continuing its sequence.
    ///
    /// The damaged segment is read, never written: whatever survived in it
    /// fixes where this one starts. A tail the crash tore off is *dropped*, so
    /// the new segment reuses the sequence number that record would have had —
    /// a record that is not durable never happened, and leaving a hole for it
    /// would put a gap in the log's only ordering key.
    pub fn resume(root: &Path) -> Result<LogWriter, LogFileError> {
        let paths = Segments::paths(root)?;
        let last_index = match paths.len() {
            0 => {
                return Err(LogFileError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no session at {} to resume", root.display()),
                )));
            }
            n => (n - 1) as u32,
        };

        let (records, _) = Segments::read_all(root)?;
        let next = records
            .last()
            .map(|r| Seq::new(r.seq.raw() + 1))
            .unwrap_or(Seq::FIRST);
        let header = Segments::header(root)?;
        LogWriter::create_at(Segments::path(root, last_index + 1), header, next)
    }

    /// Reads every segment as one record stream.
    ///
    /// Stops at the first damage and says so. A hole in a middle segment is
    /// worse than a short tail — every record after it has an unexplained gap
    /// in front of it — so nothing beyond the damage is returned even though
    /// it is sitting right there on disk.
    pub fn read_all(root: &Path) -> Result<(Vec<Record>, Recovery), LogFileError> {
        let paths = Segments::paths(root)?;
        let mut all = Vec::new();
        let mut recovery = Recovery {
            records: 0,
            discarded_bytes: 0,
            stopped: None,
        };

        let last = paths.len().saturating_sub(1);
        for (index, path) in paths.iter().enumerate() {
            let mut reader = LogReader::open(path)?;
            let (records, segment) = reader.read_all()?;

            if let Some(first) = records.first()
                && let Some(previous) = all.last().map(|r: &Record| r.seq)
                && first.seq.raw() != previous.raw() + 1
            {
                // The chain does not join up. Reporting the numbers beats
                // concatenating them and letting a replay diverge somewhere
                // downstream for a reason nobody can trace back to here.
                recovery.stopped = Some(StopReason::SequenceGap {
                    expected: Seq::new(previous.raw() + 1),
                    found: first.seq,
                });
                recovery.records = all.len() as u64;
                return Ok((all, recovery));
            }

            all.extend(records);
            recovery.records = all.len() as u64;
            recovery.discarded_bytes += segment.discarded_bytes;

            match segment.stopped {
                // A torn tail in a segment that has a successor is the
                // ordinary shape of a recovered session, not damage to report
                // and stop on: the crash tore this file, and the next segment
                // was opened to continue from what survived. Whether it really
                // does continue is decided by the contiguity check at the top
                // of the next iteration — which is a stronger check than
                // trusting the tear, because it compares actual numbers.
                //
                // Stopping here instead would make every recovered session
                // unreadable past its first crash, which is the one thing the
                // chain exists to prevent.
                Some(StopReason::TornTail { .. }) if index < last => {}
                Some(reason) => {
                    recovery.stopped = Some(reason);
                    return Ok((all, recovery));
                }
                None => {}
            }
        }

        Ok((all, recovery))
    }
}
