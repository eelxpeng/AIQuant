//! Encoding and decoding one 80-byte record.

use super::crc32::crc32;
use super::wire::*;
use super::{CodecError, RECORD_LEN};
use crate::{
    CommandEvent, Event, Inbound, MarketEvent, MarketKind, OrderKind, Outbound, PositionReport,
    Record, Seq, TimerEvent, TimerToken, VenueEvent, VenueKind,
};
use types::{InstrumentId, Notional, OrderId, Px, Qty, StrategyId, Timestamp};

/// Where the payload starts.
const PAYLOAD_AT: usize = 12;
/// How much room a payload has. The widest is `Venue/Filled` at 56 bytes.
pub(super) const PAYLOAD_LEN: usize = 56;
/// Reserved bytes, which must be zero.
const RESERVED_AT: usize = 68;
const RESERVED_LEN: usize = 8;
/// The checksum covers everything before it.
const CRC_AT: usize = 76;

/// Encodes a record into exactly [`RECORD_LEN`] bytes.
///
/// The encoding is **canonical**: the buffer is zeroed first, so every field
/// that is not written stays zero and the same record always produces the same
/// bytes. Two logs of the same session are therefore comparable byte for byte,
/// which is what D0.2 asks of replay.
pub fn encode_record(record: &Record, out: &mut [u8; RECORD_LEN]) {
    out.fill(0);

    let (kind, flags) = kind_and_flags(&record.event);
    {
        let mut head = Writer::new(&mut out[..PAYLOAD_AT]);
        head.u64(record.seq.raw());
        head.u16(record.version);
        head.u8(kind);
        head.u8(flags);
        debug_assert_eq!(head.position(), PAYLOAD_AT);
    }
    {
        // Bounded to the payload window, so a payload that outgrew its room
        // fails here rather than silently overwriting the checksum.
        let mut body = Writer::new(&mut out[PAYLOAD_AT..PAYLOAD_AT + PAYLOAD_LEN]);
        write_payload(&record.event, &mut body);
        debug_assert!(body.position() <= PAYLOAD_LEN);
    }
    // Bytes RESERVED_AT..CRC_AT stay zero, from the fill above.
    let checksum = crc32(&out[..CRC_AT]);
    out[CRC_AT..].copy_from_slice(&checksum.to_le_bytes());
}

/// Decodes a record from exactly [`RECORD_LEN`] bytes.
///
/// Checks in this order: checksum, reserved bytes, then contents. The checksum
/// first, because a corrupt record's fields are not worth complaining about
/// individually.
pub fn decode_record(bytes: &[u8; RECORD_LEN]) -> Result<Record, CodecError> {
    let found = u32::from_le_bytes(bytes[CRC_AT..].try_into().expect("4 bytes"));
    let expected = crc32(&bytes[..CRC_AT]);
    if found != expected {
        return Err(CodecError::BadChecksum { expected, found });
    }
    if bytes[RESERVED_AT..RESERVED_AT + RESERVED_LEN]
        .iter()
        .any(|b| *b != 0)
    {
        return Err(CodecError::ReservedNotZero);
    }

    let mut head = Reader::new(&bytes[..PAYLOAD_AT]);
    let seq = Seq::new(head.u64()?);
    let version = head.u16()?;
    // Per record, not just per file: a log may span a binary upgrade, and a
    // record written by a newer format may lay its payload out differently.
    // Parsing it with this build's rules would produce plausible nonsense.
    if version > crate::FORMAT_VERSION {
        return Err(CodecError::UnsupportedVersion(version));
    }
    let kind = head.u8()?;
    let flags = head.u8()?;

    let mut body = Reader::new(&bytes[PAYLOAD_AT..PAYLOAD_AT + PAYLOAD_LEN]);
    let event = read_payload(kind, flags, &mut body)?;
    Ok(Record {
        seq,
        version,
        event,
    })
}

