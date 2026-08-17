//! The order book below the touch, and the rule that nothing sees half of one.
//!
//! A book update arrives as a run of level records. Between the first and the
//! last the book can be crossed — a state the venue never published — so the
//! interesting tests here are the ones about what is *not* visible partway
//! through (ADR, order-book depth D-2).

use event::{Inbound, MarketEvent, MarketKind};
use marketdata::{Applied, Books};
use types::{InstrumentId, Px, Qty, SCALE, Side, Timestamp};

const I: InstrumentId = InstrumentId::new(0);

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn level(at: i64, side: Side, price: i64, size: i64) -> MarketEvent {
    MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::Level {
            side,
            px: px(price),
            qty: qty(size),
        },
    }
}

fn removal(at: i64, side: Side, price: i64) -> MarketEvent {
    MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::Level {
            side,
            px: px(price),
            qty: Qty::ZERO,
        },
    }
}

fn applied(at: i64) -> MarketEvent {
    MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::BookApplied,
    }
}

/// Applies a whole update and asserts every record was accepted.
fn update(books: &mut Books, at: i64, levels: &[MarketEvent]) {
    for event in levels {
        assert_eq!(books.apply(event), Applied::Accepted, "{event:?}");
    }
    assert_eq!(books.apply(&applied(at)), Applied::Accepted);
}

fn prices(books: &Books, side: Side) -> Vec<i64> {
    books
        .depth(I, side)
        .expect("side")
        .levels()
        .iter()
        .map(|l| l.px.to_scaled() / SCALE)
        .collect()
}

// ---- the book ------------------------------------------------------------

#[test]
fn levels_are_kept_best_first_on_both_sides() {
    // Bids descend and asks ascend. Getting this backwards would make a fill
    // model walk from the worst price inwards and under-report every cost.
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[
            level(10, Side::Buy, 99, 1),
            level(10, Side::Buy, 101, 1),
            level(10, Side::Buy, 100, 1),
            level(10, Side::Sell, 105, 1),
            level(10, Side::Sell, 103, 1),
            level(10, Side::Sell, 104, 1),
        ],
    );
    assert_eq!(prices(&books, Side::Buy), vec![101, 100, 99]);
    assert_eq!(prices(&books, Side::Sell), vec![103, 104, 105]);
}

#[test]
fn a_zero_quantity_removes_the_level() {
    // How the venue says it, so it is how this says it.
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[
            level(10, Side::Buy, 100, 1),
            level(10, Side::Buy, 99, 1),
            level(10, Side::Sell, 101, 1),
        ],
    );
    update(&mut books, 20, &[removal(20, Side::Buy, 100)]);
    assert_eq!(prices(&books, Side::Buy), vec![99]);
}

#[test]
fn a_level_at_a_price_that_is_already_there_replaces_its_size() {
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[level(10, Side::Buy, 100, 1), level(10, Side::Sell, 101, 1)],
    );
    update(&mut books, 20, &[level(20, Side::Buy, 100, 7)]);
    let side = books.depth(I, Side::Buy).expect("bids");
    assert_eq!(side.levels().len(), 1, "replaced, not appended");
    assert_eq!(side.best().expect("best").qty, qty(7));
}

#[test]
fn the_top_of_book_follows_from_the_depth() {
    // A depth feed and a quote feed must leave `top()` meaning the same thing,
    // or every consumer needs to know which kind of feed it is behind (D-3).
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[
            level(10, Side::Buy, 100, 3),
            level(10, Side::Buy, 99, 5),
            level(10, Side::Sell, 101, 2),
            level(10, Side::Sell, 102, 8),
        ],
    );
    let top = books.top(I).expect("top");
    assert_eq!(top.bid_px, px(100));
    assert_eq!(top.bid_qty, qty(3));
    assert_eq!(top.ask_px, px(101));
    assert_eq!(top.ask_qty, qty(2));
}

// ---- nothing sees half an update -----------------------------------------

