//! The on-disk format: round trips, and every way a record can be wrong.
//!
//! The decoder is the part that has to be tested hardest. An encoder that is
//! wrong produces files nothing can read, which is loud. A decoder that is
//! wrong produces a session that reads cleanly and says something other than
//! what was recorded, which is not.

use event::codec::{
    CodecError, InstrumentEntry, LogHeader, MAGIC, RECORD_LEN, SYMBOL_LEN, decode_record,
    encode_record,
};
use event::{
    Command, CommandEvent, EngineState, Event, FORMAT_VERSION, Inbound, MarketEvent, MarketKind,
    OrderKind, Outbound, PositionReport, Record, RejectReason, RiskReason, Seq, StateReason,
    TimerEvent, TimerToken, VenueEvent, VenueKind,
};
use proptest::prelude::*;
use types::{
    ExchangeTime, Instrument, InstrumentId, Notional, OrderId, Px, Qty, Side, StrategyId, Timestamp,
};

fn at(n: i64) -> ExchangeTime {
    Timestamp::from_nanos(n)
}

fn record(event: Event) -> Record {
    Record {
        seq: Seq::new(7),
        version: FORMAT_VERSION,
        event,
    }
}

fn round_trip(event: Event) -> Record {
    let original = record(event);
    let mut bytes = [0u8; RECORD_LEN];
    encode_record(&original, &mut bytes);
    let decoded = decode_record(&bytes).expect("decode");
    assert_eq!(decoded, original);
    decoded
}

/// Every record kind, one per variant.
///
/// Listed rather than generated, so a kind added to the alphabet without a case
/// here is visible in the diff. The property test below covers the field
/// values; this covers the shapes.
fn every_kind() -> Vec<Event> {
    vec![
        Event::In(Inbound::Market(MarketEvent {
            instrument: InstrumentId::new(3),
            exchange_time: at(1_000),
            receive_time: Timestamp::from_nanos(1_100),
            kind: MarketKind::Quote {
                bid_px: Px::from_scaled(-99),
                bid_qty: Qty::from_scaled(1),
                ask_px: Px::MAX,
                ask_qty: Qty::MIN,
            },
        })),
        Event::In(Inbound::Market(MarketEvent {
            instrument: InstrumentId::new(0),
            exchange_time: at(2_000),
            receive_time: Timestamp::from_nanos(2_100),
            kind: MarketKind::Trade {
                px: Px::from_scaled(i64::MIN),
                qty: Qty::from_scaled(i64::MAX),
                aggressor: Side::Sell,
            },
        })),
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(u64::MAX),
            venue_time: at(3_000),
            receive_time: Timestamp::from_nanos(3_100),
            kind: VenueKind::Accepted,
        })),
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(1),
            venue_time: at(3_000),
            receive_time: Timestamp::from_nanos(3_100),
            kind: VenueKind::Rejected {
                reason: RejectReason::InsufficientFunds,
            },
        })),
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(2),
            venue_time: at(3_000),
            receive_time: Timestamp::from_nanos(3_100),
            // The widest payload in the format.
            kind: VenueKind::Filled {
                px: Px::MIN,
                qty: Qty::MAX,
                fee: Notional::MIN,
            },
        })),
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(3),
            venue_time: at(3_000),
            receive_time: Timestamp::from_nanos(3_100),
            kind: VenueKind::Cancelled,
        })),
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(4),
            venue_time: at(3_000),
            receive_time: Timestamp::from_nanos(3_100),
            kind: VenueKind::CancelRejected {
                reason: RejectReason::UnknownOrder,
            },
        })),
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(5),
            venue_time: at(3_000),
            receive_time: Timestamp::from_nanos(3_100),
            kind: VenueKind::Expired,
        })),
        Event::In(Inbound::VenuePosition(PositionReport {
            instrument: InstrumentId::new(1),
            venue_qty: Qty::from_scaled(-42),
            venue_time: at(4_000),
            receive_time: Timestamp::from_nanos(4_100),
        })),
        Event::In(Inbound::Timer(TimerEvent {
            strategy: StrategyId::new(u16::MAX),
            token: TimerToken::new(u64::MAX),
            fires_at: at(i64::MAX),
            receive_time: Timestamp::from_nanos(i64::MIN),
        })),
        Event::In(Inbound::Command(CommandEvent {
            command: Command::Flatten,
            receive_time: Timestamp::from_nanos(5_000),
        })),
        Event::Out(Outbound::OrderSubmitted {
            caused_by: Seq::new(6),
            order: OrderId::new(9),
            strategy: StrategyId::new(2),
            instrument: InstrumentId::new(1),
            side: Side::Buy,
            qty: Qty::from_scaled(1_000),
            kind: OrderKind::Limit(Px::from_scaled(-7)),
            reduce_only: true,
        }),
        Event::Out(Outbound::OrderSubmitted {
            caused_by: Seq::new(6),
            order: OrderId::new(10),
            strategy: StrategyId::new(0),
            instrument: InstrumentId::new(0),
            side: Side::Sell,
            qty: Qty::MIN,
            kind: OrderKind::Market,
            reduce_only: false,
        }),
        Event::Out(Outbound::CancelSubmitted {
            caused_by: Seq::new(11),
            order: OrderId::new(12),
        }),
        Event::Out(Outbound::IntentRejected {
            caused_by: Seq::new(13),
            strategy: StrategyId::new(1),
            instrument: InstrumentId::new(2),
            side: Side::Sell,
            qty: Qty::from_scaled(-1),
            reason: RiskReason::VenueUnreachable,
        }),
        Event::Out(Outbound::TimerRequested {
            caused_by: Seq::new(14),
            strategy: StrategyId::new(3),
            token: TimerToken::new(88),
            at: at(-1),
        }),
        Event::Out(Outbound::StateChanged {
            caused_by: Seq::new(15),
            from: EngineState::Running,
            to: EngineState::Killed,
            reason: StateReason::ReconciliationDivergence,
        }),
    ]
}

