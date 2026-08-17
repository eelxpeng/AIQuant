//! Cutting a recording into a part to choose on and a part to be judged on.
//!
//! Getting this wrong is quiet and expensive: a split that hands most of the
//! market to one side makes the out-of-sample column look like agreement when
//! it is mostly the same data, which is exactly the reassurance the split
//! exists to withhold.

use event::{
    Event, FORMAT_VERSION, Inbound, MarketEvent, MarketKind, Outbound, Record, Seq, VenueEvent,
    VenueKind,
};
use harness::split_by_market_events;
use types::{InstrumentId, OrderId, Px, Qty, SCALE, Side, StrategyId, Timestamp};

const I: InstrumentId = InstrumentId::new(0);

fn record(seq: u64, event: Event) -> Record {
    Record {
        seq: Seq::new(seq),
        version: FORMAT_VERSION,
        event,
    }
}

fn market(seq: u64) -> Record {
    record(
        seq,
        Event::In(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(seq as i64),
            receive_time: Timestamp::from_nanos(seq as i64),
            kind: MarketKind::Quote {
                bid_px: Px::from_scaled(SCALE),
                bid_qty: Qty::from_scaled(SCALE),
                ask_px: Px::from_scaled(2 * SCALE),
                ask_qty: Qty::from_scaled(SCALE),
            },
        })),
    )
}

/// A decision, which is not market data and must not count towards the split.
fn decision(seq: u64) -> Record {
    record(
        seq,
        Event::Out(Outbound::OrderSubmitted {
            caused_by: Seq::new(0),
            order: OrderId::new(seq),
            strategy: StrategyId::new(0),
            instrument: I,
            side: Side::Buy,
            qty: Qty::from_scaled(SCALE),
            kind: event::OrderKind::Market,
            reduce_only: false,
        }),
    )
}

fn venue(seq: u64) -> Record {
    record(
        seq,
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(seq),
            venue_time: Timestamp::from_nanos(seq as i64),
            receive_time: Timestamp::from_nanos(seq as i64),
            kind: VenueKind::Accepted,
        })),
    )
}

fn count_market(records: &[Record]) -> usize {
    records
        .iter()
        .filter(|r| matches!(r.event, Event::In(Inbound::Market(_))))
        .count()
}

#[test]
fn a_half_split_puts_half_the_market_on_each_side() {
    let records: Vec<Record> = (0..10).map(market).collect();
    let (a, b) = split_by_market_events(&records, 0.5).expect("split");
    assert_eq!(count_market(a), 5);
    assert_eq!(count_market(b), 5);
}

#[test]
fn the_split_counts_market_events_and_not_records() {
    // The reason it is not a record-index split. Decisions and venue reports
    // cluster around active moments, so cutting on record count hands the
    // busier half of the session to whichever side had more trading in it.
    let mut records = Vec::new();
    // Five quiet market events...
    for seq in 0..5 {
        records.push(market(seq));
    }
    // ...then five more, each buried in a burst of trading activity.
    for seq in 5..10 {
        records.push(market(seq * 10));
        records.push(decision(seq * 10 + 1));
        records.push(venue(seq * 10 + 2));
        records.push(decision(seq * 10 + 3));
        records.push(venue(seq * 10 + 4));
    }

    let (a, b) = split_by_market_events(&records, 0.5).expect("split");
    assert_eq!(count_market(a), 5, "five market events either side");
    assert_eq!(count_market(b), 5);
    assert!(
        a.len() < b.len(),
        "the busy half holds far more records, which is the point"
    );
}

#[test]
fn every_record_ends_up_on_exactly_one_side() {
    let mut records = Vec::new();
    for seq in 0..8 {
        records.push(market(seq * 3));
        records.push(decision(seq * 3 + 1));
    }
    let (a, b) = split_by_market_events(&records, 0.6).expect("split");
    assert_eq!(a.len() + b.len(), records.len(), "nothing lost or doubled");
    assert_eq!(count_market(a) + count_market(b), count_market(&records));
}

#[test]
fn a_split_never_leaves_a_side_with_no_market_at_all() {
    // A fraction near the edge would otherwise produce an out-of-sample column
    // measured on nothing, which reads as "no disagreement" rather than "no
    // evidence".
    let records: Vec<Record> = (0..10).map(market).collect();
    for fraction in [0.001, 0.01, 0.999] {
        let (a, b) = split_by_market_events(&records, fraction).expect("split");
        assert!(count_market(a) >= 1, "in sample is empty at {fraction}");
        assert!(count_market(b) >= 1, "out of sample is empty at {fraction}");
    }
}

#[test]
fn a_recording_with_too_little_market_data_is_refused() {
    // Refusing beats a one-sided split nobody asked for.
    assert!(split_by_market_events(&[], 0.5).is_none());
    assert!(split_by_market_events(&[market(0)], 0.5).is_none());
    let only_decisions: Vec<Record> = (0..10).map(decision).collect();
    assert!(split_by_market_events(&only_decisions, 0.5).is_none());
}

#[test]
fn the_in_sample_side_comes_first() {
    // Order matters to every caller, and getting it backwards would silently
    // train on the future and test on the past.
    let records: Vec<Record> = (0..10).map(market).collect();
    let (a, b) = split_by_market_events(&records, 0.5).expect("split");
    let first_of_b = b
        .iter()
        .find_map(|r| match r.event {
            Event::In(Inbound::Market(m)) => Some(m.exchange_time),
            _ => None,
        })
        .expect("b holds market data");
    let last_of_a = a
        .iter()
        .rev()
        .find_map(|r| match r.event {
            Event::In(Inbound::Market(m)) => Some(m.exchange_time),
            _ => None,
        })
        .expect("a holds market data");
    assert!(
        last_of_a.to_nanos() < first_of_b.to_nanos(),
        "in sample must be the earlier stretch"
    );
}

// ---- the config and the recording must agree -----------------------------

#[test]
fn a_config_for_a_different_instrument_is_refused() {
    // The tick and lot come from the recording, so a config naming another
    // symbol does not fail on its own — it trades the recording's instrument
    // under the config's name and reports the result as if it meant
    // something. Found by pointing an `AAA` config at a `BTCUSD` recording and
    // getting 874 refusals instead of a refusal.
    use config::{InstrumentConfig, SessionConfig};
    use event::codec::{InstrumentEntry, LogHeader};
    use harness::{Costs, backtest};
    use marketdata::MarkRule;
    use risk::LimitBook;
    use types::{Instrument, RoundDir};

    let recorded = Instrument::new(
        I,
        Px::from_scaled(SCALE / 100),
        Qty::from_scaled(SCALE),
        Qty::from_scaled(SCALE),
    )
    .expect("conventions");
    let header = LogHeader::new(
        1,
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(recorded, "BTCUSD").expect("entry")],
    );
    let session = SessionConfig {
        instruments: vec![InstrumentConfig {
            symbol: "AAA".to_string(),
            id: I,
            instrument: recorded,
        }],
        limits: LimitBook::with_instruments(1),
        strategies: Vec::new(),
    };

    let error = backtest(
        &header,
        &[],
        &session,
        Costs::DEFAULT,
        MarkRule::Mid(RoundDir::Down),
    )
    .expect_err("the symbols disagree");
    let text = error.to_string();
    assert!(text.contains("BTCUSD"), "{text}");
    assert!(text.contains("AAA"), "{text}");
}
