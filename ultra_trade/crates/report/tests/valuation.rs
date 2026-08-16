//! What an open position is worth, and refusing to guess when nothing says.
//!
//! Every session so far has ended holding something, and until now the report
//! said only what had been *realized*. That is not a result: a strategy that
//! closed out at a small profit and one that is sitting on a large loss can
//! print the same realized number, and ranking them on it gets the order
//! wrong.
//!
//! The refusals matter most here. A position with no usable mark must be
//! reported as unmarked, never valued at zero — zero is a number someone will
//! add up, and a missing mark is not worth nothing.

use event::{
    Event, FORMAT_VERSION, Inbound, MarketEvent, MarketKind, Outbound, Record, Seq, VenueEvent,
    VenueKind,
};
use marketdata::MarkRule;
use report::summarize;
use types::{
    ExchangeTime, InstrumentId, Notional, OrderId, Px, Qty, RoundDir, SCALE, Side, StrategyId,
    Timestamp,
};

const A: InstrumentId = InstrumentId::new(0);
const B: InstrumentId = InstrumentId::new(1);
const MID: MarkRule = MarkRule::Mid(RoundDir::Down);

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

fn at(n: i64) -> ExchangeTime {
    Timestamp::from_nanos(n)
}

fn record(seq: u64, event: Event) -> Record {
    Record {
        seq: Seq::new(seq),
        version: FORMAT_VERSION,
        event,
    }
}

fn quote(seq: u64, instrument: InstrumentId, bid: i64, ask: i64) -> Record {
    record(
        seq,
        Event::In(Inbound::Market(MarketEvent {
            instrument,
            exchange_time: at(seq as i64),
            receive_time: Timestamp::from_nanos(seq as i64),
            kind: MarketKind::Quote {
                bid_px: px(bid),
                bid_qty: qty(10),
                ask_px: px(ask),
                ask_qty: qty(10),
            },
        })),
    )
}

/// A quote whose two sides are the same size but one of them is empty, which
/// is a book with no honest midpoint.
fn one_sided(seq: u64, instrument: InstrumentId, bid: i64) -> Record {
    record(
        seq,
        Event::In(Inbound::Market(MarketEvent {
            instrument,
            exchange_time: at(seq as i64),
            receive_time: Timestamp::from_nanos(seq as i64),
            kind: MarketKind::Quote {
                bid_px: px(bid),
                bid_qty: qty(10),
                ask_px: px(bid + 1),
                ask_qty: Qty::ZERO,
            },
        })),
    )
}

fn trade(seq: u64, instrument: InstrumentId, price: i64) -> Record {
    record(
        seq,
        Event::In(Inbound::Market(MarketEvent {
            instrument,
            exchange_time: at(seq as i64),
            receive_time: Timestamp::from_nanos(seq as i64),
            kind: MarketKind::Trade {
                px: px(price),
                qty: qty(1),
                aggressor: Side::Buy,
            },
        })),
    )
}

fn submitted(seq: u64, order: u64, instrument: InstrumentId, side: Side) -> Record {
    record(
        seq,
        Event::Out(Outbound::OrderSubmitted {
            caused_by: Seq::new(0),
            order: OrderId::new(order),
            strategy: StrategyId::new(0),
            instrument,
            side,
            qty: qty(10),
            kind: event::OrderKind::Market,
            reduce_only: false,
        }),
    )
}

fn filled(seq: u64, order: u64, price: i64, size: i64) -> Record {
    record(
        seq,
        Event::In(Inbound::Venue(VenueEvent {
            order: OrderId::new(order),
            venue_time: at(seq as i64),
            receive_time: Timestamp::from_nanos(seq as i64),
            kind: VenueKind::Filled {
                px: px(price),
                qty: qty(size),
                fee: Notional::ZERO,
            },
        })),
    )
}

// ---- the arithmetic ------------------------------------------------------