#[test]
fn every_record_kind_round_trips() {
    let kinds = every_kind();
    assert_eq!(
        kinds.len(),
        17,
        "a kind was added or removed without a case"
    );
    for event in kinds {
        round_trip(event);
    }
}

#[test]
fn a_record_is_exactly_eighty_bytes() {
    assert_eq!(RECORD_LEN, 80);
}

#[test]
fn encoding_is_canonical() {
    // The same record always produces the same bytes. This is what makes two
    // logs of one session comparable byte for byte, which is what D0.2 means
    // by "replays byte-identically".
    for event in every_kind() {
        let r = record(event);
        let mut first = [0u8; RECORD_LEN];
        let mut second = [0xAAu8; RECORD_LEN]; // deliberately dirty
        encode_record(&r, &mut first);
        encode_record(&r, &mut second);
        assert_eq!(first, second, "encoding depended on the buffer's contents");
    }
}

#[test]
fn the_widest_record_leaves_its_reserved_bytes_free() {
    // Venue/Filled is the widest payload: a fee is a Notional, so i128. If this
    // ever fails, the record size has to grow and that is a format version.
    let widest = record(Event::In(Inbound::Venue(VenueEvent {
        order: OrderId::new(u64::MAX),
        venue_time: at(i64::MAX),
        receive_time: Timestamp::from_nanos(i64::MAX),
        kind: VenueKind::Filled {
            px: Px::MAX,
            qty: Qty::MAX,
            fee: Notional::MAX,
        },
    })));
    let mut bytes = [0u8; RECORD_LEN];
    encode_record(&widest, &mut bytes);
    assert_eq!(&bytes[68..76], &[0u8; 8], "the reserved bytes were used");
}

// ---- corruption ----------------------------------------------------------

fn encoded(event: Event) -> [u8; RECORD_LEN] {
    let mut bytes = [0u8; RECORD_LEN];
    encode_record(&record(event), &mut bytes);
    bytes
}

fn a_quote() -> Event {
    Event::In(Inbound::Market(MarketEvent {
        instrument: InstrumentId::new(1),
        exchange_time: at(10),
        receive_time: Timestamp::from_nanos(11),
        kind: MarketKind::Quote {
            bid_px: Px::from_scaled(99),
            bid_qty: Qty::from_scaled(5),
            ask_px: Px::from_scaled(101),
            ask_qty: Qty::from_scaled(5),
        },
    }))
}