fn kind_and_flags(event: &Event) -> (u8, u8) {
    match event {
        Event::In(Inbound::Market(m)) => (
            match m.kind {
                MarketKind::Quote { .. } => QUOTE,
                MarketKind::Trade { .. } => TRADE,
                MarketKind::Level { .. } => BOOK_LEVEL,
                MarketKind::BookApplied => BOOK_APPLIED,
                MarketKind::BookReset => BOOK_RESET,
            },
            0,
        ),
        Event::In(Inbound::Venue(v)) => (
            match v.kind {
                VenueKind::Accepted => VENUE_ACCEPTED,
                VenueKind::Rejected { .. } => VENUE_REJECTED,
                VenueKind::Filled { .. } => VENUE_FILLED,
                VenueKind::Cancelled => VENUE_CANCELLED,
                VenueKind::CancelRejected { .. } => VENUE_CANCEL_REJECTED,
                VenueKind::Expired => VENUE_EXPIRED,
            },
            0,
        ),
        Event::In(Inbound::VenuePosition(_)) => (VENUE_POSITION, 0),
        Event::In(Inbound::Timer(_)) => (TIMER, 0),
        Event::In(Inbound::Command(_)) => (COMMAND, 0),
        Event::Out(Outbound::OrderSubmitted { reduce_only, .. }) => (
            ORDER_SUBMITTED,
            if *reduce_only { FLAG_REDUCE_ONLY } else { 0 },
        ),
        Event::Out(Outbound::CancelSubmitted { .. }) => (CANCEL_SUBMITTED, 0),
        Event::Out(Outbound::IntentRejected { .. }) => (INTENT_REJECTED, 0),
        Event::Out(Outbound::TimerRequested { .. }) => (TIMER_REQUESTED, 0),
        Event::Out(Outbound::StateChanged { .. }) => (STATE_CHANGED, 0),
    }
}

fn write_payload(event: &Event, w: &mut Writer<'_>) {
    match event {
        Event::In(Inbound::Market(m)) => {
            w.u32(m.instrument.raw());
            w.i64(m.exchange_time.to_nanos());
            w.i64(m.receive_time.to_nanos());
            match m.kind {
                MarketKind::Quote {
                    bid_px,
                    bid_qty,
                    ask_px,
                    ask_qty,
                } => {
                    w.i64(bid_px.to_scaled());
                    w.i64(bid_qty.to_scaled());
                    w.i64(ask_px.to_scaled());
                    w.i64(ask_qty.to_scaled());
                }
                MarketKind::Level { side, px, qty } => {
                    w.u8(side_to_wire(side));
                    w.i64(px.to_scaled());
                    w.i64(qty.to_scaled());
                }
                MarketKind::BookApplied | MarketKind::BookReset => {}
                MarketKind::Trade { px, qty, aggressor } => {
                    w.i64(px.to_scaled());
                    w.i64(qty.to_scaled());
                    w.u8(side_to_wire(aggressor));
                }
            }
        }
        Event::In(Inbound::Venue(v)) => {
            w.u64(v.order.raw());
            w.i64(v.venue_time.to_nanos());
            w.i64(v.receive_time.to_nanos());
            match v.kind {
                VenueKind::Accepted | VenueKind::Cancelled | VenueKind::Expired => {}
                VenueKind::Rejected { reason } | VenueKind::CancelRejected { reason } => {
                    w.u8(reject_reason_to_wire(reason));
                }
                VenueKind::Filled { px, qty, fee } => {
                    w.i64(px.to_scaled());
                    w.i64(qty.to_scaled());
                    w.i128(fee.to_scaled());
                }
            }
        }
        Event::In(Inbound::VenuePosition(p)) => {
            w.u32(p.instrument.raw());
            w.i64(p.venue_qty.to_scaled());
            w.i64(p.venue_time.to_nanos());
            w.i64(p.receive_time.to_nanos());
        }
        Event::In(Inbound::Timer(t)) => {
            w.u16(t.strategy.raw());
            w.u64(t.token.raw());
            w.i64(t.fires_at.to_nanos());
            w.i64(t.receive_time.to_nanos());
        }
        Event::In(Inbound::Command(c)) => {
            w.u8(command_to_wire(c.command));
            w.i64(c.receive_time.to_nanos());
        }
        Event::Out(Outbound::OrderSubmitted {
            caused_by,
            order,
            strategy,
            instrument,
            side,
            qty,
            kind,
            ..
        }) => {
            w.u64(caused_by.raw());
            w.u64(order.raw());
            w.u16(strategy.raw());
            w.u32(instrument.raw());
            w.u8(side_to_wire(*side));
            w.i64(qty.to_scaled());
            match kind {
                OrderKind::Market => {
                    w.u8(ORDER_KIND_MARKET);
                    w.i64(0);
                }
                OrderKind::Limit(px) => {
                    w.u8(ORDER_KIND_LIMIT);
                    w.i64(px.to_scaled());
                }
            }
        }
        Event::Out(Outbound::CancelSubmitted { caused_by, order }) => {
            w.u64(caused_by.raw());
            w.u64(order.raw());
        }
        Event::Out(Outbound::IntentRejected {
            caused_by,
            strategy,
            instrument,
            side,
            qty,
            reason,
        }) => {
            w.u64(caused_by.raw());
            w.u16(strategy.raw());
            w.u32(instrument.raw());
            w.u8(side_to_wire(*side));
            w.i64(qty.to_scaled());
            w.u8(risk_reason_to_wire(*reason));
        }
        Event::Out(Outbound::TimerRequested {
            caused_by,
            strategy,
            token,
            at,
        }) => {
            w.u64(caused_by.raw());
            w.u16(strategy.raw());
            w.u64(token.raw());
            w.i64(at.to_nanos());
        }
        Event::Out(Outbound::StateChanged {
            caused_by,
            from,
            to,
            reason,
        }) => {
            w.u64(caused_by.raw());
            w.u8(engine_state_to_wire(*from));
            w.u8(engine_state_to_wire(*to));
            w.u8(state_reason_to_wire(*reason));
        }
    }
}