#[test]
fn a_session_that_ends_flat_has_nothing_unrealized() {
    // Bought 10 at 100, sold 10 at 110. Realized 100, holding nothing.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        submitted(3, 1, A, Side::Sell),
        filled(4, 1, 110, 10),
        quote(5, A, 109, 111),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(r.realized, money(100));
    assert_eq!(r.valuation.unrealized, Notional::ZERO);
    assert_eq!(r.valuation.total, r.realized, "total is realized when flat");
}

#[test]
fn a_long_left_open_is_valued_at_the_mark() {
    // Bought 10 at 100 and kept it. The book ends 109/111, so the mid is 110
    // and the position is worth 10 x 10 = 100 more than it cost.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        quote(3, A, 109, 111),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(r.realized, Notional::ZERO, "nothing was closed");
    assert_eq!(r.valuation.unrealized, money(100));
    assert_eq!(r.valuation.total, money(100));
}

#[test]
fn a_short_left_open_is_valued_the_other_way() {
    // Sold 10 at 100 with the book ending at a mid of 110: a loss of 100.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Sell),
        filled(2, 0, 100, 10),
        quote(3, A, 109, 111),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(r.valuation.unrealized, money(-100));
    assert_eq!(r.valuation.total, money(-100));
}

#[test]
fn realized_and_unrealized_always_add_back_to_the_total() {
    // The property that keeps the three numbers from drifting apart. Bought 20
    // at 100, sold 10 at 110, still long 10, book ends at a mid of 120.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 20),
        submitted(3, 1, A, Side::Sell),
        filled(4, 1, 110, 10),
        quote(5, A, 119, 121),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(r.realized, money(100), "10 closed at +10 each");
    assert_eq!(r.valuation.unrealized, money(200), "10 open at +20 each");
    assert_eq!(r.valuation.total, money(300));
    assert_eq!(
        r.valuation.total,
        r.realized + r.valuation.unrealized,
        "the three must reconcile"
    );
}

#[test]
fn fees_are_already_inside_the_total() {
    // A fee is cash gone. If it were left out of the mark-to-market figure the
    // total would flatter every session that paid one.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        record(
            2,
            Event::In(Inbound::Venue(VenueEvent {
                order: OrderId::new(0),
                venue_time: at(2),
                receive_time: Timestamp::from_nanos(2),
                kind: VenueKind::Filled {
                    px: px(100),
                    qty: qty(10),
                    fee: money(7),
                },
            })),
        ),
        quote(3, A, 99, 101),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(r.fees, money(7));
    assert_eq!(
        r.valuation.total,
        money(-7),
        "bought at the mid, so the fee is the whole result"
    );
}

// ---- the mark itself -----------------------------------------------------

#[test]
fn the_mark_rule_is_the_callers_and_changes_the_answer() {
    // Same log, two rules, two different valuations. Which one was used has to
    // be a decision somebody made, not a default hidden in here.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        trade(3, A, 105),
        quote(4, A, 119, 121),
    ];
    let mid = summarize(&log, 1, MID).expect("summary");
    let last = summarize(&log, 1, MarkRule::LastTrade).expect("summary");

    assert_eq!(mid.valuation.unrealized, money(200), "mid of 120");
    assert_eq!(last.valuation.unrealized, money(50), "last trade of 105");
    assert_eq!(mid.valuation.rule, MID);
    assert_eq!(last.valuation.rule, MarkRule::LastTrade);
}

#[test]
fn the_valuation_says_what_it_marked_at_and_when() {
    // "Marked at what, as of when" is the difference between a number and a
    // number somebody can check.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        quote(7, A, 109, 111),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    let mark = r.valuation.marks[0].expect("A has a mark");
    assert_eq!(mark.px, px(110));
    assert_eq!(mark.at, at(7), "the last quote, not the last record");
}

// ---- the refusals --------------------------------------------------------