#[test]
fn a_single_flipped_bit_anywhere_is_caught() {
    // Every byte the checksum covers, one bit each. A format that only checked
    // some of the record would pass most of these.
    let clean = encoded(a_quote());
    for byte in 0..76 {
        for bit in 0..8 {
            let mut corrupt = clean;
            corrupt[byte] ^= 1 << bit;
            let result = decode_record(&corrupt);
            assert!(
                result.is_err(),
                "byte {byte} bit {bit} was flipped and decoded anyway as {result:?}"
            );
        }
    }
}

#[test]
fn a_non_zero_reserved_field_is_refused() {
    let mut bytes = encoded(a_quote());
    bytes[70] = 1;
    // Re-stamp the checksum, so this tests the reserved check rather than the
    // checksum catching it by accident.
    let crc = recrc(&bytes);
    bytes[76..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(decode_record(&bytes), Err(CodecError::ReservedNotZero));
}

#[test]
fn an_unknown_record_kind_is_refused_rather_than_skipped() {
    let mut bytes = encoded(a_quote());
    bytes[10] = 200;
    let crc = recrc(&bytes);
    bytes[76..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(decode_record(&bytes), Err(CodecError::UnknownKind(200)));
}

#[test]
fn an_unknown_flag_bit_is_refused() {
    // A flag this build does not know means the writer said something about
    // this record that the reader would be dropping.
    let mut bytes = encoded(a_quote());
    bytes[11] = 0b1000_0000;
    let crc = recrc(&bytes);
    bytes[76..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        decode_record(&bytes),
        Err(CodecError::UnknownFlags(0b1000_0000))
    );
}

#[test]
fn a_record_from_a_newer_format_is_refused() {
    let mut bytes = encoded(a_quote());
    bytes[8..10].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    let crc = recrc(&bytes);
    bytes[76..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        decode_record(&bytes),
        Err(CodecError::UnsupportedVersion(FORMAT_VERSION + 1))
    );
}

#[test]
fn an_all_zero_record_does_not_decode() {
    // The property that makes a wiped or unwritten region loud: no enum's wire
    // value is 0, so zeros are never a well-formed record.
    let zeros = [0u8; RECORD_LEN];
    assert!(decode_record(&zeros).is_err());
}

#[test]
fn a_zeroed_enum_field_is_refused_rather_than_read_as_the_first_variant() {
    let mut bytes = encoded(Event::In(Inbound::Command(CommandEvent {
        command: Command::Halt,
        receive_time: Timestamp::from_nanos(1),
    })));
    bytes[12] = 0; // the command byte
    let crc = recrc(&bytes);
    bytes[76..].copy_from_slice(&crc.to_le_bytes());
    assert_eq!(
        decode_record(&bytes),
        Err(CodecError::UnknownEnumValue {
            field: "command",
            value: 0
        })
    );
}

/// Recomputes a record's checksum, so a test can corrupt a field without the
/// checksum masking the check it is actually exercising.
fn recrc(bytes: &[u8; RECORD_LEN]) -> u32 {
    // Same algorithm, computed independently of the codec's own helper.
    let mut crc = 0xFFFF_FFFFu32;
    for &b in &bytes[..76] {
        crc ^= u32::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

// ---- header --------------------------------------------------------------

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
        0xDEAD_BEEF,
        at(1_700_000_000_000_000_000),
        OrderId::new(500),
        vec![
            InstrumentEntry::new(instrument(0), "AAPL").expect("entry"),
            InstrumentEntry::new(instrument(1), "BRK.A").expect("entry"),
        ],
    )
}

#[test]
fn a_header_round_trips() {
    let original = header();
    let mut bytes = Vec::new();
    original.encode(&mut bytes);
    assert_eq!(bytes.len(), original.encoded_len());
    assert_eq!(LogHeader::decode(&bytes).expect("decode"), original);
}

#[test]
fn a_header_names_its_instruments() {
    let h = header();
    assert_eq!(h.instruments[0].symbol_str(), Some("AAPL"));
    assert_eq!(h.instruments[1].symbol_str(), Some("BRK.A"));
}

#[test]
fn a_symbol_that_does_not_fit_is_refused() {
    let long = "A".repeat(SYMBOL_LEN + 1);
    assert_eq!(
        InstrumentEntry::new(instrument(0), &long),
        Err(CodecError::SymbolTooLong {
            len: SYMBOL_LEN + 1,
            max: SYMBOL_LEN
        })
    );
}

#[test]
fn record_offsets_are_arithmetic() {
    // The reason for a fixed record size: seeking to a sequence number needs no
    // index.
    let h = header();
    let base = h.encoded_len() as u64;
    assert_eq!(h.offset_of(0), base);
    assert_eq!(h.offset_of(1), base + RECORD_LEN as u64);
    assert_eq!(h.offset_of(1_000_000), base + 1_000_000 * RECORD_LEN as u64);
}

#[test]
fn a_file_that_is_not_a_log_is_refused_on_the_first_read() {
    let mut bytes = Vec::new();
    header().encode(&mut bytes);
    bytes[0] = b'X';
    assert_eq!(LogHeader::decode(&bytes), Err(CodecError::BadMagic));
    assert_eq!(&MAGIC[..], b"ULTRALOG");
}

#[test]
fn a_header_from_a_newer_format_is_refused() {
    let mut bytes = Vec::new();
    header().encode(&mut bytes);
    bytes[8..10].copy_from_slice(&(FORMAT_VERSION + 1).to_le_bytes());
    assert_eq!(
        LogHeader::decode(&bytes),
        Err(CodecError::UnsupportedVersion(FORMAT_VERSION + 1))
    );
}

#[test]
fn a_header_declaring_a_different_record_size_is_refused() {
    let mut bytes = Vec::new();
    header().encode(&mut bytes);
    bytes[10..12].copy_from_slice(&64u16.to_le_bytes());
    assert_eq!(
        LogHeader::decode(&bytes),
        Err(CodecError::UnexpectedRecordLen(64))
    );
}

#[test]
fn a_truncated_header_is_refused_rather_than_read_short() {
    let mut bytes = Vec::new();
    header().encode(&mut bytes);
    let full = bytes.len();
    bytes.truncate(full - 1);
    assert!(matches!(
        LogHeader::decode(&bytes),
        Err(CodecError::ShortBuffer { .. })
    ));
}

#[test]
fn a_header_whose_declared_length_disagrees_with_its_contents_is_refused() {
    let mut bytes = Vec::new();
    header().encode(&mut bytes);
    bytes[12..16].copy_from_slice(&9_999u32.to_le_bytes());
    assert!(matches!(
        LogHeader::decode(&bytes),
        Err(CodecError::HeaderLengthMismatch { .. })
    ));
}

#[test]
fn a_header_claiming_absurdly_many_instruments_is_refused_before_allocating() {
    let mut bytes = Vec::new();
    header().encode(&mut bytes);
    bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(matches!(
        LogHeader::decode(&bytes),
        Err(CodecError::TooManyInstruments { .. })
    ));
}

#[test]
fn a_matching_configuration_is_accepted() {
    assert_eq!(
        header().check_against(&[instrument(0), instrument(1)]),
        Ok(())
    );
}

#[test]
fn a_configuration_with_different_conventions_is_refused() {
    // The check that matters: without it, InstrumentId(1) in the file silently
    // means whatever instrument 1 is now, and a backtest reads one contract's
    // prices as another's.
    let different = Instrument::new(
        InstrumentId::new(1),
        Px::from_scaled(50_000_000), // a different tick
        Qty::from_scaled(1_000_000_000),
        Qty::from_scaled(1_000_000_000),
    )
    .expect("conventions");
    assert_eq!(
        header().check_against(&[instrument(0), different]),
        Err(CodecError::InstrumentMismatch {
            id: InstrumentId::new(1)
        })
    );
}

#[test]
fn a_configuration_with_a_different_instrument_count_is_refused() {
    assert_eq!(
        header().check_against(&[instrument(0)]),
        Err(CodecError::InstrumentCountMismatch {
            recorded: 2,
            configured: 1
        })
    );
}

// ---- property ------------------------------------------------------------

fn any_side() -> impl Strategy<Value = Side> {
    prop_oneof![Just(Side::Buy), Just(Side::Sell)]
}

fn any_event() -> impl Strategy<Value = Event> {
    prop_oneof![
        (
            any::<u32>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>()
        )
            .prop_map(|(i, e, r, bp, bq, ap, aq)| Event::In(Inbound::Market(
                MarketEvent {
                    instrument: InstrumentId::new(i),
                    exchange_time: at(e),
                    receive_time: Timestamp::from_nanos(r),
                    kind: MarketKind::Quote {
                        bid_px: Px::from_scaled(bp),
                        bid_qty: Qty::from_scaled(bq),
                        ask_px: Px::from_scaled(ap),
                        ask_qty: Qty::from_scaled(aq),
                    },
                }
            ))),
        (
            any::<u32>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>(),
            any_side()
        )
            .prop_map(|(i, e, r, p, q, s)| Event::In(Inbound::Market(MarketEvent {
                instrument: InstrumentId::new(i),
                exchange_time: at(e),
                receive_time: Timestamp::from_nanos(r),
                kind: MarketKind::Trade {
                    px: Px::from_scaled(p),
                    qty: Qty::from_scaled(q),
                    aggressor: s,
                },
            }))),
        (
            any::<u64>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>(),
            any::<i64>(),
            any::<i128>()
        )
            .prop_map(|(o, v, r, p, q, f)| Event::In(Inbound::Venue(VenueEvent {
                order: OrderId::new(o),
                venue_time: at(v),
                receive_time: Timestamp::from_nanos(r),
                kind: VenueKind::Filled {
                    px: Px::from_scaled(p),
                    qty: Qty::from_scaled(q),
                    fee: Notional::from_scaled(f),
                },
            }))),
        (any::<u32>(), any::<i64>(), any::<i64>(), any::<i64>()).prop_map(|(i, q, v, r)| {
            Event::In(Inbound::VenuePosition(PositionReport {
                instrument: InstrumentId::new(i),
                venue_qty: Qty::from_scaled(q),
                venue_time: at(v),
                receive_time: Timestamp::from_nanos(r),
            }))
        }),
        (
            any::<u64>(),
            any::<u64>(),
            any::<u16>(),
            any::<u32>(),
            any_side(),
            any::<i64>(),
            any::<i64>(),
            any::<bool>()
        )
            .prop_map(|(c, o, s, i, side, q, px, reduce)| Event::Out(
                Outbound::OrderSubmitted {
                    caused_by: Seq::new(c),
                    order: OrderId::new(o),
                    strategy: StrategyId::new(s),
                    instrument: InstrumentId::new(i),
                    side,
                    qty: Qty::from_scaled(q),
                    kind: if reduce {
                        OrderKind::Limit(Px::from_scaled(px))
                    } else {
                        OrderKind::Market
                    },
                    reduce_only: reduce,
                }
            )),
        (any::<u64>(), any::<u16>(), any::<u64>(), any::<i64>()).prop_map(|(c, s, t, a)| {
            Event::Out(Outbound::TimerRequested {
                caused_by: Seq::new(c),
                strategy: StrategyId::new(s),
                token: TimerToken::new(t),
                at: at(a),
            })
        }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2_000))]

    /// Any record survives the round trip with every field intact.
    #[test]
    fn any_record_round_trips(seq in any::<u64>(), event in any_event()) {
        let original = Record { seq: Seq::new(seq), version: FORMAT_VERSION, event };
        let mut bytes = [0u8; RECORD_LEN];
        encode_record(&original, &mut bytes);
        prop_assert_eq!(decode_record(&bytes).expect("decode"), original);
    }

    /// Encoding twice produces the same bytes, whatever the buffer held before.
    #[test]
    fn encoding_never_depends_on_what_was_in_the_buffer(
        seq in any::<u64>(),
        event in any_event(),
        fill in any::<u8>(),
    ) {
        let r = Record { seq: Seq::new(seq), version: FORMAT_VERSION, event };
        let mut clean = [0u8; RECORD_LEN];
        let mut dirty = [fill; RECORD_LEN];
        encode_record(&r, &mut clean);
        encode_record(&r, &mut dirty);
        prop_assert_eq!(clean, dirty);
    }
}
