//! Wire values, and the cursors that read and write them.
//!
//! # Discriminants are assigned by hand
//!
//! Never derived from a Rust enum's declaration order. Reordering a variant
//! must not change what a recorded byte means, and a retired number is never
//! given to something else (ADR, recorded log format, D-4).
//!
//! # No wire value is zero
//!
//! Every mapping below starts at 1. Zeroed bytes therefore never decode as a
//! valid value, so a truncated or wiped record fails loudly instead of reading
//! as a well-formed one with the first variant of everything.
//!
//! Each `to_wire` is an exhaustive `match`, so adding a variant anywhere in the
//! event alphabet fails to compile here until its wire value is chosen. That is
//! the intended way to find out that a durable format needs updating.

use super::CodecError;
use crate::{Command, EngineState, RejectReason, RiskReason, StateReason};
use types::Side;

// ---- record kinds --------------------------------------------------------

/// Inbound kinds occupy 1..=63; outbound kinds start at 64. The split is
/// cosmetic, but it makes a hex dump readable at a glance.
pub(super) const QUOTE: u8 = 1;
pub(super) const TRADE: u8 = 2;
pub(super) const VENUE_ACCEPTED: u8 = 3;
pub(super) const VENUE_REJECTED: u8 = 4;
pub(super) const VENUE_FILLED: u8 = 5;
pub(super) const VENUE_CANCELLED: u8 = 6;
pub(super) const VENUE_CANCEL_REJECTED: u8 = 7;
pub(super) const VENUE_EXPIRED: u8 = 8;
pub(super) const VENUE_POSITION: u8 = 9;
pub(super) const TIMER: u8 = 10;
pub(super) const COMMAND: u8 = 11;
/// A single book level (format 2).
pub(super) const BOOK_LEVEL: u8 = 12;
/// The end of a book update (format 2).
pub(super) const BOOK_APPLIED: u8 = 13;
/// Discard the book; a snapshot follows (format 3).
pub(super) const BOOK_RESET: u8 = 14;

pub(super) const ORDER_SUBMITTED: u8 = 64;
pub(super) const CANCEL_SUBMITTED: u8 = 65;
pub(super) const INTENT_REJECTED: u8 = 66;
pub(super) const TIMER_REQUESTED: u8 = 67;
pub(super) const STATE_CHANGED: u8 = 68;

/// Bit 0 of the record's flag byte.
pub(super) const FLAG_REDUCE_ONLY: u8 = 0b0000_0001;

/// Market or limit, as a payload tag.
pub(super) const ORDER_KIND_MARKET: u8 = 1;
pub(super) const ORDER_KIND_LIMIT: u8 = 2;

// ---- enum mappings -------------------------------------------------------

pub(super) const fn side_to_wire(side: Side) -> u8 {
    match side {
        Side::Buy => 1,
        Side::Sell => 2,
    }
}

pub(super) fn side_from_wire(value: u8) -> Result<Side, CodecError> {
    match value {
        1 => Ok(Side::Buy),
        2 => Ok(Side::Sell),
        _ => Err(CodecError::UnknownEnumValue {
            field: "side",
            value,
        }),
    }
}

pub(super) const fn command_to_wire(command: Command) -> u8 {
    match command {
        Command::Halt => 1,
        Command::Resume => 2,
        Command::Kill => 3,
        Command::Flatten => 4,
    }
}

pub(super) fn command_from_wire(value: u8) -> Result<Command, CodecError> {
    match value {
        1 => Ok(Command::Halt),
        2 => Ok(Command::Resume),
        3 => Ok(Command::Kill),
        4 => Ok(Command::Flatten),
        _ => Err(CodecError::UnknownEnumValue {
            field: "command",
            value,
        }),
    }
}

pub(super) const fn engine_state_to_wire(state: EngineState) -> u8 {
    match state {
        EngineState::Running => 1,
        EngineState::Halted => 2,
        EngineState::Killed => 3,
    }
}