#[test]
fn a_position_with_no_mark_is_reported_unmarked_rather_than_valued_at_zero() {
    // The one that matters. Zero is a number someone will add up; a missing
    // mark is not worth nothing (Constitution V).
    let log = vec![
        submitted(0, 0, A, Side::Buy),
        filled(1, 0, 100, 10), // filled with no market data ever recorded
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert!(r.valuation.marks[0].is_none());
    assert_eq!(
        r.valuation.unmarked,
        vec![A],
        "it must be named, not silently skipped"
    );
    assert_eq!(
        r.valuation.unrealized,
        Notional::ZERO,
        "nothing is added for it"
    );
    assert!(
        !r.valuation.is_complete(),
        "and the total must not be presented as whole"
    );
}

#[test]
fn a_book_with_no_honest_midpoint_does_not_produce_one() {
    let log = vec![
        one_sided(0, A, 99),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        one_sided(3, A, 99),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert!(r.valuation.marks[0].is_none());
    assert_eq!(r.valuation.unmarked, vec![A]);
}

#[test]
fn an_instrument_that_is_flat_and_unmarked_is_not_complained_about() {
    // Only a position needs a mark. An instrument the session never traded has
    // nothing to value, and listing it would train people to ignore the list.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        quote(3, A, 109, 111),
    ];
    let r = summarize(&log, 2, MID).expect("summary");
    assert!(r.valuation.marks[1].is_none(), "B never quoted");
    assert!(
        r.valuation.unmarked.is_empty(),
        "but B is flat, so nothing is missing"
    );
    assert!(r.valuation.is_complete());
}

#[test]
fn each_instrument_is_valued_at_its_own_mark() {
    let log = vec![
        quote(0, A, 99, 101),
        quote(1, B, 199, 201),
        submitted(2, 0, A, Side::Buy),
        filled(3, 0, 100, 10),
        submitted(4, 1, B, Side::Buy),
        filled(5, 1, 200, 10),
        quote(6, A, 109, 111), // A mid 110, +10 each
        quote(7, B, 189, 191), // B mid 190, -10 each
    ];
    let r = summarize(&log, 2, MID).expect("summary");
    assert_eq!(r.valuation.marks[0].expect("A").px, px(110));
    assert_eq!(r.valuation.marks[1].expect("B").px, px(190));
    assert_eq!(
        r.valuation.unrealized,
        Notional::ZERO,
        "+100 on A and -100 on B"
    );
    assert!(r.valuation.is_complete());
}

#[test]
fn an_out_of_order_quote_does_not_become_the_mark() {
    // The book refused it during the session, so a report that valued against
    // it would disagree with what the session itself believed.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        quote(9, A, 109, 111),
        // Same instrument, an earlier exchange time: refused by the book.
        record(
            10,
            Event::In(Inbound::Market(MarketEvent {
                instrument: A,
                exchange_time: at(4),
                receive_time: Timestamp::from_nanos(10),
                kind: MarketKind::Quote {
                    bid_px: px(1),
                    bid_qty: qty(10),
                    ask_px: px(3),
                    ask_qty: qty(10),
                },
            })),
        ),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(
        r.valuation.marks[0].expect("A").px,
        px(110),
        "the stale quote must not win"
    );
}

// ---- drawdown ------------------------------------------------------------

#[test]
fn a_drawdown_ridden_out_in_an_open_position_is_counted() {
    // The case realized-only missed entirely, and it is the common one: a
    // strategy that is down usually still holds the thing it is down on.
    // Bought 10 at 100, the mid fell to 80, then recovered to 100. Nothing
    // traded on the way, so realized never moved.
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        quote(3, A, 79, 81),  // mid 80 — down 200
        quote(4, A, 99, 101), // back to 100
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(r.realized, Notional::ZERO, "nothing was ever closed");
    assert_eq!(
        r.max_drawdown,
        money(200),
        "the fall has to show even though no fill recorded it"
    );
}

#[test]
fn the_drawdown_of_a_session_that_never_lost_is_zero() {
    let log = vec![
        quote(0, A, 99, 101),
        submitted(1, 0, A, Side::Buy),
        filled(2, 0, 100, 10),
        quote(3, A, 109, 111),
        quote(4, A, 119, 121),
    ];
    let r = summarize(&log, 1, MID).expect("summary");
    assert_eq!(r.max_drawdown, Notional::ZERO);
}
