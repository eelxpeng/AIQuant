//! The file header: what a recorded session was recorded under.

use super::wire::{Reader, Writer};
use super::{CodecError, MAGIC, RECORD_LEN};
use crate::FORMAT_VERSION;
use types::{ExchangeTime, Instrument, InstrumentId, OrderId, Px, Qty, Timestamp};

/// Bytes before the first instrument entry.
const FIXED_LEN: usize = 48;
/// Bytes per instrument entry.
const ENTRY_LEN: usize = 48;
/// A symbol is fixed width and NUL-padded, so entries stay a constant size.
pub const SYMBOL_LEN: usize = 16;
/// More than this and the header stops being a header.
const MAX_INSTRUMENTS: usize = 4_096;

/// One instrument, as the session was configured with it.
///
/// Carries a symbol, which [`Instrument`] does not. The symbol is a property of
/// the session's *configuration* rather than of the in-memory value, and
/// without it a log that says `InstrumentId(3)` is not interpretable by anyone
/// who no longer has the config that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstrumentEntry {
    /// Which instrument.
    pub id: InstrumentId,
    /// NUL-padded venue symbol.
    pub symbol: [u8; SYMBOL_LEN],
    /// The venue's price increment.
    pub tick: Px,
    /// The venue's quantity increment.
    pub lot: Qty,
    /// The smallest quantity the venue accepts.
    pub min_qty: Qty,
}

impl InstrumentEntry {
    /// Builds an entry from an instrument and a symbol.
    pub fn new(instrument: Instrument, symbol: &str) -> Result<InstrumentEntry, CodecError> {
        let bytes = symbol.as_bytes();
        if bytes.len() > SYMBOL_LEN {
            return Err(CodecError::SymbolTooLong {
                len: bytes.len(),
                max: SYMBOL_LEN,
            });
        }
        let mut padded = [0u8; SYMBOL_LEN];
        padded[..bytes.len()].copy_from_slice(bytes);
        Ok(InstrumentEntry {
            id: instrument.id(),
            symbol: padded,
            tick: instrument.tick(),
            lot: instrument.lot(),
            min_qty: instrument.min_qty(),
        })
    }

    /// The symbol with its padding removed.
    ///
    /// `None` if the recorded bytes are not UTF-8, which is a corrupt header
    /// rather than an odd symbol.
    pub fn symbol_str(&self) -> Option<&str> {
        let end = self
            .symbol
            .iter()
            .position(|b| *b == 0)
            .unwrap_or(SYMBOL_LEN);
        core::str::from_utf8(&self.symbol[..end]).ok()
    }

    /// Whether this entry describes the same contract as `instrument`.
    ///
    /// The symbol is not compared, because [`Instrument`] does not carry one.
    fn describes(&self, instrument: Instrument) -> bool {
        self.id == instrument.id()
            && self.tick == instrument.tick()
            && self.lot == instrument.lot()
            && self.min_qty == instrument.min_qty()
    }
}

/// What a recorded session was recorded under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogHeader {
    /// The format the file was written in.
    pub format_version: u16,
    /// Which session this is. Caller-supplied — never from a clock or a random
    /// source, because nothing about a recorded session may be ambient.
    pub session_id: u64,
    /// The exchange time the session started at.
    pub session_start: ExchangeTime,
    /// The id the session's first order received.
    pub first_order_id: OrderId,
    /// Every instrument the session could trade, in id order.
    pub instruments: Vec<InstrumentEntry>,
}

impl LogHeader {
    /// A header for a session about to be recorded.
    pub fn new(
        session_id: u64,
        session_start: ExchangeTime,
        first_order_id: OrderId,
        instruments: Vec<InstrumentEntry>,
    ) -> LogHeader {
        LogHeader {
            format_version: FORMAT_VERSION,
            session_id,
            session_start,
            first_order_id,
            instruments,
        }
    }

    /// How many bytes this header occupies, and therefore where record 0 sits.
    pub fn encoded_len(&self) -> usize {
        FIXED_LEN + self.instruments.len() * ENTRY_LEN
    }

    /// The byte offset of the record at `index`.
    ///
    /// The whole point of a fixed record size: seeking is arithmetic rather
    /// than an index lookup (ADR, D-2).
    pub fn offset_of(&self, index: u64) -> u64 {
        self.encoded_len() as u64 + index * RECORD_LEN as u64
    }