pub(super) fn engine_state_from_wire(value: u8) -> Result<EngineState, CodecError> {
    match value {
        1 => Ok(EngineState::Running),
        2 => Ok(EngineState::Halted),
        3 => Ok(EngineState::Killed),
        _ => Err(CodecError::UnknownEnumValue {
            field: "engine state",
            value,
        }),
    }
}

pub(super) const fn state_reason_to_wire(reason: StateReason) -> u8 {
    match reason {
        StateReason::OperatorCommand => 1,
        StateReason::ReconciliationDivergence => 2,
        StateReason::LogWriteFailed => 3,
    }
}

pub(super) fn state_reason_from_wire(value: u8) -> Result<StateReason, CodecError> {
    match value {
        1 => Ok(StateReason::OperatorCommand),
        2 => Ok(StateReason::ReconciliationDivergence),
        3 => Ok(StateReason::LogWriteFailed),
        _ => Err(CodecError::UnknownEnumValue {
            field: "state reason",
            value,
        }),
    }
}

pub(super) const fn reject_reason_to_wire(reason: RejectReason) -> u8 {
    match reason {
        RejectReason::UnknownInstrument => 1,
        RejectReason::InvalidPrice => 2,
        RejectReason::InvalidQuantity => 3,
        RejectReason::InsufficientFunds => 4,
        RejectReason::RateLimited => 5,
        RejectReason::UnknownOrder => 6,
        RejectReason::AlreadyTerminal => 7,
        RejectReason::Unavailable => 8,
    }
}

pub(super) fn reject_reason_from_wire(value: u8) -> Result<RejectReason, CodecError> {
    match value {
        1 => Ok(RejectReason::UnknownInstrument),
        2 => Ok(RejectReason::InvalidPrice),
        3 => Ok(RejectReason::InvalidQuantity),
        4 => Ok(RejectReason::InsufficientFunds),
        5 => Ok(RejectReason::RateLimited),
        6 => Ok(RejectReason::UnknownOrder),
        7 => Ok(RejectReason::AlreadyTerminal),
        8 => Ok(RejectReason::Unavailable),
        _ => Err(CodecError::UnknownEnumValue {
            field: "reject reason",
            value,
        }),
    }
}

pub(super) const fn risk_reason_to_wire(reason: RiskReason) -> u8 {
    match reason {
        RiskReason::KillSwitchEngaged => 1,
        RiskReason::Halted => 2,
        RiskReason::UnknownInstrument => 3,
        RiskReason::NoLimitsConfigured => 4,
        RiskReason::StaleMarketData => 5,
        RiskReason::NoMarketData => 6,
        RiskReason::PositionLimit => 7,
        RiskReason::ExposureLimit => 8,
        RiskReason::OrderNotionalLimit => 9,
        RiskReason::OrderRateLimit => 10,
        RiskReason::UnreconciledPosition => 11,
        RiskReason::BelowMinimumQty => 12,
        RiskReason::NonPositiveQuantity => 13,
        RiskReason::NonPositivePrice => 14,
        RiskReason::NotReducing => 15,
        RiskReason::Unrepresentable => 16,
        RiskReason::VenueUnreachable => 17,
    }
}

pub(super) fn risk_reason_from_wire(value: u8) -> Result<RiskReason, CodecError> {
    match value {
        1 => Ok(RiskReason::KillSwitchEngaged),
        2 => Ok(RiskReason::Halted),
        3 => Ok(RiskReason::UnknownInstrument),
        4 => Ok(RiskReason::NoLimitsConfigured),
        5 => Ok(RiskReason::StaleMarketData),
        6 => Ok(RiskReason::NoMarketData),
        7 => Ok(RiskReason::PositionLimit),
        8 => Ok(RiskReason::ExposureLimit),
        9 => Ok(RiskReason::OrderNotionalLimit),
        10 => Ok(RiskReason::OrderRateLimit),
        11 => Ok(RiskReason::UnreconciledPosition),
        12 => Ok(RiskReason::BelowMinimumQty),
        13 => Ok(RiskReason::NonPositiveQuantity),
        14 => Ok(RiskReason::NonPositivePrice),
        15 => Ok(RiskReason::NotReducing),
        16 => Ok(RiskReason::Unrepresentable),
        17 => Ok(RiskReason::VenueUnreachable),
        _ => Err(CodecError::UnknownEnumValue {
            field: "risk reason",
            value,
        }),
    }
}

