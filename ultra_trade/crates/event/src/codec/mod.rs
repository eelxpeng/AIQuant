//! The on-disk format for a recorded session.
//!
//! Decided in `docs/adr/1-recorded-log-format.md`, which this implements. The
//! shape in one paragraph: a header carrying the instrument table the session
//! ran under, followed by fixed-size 80-byte records. Record *n* lives at
//! `header_len + n * 80`, so seeking to a sequence number is arithmetic and a
//! torn tail is visible in the file length alone.
//!
//! Three properties are worth stating because code elsewhere depends on them:
//!
//! - **Canonical.** The same record always encodes to the same bytes, so two
//!   logs of the same session compare byte for byte. That is what lets D0.2's
//!   "replays byte-identically" be taken literally.
//! - **Zero decodes as nothing.** No enum's wire value is 0, so a wiped or
//!   truncated record fails loudly rather than reading as the first variant of
//!   everything.
//! - **Unknown means stop.** An unrecognised record kind, flag bit, or enum
//!   value is an error, never something to skip. A reader that guessed would
//!   silently drop what a newer writer said.
//!
//! This module does no I/O. It turns records into bytes and back; opening files
//! is the writer's and reader's business.

mod crc32;
mod header;
mod record;
mod wire;

pub use header::{InstrumentEntry, LogHeader, SYMBOL_LEN};
pub use record::{decode_record, encode_record};

use core::fmt;
use types::InstrumentId;

/// Identifies the file type. Eight bytes, so a mistyped path fails on the
/// first read rather than somewhere deep in a decode.
pub const MAGIC: &[u8; 8] = b"ULTRALOG";

/// Bytes per record.
///
/// 80 rather than the 64 that would be tidier: a fill's fee is a `Notional`,
/// which is `i128`, so the widest payload needs 56 bytes against 16 bytes of
/// framing. The remaining 8 are reserved and must be zero, which buys one more
/// `i64` field before a version bump.
pub const RECORD_LEN: usize = 80;

/// What the codec refused.
///
/// Every arm is a refusal to guess. A format that recovered from these by
/// filling in a default would produce a log that reads cleanly and says
/// something other than what was recorded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CodecError {
    /// The buffer ran out before the fields did.
    ShortBuffer {
        /// Bytes the decode needed.
        needed: usize,
        /// Bytes it was given.
        found: usize,
    },
    /// The record's checksum does not match its contents.
    BadChecksum {
        /// What the contents hash to.
        expected: u32,
        /// What the record claims.
        found: u32,
    },
    /// A reserved field was not zero, so a writer put something there that this
    /// reader does not know about.
    ReservedNotZero,
    /// The record kind byte is not one this build knows.
    UnknownKind(u8),
    /// A flag bit is set that this build does not know.
    UnknownFlags(u8),
    /// A field's value is not a member of its enum.
    UnknownEnumValue {
        /// Which field.
        field: &'static str,
        /// The value found.
        value: u8,
    },
    /// The file does not start with [`MAGIC`].
    BadMagic,
    /// The file was written by a newer format than this build understands.
    UnsupportedVersion(u16),
    /// The header declares a record size this build cannot read.
    UnexpectedRecordLen(u16),
    /// The header's declared length does not match its instrument count.
    HeaderLengthMismatch {
        /// What the header says.
        declared: usize,
        /// What its instrument count implies.
        computed: usize,
    },
    /// The header claims more instruments than a header can hold.
    TooManyInstruments {
        /// What the header claims.
        count: usize,
        /// The most that is accepted.
        max: usize,
    },
    /// A symbol does not fit the fixed-width field.
    SymbolTooLong {
        /// The symbol's length.
        len: usize,
        /// The most that fits.
        max: usize,
    },
    /// The recorded session and the configured one disagree on how many
    /// instruments there are.
    InstrumentCountMismatch {
        /// What the file says.
        recorded: usize,
        /// What the session is configured with.
        configured: usize,
    },
    /// The recorded session and the configured one disagree about an
    /// instrument's conventions.
    InstrumentMismatch {
        /// Which instrument.
        id: InstrumentId,
    },
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodecError::ShortBuffer { needed, found } => {
                write!(f, "buffer holds {found} bytes, needed {needed}")
            }
            CodecError::BadChecksum { expected, found } => {
                write!(f, "checksum {found:#010x} does not match {expected:#010x}")
            }
            CodecError::ReservedNotZero => f.write_str("a reserved field was not zero"),
            CodecError::UnknownKind(kind) => write!(f, "unknown record kind {kind}"),
            CodecError::UnknownFlags(flags) => write!(f, "unknown flag bits {flags:#010b}"),
            CodecError::UnknownEnumValue { field, value } => {
                write!(f, "{value} is not a valid {field}")
            }
            CodecError::BadMagic => f.write_str("not an ultra_trade log"),
            CodecError::UnsupportedVersion(v) => {
                write!(f, "format version {v} is newer than this build")
            }
            CodecError::UnexpectedRecordLen(len) => {
                write!(f, "record length {len} is not supported")
            }
            CodecError::HeaderLengthMismatch { declared, computed } => {
                write!(
                    f,
                    "header declares {declared} bytes, its contents imply {computed}"
                )
            }
            CodecError::TooManyInstruments { count, max } => {
                write!(f, "{count} instruments exceeds the maximum of {max}")
            }
            CodecError::SymbolTooLong { len, max } => {
                write!(f, "symbol of {len} bytes exceeds the maximum of {max}")
            }
            CodecError::InstrumentCountMismatch {
                recorded,
                configured,
            } => write!(
                f,
                "the log records {recorded} instruments, the session is configured with {configured}"
            ),
            CodecError::InstrumentMismatch { id } => {
                write!(f, "instrument {id} does not match the recorded conventions")
            }
        }
    }
}

impl core::error::Error for CodecError {}