    /// Appends the encoded header to `out`.
    pub fn encode(&self, out: &mut Vec<u8>) {
        let len = self.encoded_len();
        let start = out.len();
        out.resize(start + len, 0);
        let buf = &mut out[start..];

        let mut w = Writer::new(&mut buf[..FIXED_LEN]);
        w.bytes(MAGIC);
        w.u16(self.format_version);
        w.u16(RECORD_LEN as u16);
        w.u32(len as u32);
        w.u64(self.session_id);
        w.i64(self.session_start.to_nanos());
        w.u64(self.first_order_id.raw());
        w.u32(self.instruments.len() as u32);
        // The remaining four bytes stay zero: reserved.
        debug_assert!(w.position() <= FIXED_LEN);

        for (index, entry) in self.instruments.iter().enumerate() {
            let at = FIXED_LEN + index * ENTRY_LEN;
            let mut e = Writer::new(&mut buf[at..at + ENTRY_LEN]);
            e.u32(entry.id.raw());
            e.bytes(&entry.symbol);
            e.i64(entry.tick.to_scaled());
            e.i64(entry.lot.to_scaled());
            e.i64(entry.min_qty.to_scaled());
            debug_assert!(e.position() <= ENTRY_LEN);
        }
    }

    /// Reads a header from the start of `bytes`.
    pub fn decode(bytes: &[u8]) -> Result<LogHeader, CodecError> {
        if bytes.len() < FIXED_LEN {
            return Err(CodecError::ShortBuffer {
                needed: FIXED_LEN,
                found: bytes.len(),
            });
        }
        let mut r = Reader::new(&bytes[..FIXED_LEN]);
        if r.bytes(MAGIC.len())? != MAGIC {
            return Err(CodecError::BadMagic);
        }
        let format_version = r.u16()?;
        if format_version > FORMAT_VERSION {
            return Err(CodecError::UnsupportedVersion(format_version));
        }
        let record_len = r.u16()?;
        if record_len as usize != RECORD_LEN {
            return Err(CodecError::UnexpectedRecordLen(record_len));
        }
        let header_len = r.u32()? as usize;
        let session_id = r.u64()?;
        let session_start = Timestamp::from_nanos(r.i64()?);
        let first_order_id = OrderId::new(r.u64()?);
        let count = r.u32()? as usize;
        if count > MAX_INSTRUMENTS {
            return Err(CodecError::TooManyInstruments {
                count,
                max: MAX_INSTRUMENTS,
            });
        }
        if bytes[r.position()..FIXED_LEN].iter().any(|b| *b != 0) {
            return Err(CodecError::ReservedNotZero);
        }

        let expected_len = FIXED_LEN + count * ENTRY_LEN;
        if header_len != expected_len {
            return Err(CodecError::HeaderLengthMismatch {
                declared: header_len,
                computed: expected_len,
            });
        }
        if bytes.len() < expected_len {
            return Err(CodecError::ShortBuffer {
                needed: expected_len,
                found: bytes.len(),
            });
        }

        let mut instruments = Vec::with_capacity(count);
        for index in 0..count {
            let at = FIXED_LEN + index * ENTRY_LEN;
            let mut e = Reader::new(&bytes[at..at + ENTRY_LEN]);
            let id = InstrumentId::new(e.u32()?);
            let mut symbol = [0u8; SYMBOL_LEN];
            symbol.copy_from_slice(e.bytes(SYMBOL_LEN)?);
            let tick = Px::from_scaled(e.i64()?);
            let lot = Qty::from_scaled(e.i64()?);
            let min_qty = Qty::from_scaled(e.i64()?);
            if bytes[at + e.position()..at + ENTRY_LEN]
                .iter()
                .any(|b| *b != 0)
            {
                return Err(CodecError::ReservedNotZero);
            }
            instruments.push(InstrumentEntry {
                id,
                symbol,
                tick,
                lot,
                min_qty,
            });
        }

        Ok(LogHeader {
            format_version,
            session_id,
            session_start,
            first_order_id,
            instruments,
        })
    }

    /// Checks this header against the instruments a session is configured with.
    ///
    /// **A reader must call this before replaying a recorded session.** Without
    /// it, `InstrumentId(3)` in the file silently means whatever instrument 3
    /// happens to be in the current configuration, and a backtest reads one
    /// contract's prices as another's — wrong in a way no test downstream would
    /// catch (ADR, D-3).
    pub fn check_against(&self, instruments: &[Instrument]) -> Result<(), CodecError> {
        if self.instruments.len() != instruments.len() {
            return Err(CodecError::InstrumentCountMismatch {
                recorded: self.instruments.len(),
                configured: instruments.len(),
            });
        }
        for (entry, instrument) in self.instruments.iter().zip(instruments) {
            if !entry.describes(*instrument) {
                return Err(CodecError::InstrumentMismatch {
                    id: instrument.id(),
                });
            }
        }
        Ok(())
    }
}