// ---- little-endian cursors ----------------------------------------------

/// Writes fields little-endian into a fixed buffer.
///
/// Field by field, never a reinterpreted struct: the crate forbids `unsafe`,
/// and a native-endian dump would not be portable between machines anyway
/// (ADR, D-5).
pub(super) struct Writer<'a> {
    buf: &'a mut [u8],
    at: usize,
}

impl<'a> Writer<'a> {
    pub(super) fn new(buf: &'a mut [u8]) -> Writer<'a> {
        Writer { buf, at: 0 }
    }

    pub(super) fn position(&self) -> usize {
        self.at
    }

    fn put(&mut self, bytes: &[u8]) {
        self.buf[self.at..self.at + bytes.len()].copy_from_slice(bytes);
        self.at += bytes.len();
    }

    pub(super) fn u8(&mut self, v: u8) {
        self.put(&[v]);
    }
    pub(super) fn u16(&mut self, v: u16) {
        self.put(&v.to_le_bytes());
    }
    pub(super) fn u32(&mut self, v: u32) {
        self.put(&v.to_le_bytes());
    }
    pub(super) fn u64(&mut self, v: u64) {
        self.put(&v.to_le_bytes());
    }
    pub(super) fn i64(&mut self, v: i64) {
        self.put(&v.to_le_bytes());
    }
    pub(super) fn i128(&mut self, v: i128) {
        self.put(&v.to_le_bytes());
    }
    pub(super) fn bytes(&mut self, v: &[u8]) {
        self.put(v);
    }
}

/// Reads fields little-endian from a fixed buffer.
pub(super) struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, at: 0 }
    }

    pub(super) fn position(&self) -> usize {
        self.at
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        let end = self.at + len;
        if end > self.buf.len() {
            return Err(CodecError::ShortBuffer {
                needed: end,
                found: self.buf.len(),
            });
        }
        let slice = &self.buf[self.at..end];
        self.at = end;
        Ok(slice)
    }

    pub(super) fn u8(&mut self) -> Result<u8, CodecError> {
        Ok(self.take(1)?[0])
    }
    pub(super) fn u16(&mut self) -> Result<u16, CodecError> {
        Ok(u16::from_le_bytes(
            self.take(2)?.try_into().expect("2 bytes"),
        ))
    }
    pub(super) fn u32(&mut self) -> Result<u32, CodecError> {
        Ok(u32::from_le_bytes(
            self.take(4)?.try_into().expect("4 bytes"),
        ))
    }
    pub(super) fn u64(&mut self) -> Result<u64, CodecError> {
        Ok(u64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    pub(super) fn i64(&mut self) -> Result<i64, CodecError> {
        Ok(i64::from_le_bytes(
            self.take(8)?.try_into().expect("8 bytes"),
        ))
    }
    pub(super) fn i128(&mut self) -> Result<i128, CodecError> {
        Ok(i128::from_le_bytes(
            self.take(16)?.try_into().expect("16 bytes"),
        ))
    }
    pub(super) fn bytes(&mut self, len: usize) -> Result<&'a [u8], CodecError> {
        self.take(len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_wire_value_is_zero() {
        // Zeroed bytes must never decode as a well-formed value, or a wiped
        // record would read as the first variant of everything.
        assert!(side_from_wire(0).is_err());
        assert!(command_from_wire(0).is_err());
        assert!(engine_state_from_wire(0).is_err());
        assert!(state_reason_from_wire(0).is_err());
        assert!(reject_reason_from_wire(0).is_err());
        assert!(risk_reason_from_wire(0).is_err());
    }

    #[test]
    fn every_enum_round_trips_through_its_wire_value() {
        for side in [Side::Buy, Side::Sell] {
            assert_eq!(side_from_wire(side_to_wire(side)), Ok(side));
        }
        for c in [
            Command::Halt,
            Command::Resume,
            Command::Kill,
            Command::Flatten,
        ] {
            assert_eq!(command_from_wire(command_to_wire(c)), Ok(c));
        }
        for s in [
            EngineState::Running,
            EngineState::Halted,
            EngineState::Killed,
        ] {
            assert_eq!(engine_state_from_wire(engine_state_to_wire(s)), Ok(s));
        }
        for r in [
            StateReason::OperatorCommand,
            StateReason::ReconciliationDivergence,
            StateReason::LogWriteFailed,
        ] {
            assert_eq!(state_reason_from_wire(state_reason_to_wire(r)), Ok(r));
        }
        for r in [
            RejectReason::UnknownInstrument,
            RejectReason::InvalidPrice,
            RejectReason::InvalidQuantity,
            RejectReason::InsufficientFunds,
            RejectReason::RateLimited,
            RejectReason::UnknownOrder,
            RejectReason::AlreadyTerminal,
            RejectReason::Unavailable,
        ] {
            assert_eq!(reject_reason_from_wire(reject_reason_to_wire(r)), Ok(r));
        }
        for r in [
            RiskReason::KillSwitchEngaged,
            RiskReason::Halted,
            RiskReason::UnknownInstrument,
            RiskReason::NoLimitsConfigured,
            RiskReason::StaleMarketData,
            RiskReason::NoMarketData,
            RiskReason::PositionLimit,
            RiskReason::ExposureLimit,
            RiskReason::OrderNotionalLimit,
            RiskReason::OrderRateLimit,
            RiskReason::UnreconciledPosition,
            RiskReason::BelowMinimumQty,
            RiskReason::NonPositiveQuantity,
            RiskReason::NonPositivePrice,
            RiskReason::NotReducing,
            RiskReason::Unrepresentable,
            RiskReason::VenueUnreachable,
        ] {
            assert_eq!(risk_reason_from_wire(risk_reason_to_wire(r)), Ok(r));
        }
    }

    #[test]
    fn wire_values_are_pinned_to_their_numbers() {
        // These numbers are the format. Changing one silently re-reads every
        // recorded session as something else, so they are asserted rather than
        // left to whatever the match arms happen to say.
        assert_eq!(side_to_wire(Side::Buy), 1);
        assert_eq!(side_to_wire(Side::Sell), 2);
        assert_eq!(command_to_wire(Command::Kill), 3);
        assert_eq!(engine_state_to_wire(EngineState::Killed), 3);
        assert_eq!(risk_reason_to_wire(RiskReason::KillSwitchEngaged), 1);
        assert_eq!(risk_reason_to_wire(RiskReason::VenueUnreachable), 17);
        assert_eq!(QUOTE, 1);
        assert_eq!(VENUE_FILLED, 5);
        assert_eq!(ORDER_SUBMITTED, 64);
        assert_eq!(STATE_CHANGED, 68);
    }

    #[test]
    fn a_reader_refuses_to_run_past_its_buffer() {
        let buf = [0u8; 4];
        let mut reader = Reader::new(&buf);
        assert_eq!(reader.u32(), Ok(0));
        assert!(matches!(reader.u8(), Err(CodecError::ShortBuffer { .. })));
    }

    #[test]
    fn a_writer_and_reader_agree_on_little_endian() {
        let mut buf = [0u8; 16];
        let mut w = Writer::new(&mut buf);
        w.u16(0x0102);
        w.i64(-2);
        assert_eq!(buf[0], 0x02, "least significant byte first");
        assert_eq!(buf[1], 0x01);

        let mut r = Reader::new(&buf);
        assert_eq!(r.u16(), Ok(0x0102));
        assert_eq!(r.i64(), Ok(-2));
    }
}