fn read_payload(kind: u8, flags: u8, r: &mut Reader<'_>) -> Result<Event, CodecError> {
    let event = match kind {
        QUOTE | TRADE | BOOK_LEVEL | BOOK_APPLIED | BOOK_RESET => {
            let instrument = InstrumentId::new(r.u32()?);
            let exchange_time = Timestamp::from_nanos(r.i64()?);
            let receive_time = Timestamp::from_nanos(r.i64()?);
            let market_kind = match kind {
                QUOTE => MarketKind::Quote {
                    bid_px: Px::from_scaled(r.i64()?),
                    bid_qty: Qty::from_scaled(r.i64()?),
                    ask_px: Px::from_scaled(r.i64()?),
                    ask_qty: Qty::from_scaled(r.i64()?),
                },
                TRADE => MarketKind::Trade {
                    px: Px::from_scaled(r.i64()?),
                    qty: Qty::from_scaled(r.i64()?),
                    aggressor: side_from_wire(r.u8()?)?,
                },
                BOOK_LEVEL => MarketKind::Level {
                    side: side_from_wire(r.u8()?)?,
                    px: Px::from_scaled(r.i64()?),
                    qty: Qty::from_scaled(r.i64()?),
                },
                BOOK_RESET => MarketKind::BookReset,
                _ => MarketKind::BookApplied,
            };
            Event::In(Inbound::Market(MarketEvent {
                instrument,
                exchange_time,
                receive_time,
                kind: market_kind,
            }))
        }

        VENUE_ACCEPTED
        | VENUE_REJECTED
        | VENUE_FILLED
        | VENUE_CANCELLED
        | VENUE_CANCEL_REJECTED
        | VENUE_EXPIRED => {
            let order = OrderId::new(r.u64()?);
            let venue_time = Timestamp::from_nanos(r.i64()?);
            let receive_time = Timestamp::from_nanos(r.i64()?);
            let venue_kind = match kind {
                VENUE_ACCEPTED => VenueKind::Accepted,
                VENUE_CANCELLED => VenueKind::Cancelled,
                VENUE_EXPIRED => VenueKind::Expired,
                VENUE_REJECTED => VenueKind::Rejected {
                    reason: reject_reason_from_wire(r.u8()?)?,
                },
                VENUE_CANCEL_REJECTED => VenueKind::CancelRejected {
                    reason: reject_reason_from_wire(r.u8()?)?,
                },
                _ => VenueKind::Filled {
                    px: Px::from_scaled(r.i64()?),
                    qty: Qty::from_scaled(r.i64()?),
                    fee: Notional::from_scaled(r.i128()?),
                },
            };
            Event::In(Inbound::Venue(VenueEvent {
                order,
                venue_time,
                receive_time,
                kind: venue_kind,
            }))
        }

        VENUE_POSITION => Event::In(Inbound::VenuePosition(PositionReport {
            instrument: InstrumentId::new(r.u32()?),
            venue_qty: Qty::from_scaled(r.i64()?),
            venue_time: Timestamp::from_nanos(r.i64()?),
            receive_time: Timestamp::from_nanos(r.i64()?),
        })),

        TIMER => Event::In(Inbound::Timer(TimerEvent {
            strategy: StrategyId::new(r.u16()?),
            token: TimerToken::new(r.u64()?),
            fires_at: Timestamp::from_nanos(r.i64()?),
            receive_time: Timestamp::from_nanos(r.i64()?),
        })),

        COMMAND => Event::In(Inbound::Command(CommandEvent {
            command: command_from_wire(r.u8()?)?,
            receive_time: Timestamp::from_nanos(r.i64()?),
        })),

        ORDER_SUBMITTED => {
            let caused_by = Seq::new(r.u64()?);
            let order = OrderId::new(r.u64()?);
            let strategy = StrategyId::new(r.u16()?);
            let instrument = InstrumentId::new(r.u32()?);
            let side = side_from_wire(r.u8()?)?;
            let qty = Qty::from_scaled(r.i64()?);
            let tag = r.u8()?;
            let px = Px::from_scaled(r.i64()?);
            let order_kind = match tag {
                ORDER_KIND_MARKET => OrderKind::Market,
                ORDER_KIND_LIMIT => OrderKind::Limit(px),
                value => {
                    return Err(CodecError::UnknownEnumValue {
                        field: "order kind",
                        value,
                    });
                }
            };
            Event::Out(Outbound::OrderSubmitted {
                caused_by,
                order,
                strategy,
                instrument,
                side,
                qty,
                kind: order_kind,
                reduce_only: flags & FLAG_REDUCE_ONLY != 0,
            })
        }

        CANCEL_SUBMITTED => Event::Out(Outbound::CancelSubmitted {
            caused_by: Seq::new(r.u64()?),
            order: OrderId::new(r.u64()?),
        }),

        INTENT_REJECTED => Event::Out(Outbound::IntentRejected {
            caused_by: Seq::new(r.u64()?),
            strategy: StrategyId::new(r.u16()?),
            instrument: InstrumentId::new(r.u32()?),
            side: side_from_wire(r.u8()?)?,
            qty: Qty::from_scaled(r.i64()?),
            reason: risk_reason_from_wire(r.u8()?)?,
        }),

        TIMER_REQUESTED => Event::Out(Outbound::TimerRequested {
            caused_by: Seq::new(r.u64()?),
            strategy: StrategyId::new(r.u16()?),
            token: TimerToken::new(r.u64()?),
            at: Timestamp::from_nanos(r.i64()?),
        }),

        STATE_CHANGED => Event::Out(Outbound::StateChanged {
            caused_by: Seq::new(r.u64()?),
            from: engine_state_from_wire(r.u8()?)?,
            to: engine_state_from_wire(r.u8()?)?,
            reason: state_reason_from_wire(r.u8()?)?,
        }),

        unknown => return Err(CodecError::UnknownKind(unknown)),
    };

    // A flag this build does not know is not a flag to ignore: it means the
    // writer said something about this record that the reader is dropping.
    if flags & !FLAG_REDUCE_ONLY != 0 {
        return Err(CodecError::UnknownFlags(flags));
    }
    Ok(event)
}