#[test]
fn a_level_on_its_own_does_not_move_the_book() {
    // The rule the completion marker exists for.
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[level(10, Side::Buy, 100, 1), level(10, Side::Sell, 101, 1)],
    );

    // Start an update that would cross the book if applied one level at a time.
    assert_eq!(
        books.apply(&level(20, Side::Buy, 105, 1)),
        Applied::Accepted
    );
    let top = books.top(I).expect("top");
    assert_eq!(
        top.bid_px,
        px(100),
        "the book must not move until the update completes"
    );
    assert!(!top.is_crossed());

    // Complete it with the matching ask move. A real book move is a delta:
    // the old ask is removed and a new one added, in the same update.
    assert_eq!(
        books.apply(&removal(20, Side::Sell, 101)),
        Applied::Accepted
    );
    assert_eq!(
        books.apply(&level(20, Side::Sell, 106, 1)),
        Applied::Accepted
    );
    assert_eq!(books.apply(&applied(20)), Applied::Accepted);
    let top = books.top(I).expect("top");
    assert_eq!(top.bid_px, px(105));
    assert_eq!(top.ask_px, px(106));
    assert!(!top.is_crossed(), "and it is never crossed on the way");
}

#[test]
fn an_update_too_large_for_the_buffer_is_refused_whole() {
    // Half a book applied is worse than none: a fill model walking it would
    // under-fill for a reason nothing in the log explains.
    let mut books = Books::with_depth(1, 16, 2);
    update(
        &mut books,
        10,
        &[level(10, Side::Buy, 100, 1), level(10, Side::Sell, 101, 1)],
    );

    assert_eq!(books.apply(&level(20, Side::Buy, 90, 1)), Applied::Accepted);
    assert_eq!(books.apply(&level(20, Side::Buy, 91, 1)), Applied::Accepted);
    assert_eq!(
        books.apply(&level(20, Side::Buy, 92, 1)),
        Applied::UpdateTooLarge,
        "the third level overflows a buffer of two"
    );
    assert_eq!(books.overflowed_updates(), 1);

    // And the book is untouched by the part that did arrive.
    assert_eq!(prices(&books, Side::Buy), vec![100]);
}

#[test]
fn a_full_side_refuses_a_new_price_rather_than_dropping_the_worst() {
    // Silently trimming would make the book look thinner than the venue said.
    let mut books = Books::with_depth(1, 2, 64);
    update(
        &mut books,
        10,
        &[
            level(10, Side::Buy, 100, 1),
            level(10, Side::Buy, 99, 1),
            level(10, Side::Sell, 101, 1),
        ],
    );
    update(&mut books, 20, &[level(20, Side::Buy, 98, 1)]);
    assert_eq!(prices(&books, Side::Buy), vec![100, 99], "unchanged");
    assert_eq!(books.overflowed_updates(), 1, "and it is counted");
}

#[test]
fn a_book_update_stamped_earlier_than_the_book_is_refused() {
    // Same rule depth inherits from quotes: a reordering feed must not move the
    // book backwards.
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        20,
        &[level(20, Side::Buy, 100, 1), level(20, Side::Sell, 101, 1)],
    );
    assert_eq!(books.apply(&level(10, Side::Buy, 90, 1)), Applied::Accepted);
    assert!(matches!(
        books.apply(&applied(10)),
        Applied::OutOfOrder { .. }
    ));
    assert_eq!(prices(&books, Side::Buy), vec![100], "still the newer book");
}

#[test]
fn depth_is_empty_until_a_depth_feed_arrives() {
    // A top-of-book session must not look like a session with a one-sided book.
    let mut books = Books::with_instruments(1);
    let quote = MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(10),
        receive_time: Timestamp::from_nanos(10),
        kind: MarketKind::Quote {
            bid_px: px(100),
            bid_qty: qty(1),
            ask_px: px(101),
            ask_qty: qty(1),
        },
    };
    assert_eq!(books.apply(&quote), Applied::Accepted);
    assert!(books.top(I).is_some(), "the quote still sets the top");
    assert!(books.depth(I, Side::Buy).expect("bids").is_empty());
    assert!(books.depth(I, Side::Sell).expect("asks").is_empty());
}

#[test]
fn an_unknown_instrument_is_refused_for_a_level_too() {
    let mut books = Books::with_instruments(1);
    let stray = MarketEvent {
        instrument: InstrumentId::new(7),
        ..level(10, Side::Buy, 100, 1)
    };
    assert_eq!(books.apply(&stray), Applied::UnknownInstrument);
}

/// A level is an `Inbound` like any other, so the alphabet stays uniform.
#[test]
fn a_level_carries_both_clocks_like_every_other_input() {
    let event = Inbound::Market(level(42, Side::Buy, 100, 1));
    assert_eq!(event.receive_time().to_nanos(), 42);
    assert_eq!(
        event.exchange_time().expect("venue stamped it").to_nanos(),
        42
    );
}

// ---- throwing a wrong book away ------------------------------------------

fn reset(at: i64) -> MarketEvent {
    MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind: MarketKind::BookReset,
    }
}

#[test]
fn a_reset_leaves_no_level_behind() {
    // The point of it. A depth feed is deltas, so a book that is wrong stays
    // wrong for ever unless something throws it away — and a level that
    // survives the reset is exactly the one that will still be wrong after.
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[
            level(10, Side::Buy, 100, 1),
            level(10, Side::Buy, 99, 1),
            level(10, Side::Sell, 101, 1),
        ],
    );
    assert_eq!(books.apply(&reset(20)), Applied::Accepted);

    assert!(books.depth(I, Side::Buy).expect("bids").is_empty());
    assert!(books.depth(I, Side::Sell).expect("asks").is_empty());
    assert!(books.top(I).is_none(), "and no stale top of book either");
    assert_eq!(books.book_resets(), 1);
}

#[test]
fn a_reset_discards_a_half_built_update_too() {
    // Those levels belong to the book being thrown away. Keeping them would
    // apply part of a bad update on top of the fresh snapshot.
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[level(10, Side::Buy, 100, 1), level(10, Side::Sell, 101, 1)],
    );
    assert_eq!(
        books.apply(&level(20, Side::Buy, 105, 7)),
        Applied::Accepted
    );
    assert_eq!(books.apply(&reset(20)), Applied::Accepted);

    update(
        &mut books,
        30,
        &[level(30, Side::Buy, 50, 1), level(30, Side::Sell, 51, 1)],
    );
    assert_eq!(
        prices(&books, Side::Buy),
        vec![50],
        "the abandoned level must not reappear"
    );
}

#[test]
fn a_snapshot_after_a_reset_rebuilds_the_book() {
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        10,
        &[level(10, Side::Buy, 100, 1), level(10, Side::Sell, 101, 1)],
    );
    assert_eq!(books.apply(&reset(20)), Applied::Accepted);
    update(
        &mut books,
        20,
        &[level(20, Side::Buy, 200, 2), level(20, Side::Sell, 201, 3)],
    );
    let top = books.top(I).expect("rebuilt");
    assert_eq!(top.bid_px, px(200));
    assert_eq!(top.ask_px, px(201));
}

#[test]
fn a_reset_is_not_refused_for_arriving_out_of_order() {
    // The book being discarded is the one whose timestamps are not evidence of
    // anything. Refusing the reset would leave the wrong book in place, which
    // is the one outcome that must not happen.
    let mut books = Books::with_instruments(1);
    update(
        &mut books,
        100,
        &[
            level(100, Side::Buy, 100, 1),
            level(100, Side::Sell, 101, 1),
        ],
    );
    assert_eq!(books.apply(&reset(1)), Applied::Accepted);
    assert!(books.depth(I, Side::Buy).expect("bids").is_empty());
}

#[test]
fn resets_are_counted_so_a_session_can_say_it_happened() {
    // Zero is the number to expect. Anything else means some earlier decisions
    // were made against a book that did not match the venue's.
    let mut books = Books::with_instruments(1);
    assert_eq!(books.book_resets(), 0);
    for at in [10, 20, 30] {
        assert_eq!(books.apply(&reset(at)), Applied::Accepted);
    }
    assert_eq!(books.book_resets(), 3);
}
