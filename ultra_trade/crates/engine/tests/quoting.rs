//! A strategy that rests orders and pulls them.
//!
//! Everything here is about the half of the order lifecycle the crossover
//! never touches. A market order is live and gone inside one event; a resting
//! order has to be named, watched, and cancelled, and each of those is a place
//! the seam between strategy and engine can be wrong.
//!
//! The engine is driven directly rather than through a binary, because what is
//! under test is the seam and not a session.

use engine::{Engine, EngineConfig};
use event::{EngineState, Event, Inbound, MarketEvent, MarketKind, MemoryLog, Outbound, VenueKind};
use oms::OrderState;
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, Queue, SimVenue};
use strategy::Quoter;
use types::{
    ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE, Side, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

fn instrument() -> Instrument {
    Instrument::new(I, Px::from_scaled(SCALE / 100), qty(1), qty(1)).expect("conventions")
}

fn limits() -> LimitBook {
    let mut book = LimitBook::with_instruments(1);
    book.set(
        I,
        Limits {
            max_position: qty(1_000),
            max_exposure: money(10_000_000),
            max_order_notional: money(1_000_000),
            max_orders_in_window: 100_000,
            rate_window: ExchangeSpan::from_nanos(60_000_000_000),
            max_quote_age: ExchangeSpan::from_nanos(600_000_000_000),
        },
    )
    .expect("limits");
    book
}

/// An engine with one quoter on it.
///
/// `half_spread` of 2 against a book one wide means the quotes rest well away
/// from the touch, so nothing fills by accident and the tests are about order
/// management rather than about the fill model.
fn wire(half_spread: i64, reprice: i64, max_inventory: i64) -> Engine<SimVenue, MemoryLog> {
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Queue::Front,
        Fees::NONE,
        ExchangeSpan::from_nanos(0),
    );
    let config = EngineConfig::new(
        vec![instrument()],
        limits(),
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let mut engine = Engine::new(config, venue, MemoryLog::with_capacity(1 << 14));
    engine
        .add_strategy(Box::new(Quoter::new(
            StrategyId::new(0),
            I,
            px(half_spread),
            qty(1),
            px(reprice),
            qty(max_inventory),
        )))
        .expect("strategy");
    engine
}

/// Sends one quote and returns nothing; the engine is the thing under test.
fn quote(engine: &mut Engine<SimVenue, MemoryLog>, at: i64, bid: i64, ask: i64) {
    engine
        .on_inbound(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(at),
            receive_time: Timestamp::from_nanos(at),
            kind: MarketKind::Quote {
                bid_px: px(bid),
                bid_qty: qty(10),
                ask_px: px(ask),
                ask_qty: qty(10),
            },
        }))
        .expect("quote");
}

fn submitted(engine: &Engine<SimVenue, MemoryLog>) -> Vec<(OrderId, Side, Px)> {
    engine
        .log()
        .records()
        .iter()
        .filter_map(|r| match r.event {
            Event::Out(Outbound::OrderSubmitted {
                order, side, kind, ..
            }) => kind.limit_px().map(|px| (order, side, px)),
            _ => None,
        })
        .collect()
}

fn cancels(engine: &Engine<SimVenue, MemoryLog>) -> Vec<OrderId> {
    engine
        .log()
        .records()
        .iter()
        .filter_map(|r| match r.event {
            Event::Out(Outbound::CancelSubmitted { order, .. }) => Some(order),
            _ => None,
        })
        .collect()
}

// ---- resting -------------------------------------------------------------

#[test]
fn a_quoter_rests_a_bid_and_an_ask_around_the_midpoint() {
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102); // mid 101

    let orders = submitted(&engine);
    assert_eq!(orders.len(), 2, "one order each side: {orders:?}");
    let bid = orders
        .iter()
        .find(|(_, s, _)| *s == Side::Buy)
        .expect("bid");
    let ask = orders
        .iter()
        .find(|(_, s, _)| *s == Side::Sell)
        .expect("ask");
    assert_eq!(bid.2, px(99), "mid 101 less a half-spread of 2");
    assert_eq!(ask.2, px(103), "mid 101 plus a half-spread of 2");
}

#[test]
fn the_orders_it_rests_are_limit_orders_that_stay_live() {
    // The point of the whole exercise. A market order would be gone by now.
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102);

    assert_eq!(engine.venue().resting_count(), 2);
    for order in engine.orders().iter() {
        assert_eq!(order.state(), OrderState::Working, "{order:?}");
    }
}

#[test]
fn a_book_that_has_not_moved_does_not_produce_a_second_pair() {
    // Re-posting on every quote would be a strategy that never rests anything
    // and floods the venue.
    let mut engine = wire(2, 1, 100);
    for n in 0..10 {
        quote(&mut engine, 1_000 + n, 100, 102);
    }
    assert_eq!(submitted(&engine).len(), 2);
    assert!(cancels(&engine).is_empty());
}

// ---- repricing -----------------------------------------------------------

#[test]
fn a_book_that_moves_far_enough_gets_the_resting_orders_pulled() {
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102); // mid 101
    let before = submitted(&engine);

    quote(&mut engine, 2_000, 110, 112); // mid 111, well past the reprice band
    let pulled = cancels(&engine);
    assert_eq!(pulled.len(), 2, "both sides pulled: {pulled:?}");
    for (order, _, _) in &before {
        assert!(pulled.contains(order), "{order} should have been cancelled");
    }
}

#[test]
fn a_pulled_quote_is_reposted_only_after_the_venue_confirms_it_is_gone() {
    // Posting the replacement before the cancel lands would leave two orders
    // resting on one side, and a market that took both would hand the strategy
    // double the size it ever intended.
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102);
    assert_eq!(engine.venue().resting_count(), 2);

    // The move pulls both sides. Nothing replaces them in this event: the
    // strategy waits to be told the orders are gone, and it does not act on a
    // midpoint it has already decided is stale.
    quote(&mut engine, 2_000, 110, 112);
    assert_eq!(submitted(&engine).len(), 2, "no replacement yet");
    assert_eq!(
        engine.venue().resting_count(),
        0,
        "and the old pair is gone"
    );

    // The next quote is where it comes back, at the new midpoint.
    quote(&mut engine, 3_000, 110, 112);
    let orders = submitted(&engine);
    assert_eq!(orders.len(), 4, "two pairs in total: {orders:?}");
    assert_eq!(
        engine.venue().resting_count(),
        2,
        "one live pair, never two"
    );

    let live: Vec<Px> = engine
        .orders()
        .iter()
        .filter(|o| o.state().is_live())
        .filter_map(|o| o.kind().limit_px())
        .collect();
    assert!(live.contains(&px(109)), "new bid at mid 111 - 2: {live:?}");
    assert!(live.contains(&px(113)), "new ask at mid 111 + 2: {live:?}");
}

#[test]
fn one_side_never_has_two_orders_resting_at_once() {
    // The property the cancel-then-wait dance exists to protect. A market that
    // took both would hand the strategy double the size it ever intended.
    let mut engine = wire(2, 1, 100);
    for n in 0..40i64 {
        let level = 100 + (n % 7) * 4;
        quote(&mut engine, 1_000 + n, level, level + 2);
        assert!(
            engine.venue().resting_count() <= 2,
            "step {n}: {} resting",
            engine.venue().resting_count()
        );
        let per_side = |want: Side| {
            engine
                .orders()
                .iter()
                .filter(|o| o.state() == OrderState::Working && o.side() == want)
                .count()
        };
        assert!(per_side(Side::Buy) <= 1, "step {n}: two bids resting");
        assert!(per_side(Side::Sell) <= 1, "step {n}: two asks resting");
    }
}

#[test]
fn a_flicker_smaller_than_the_reprice_band_is_ignored() {
    // Without a band, a one-tick wobble would cancel and re-post for ever and
    // the strategy would spend the session waiting for its own round trips.
    let mut engine = wire(2, 5, 100);
    quote(&mut engine, 1_000, 100, 102); // mid 101
    quote(&mut engine, 2_000, 101, 103); // mid 102, drift of 1 < band of 5
    assert!(
        cancels(&engine).is_empty(),
        "a small move should not cost a round trip"
    );
    assert_eq!(submitted(&engine).len(), 2);
}

// ---- inventory -----------------------------------------------------------

#[test]
fn it_stops_adding_on_the_side_that_would_break_its_inventory_cap() {
    // A cap of zero means it may never take on a position, so with nothing
    // held it must not quote either side.
    let mut engine = wire(2, 1, 0);
    quote(&mut engine, 1_000, 100, 102);
    assert!(
        submitted(&engine).is_empty(),
        "an inventory cap of zero leaves nothing to quote with"
    );
}

// ---- the seam ------------------------------------------------------------

#[test]
fn a_cancel_a_strategy_asks_for_is_recorded_as_its_own_decision() {
    // "Who pulled this quote" has to be answerable from the log, which is why
    // a strategy's cancel is not folded into the operator's flatten sweep.
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102);
    quote(&mut engine, 2_000, 110, 112);

    let has_cancel = engine
        .log()
        .records()
        .iter()
        .any(|r| matches!(r.event, Event::Out(Outbound::CancelSubmitted { .. })));
    assert!(has_cancel, "the cancel must be in the log");
}

#[test]
fn cancelling_an_order_that_has_already_gone_is_not_an_error() {
    // A fill and a cancel can cross. A strategy that had to win that race
    // would be wrong occasionally rather than never.
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102);
    let orders = submitted(&engine);
    let (bid, _, _) = orders
        .iter()
        .find(|(_, s, _)| *s == Side::Buy)
        .copied()
        .expect("bid");

    // Take the resting bid out with a trade through it, then move the book so
    // the strategy tries to pull an order that is already filled.
    engine
        .on_inbound(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(1_500),
            receive_time: Timestamp::from_nanos(1_500),
            kind: MarketKind::Trade {
                px: px(98),
                qty: qty(5),
                aggressor: Side::Sell,
            },
        }))
        .expect("trade");
    assert_eq!(
        engine.orders().get(bid).expect("order").state(),
        OrderState::Filled
    );

    quote(&mut engine, 2_000, 110, 112);
    assert_eq!(engine.state(), EngineState::Running, "no halt, no error");
}

#[test]
fn a_refused_intent_frees_the_side_rather_than_wedging_it() {
    // Without this the strategy would sit waiting for an acknowledgement that
    // is never coming, and would stop quoting that side for the session.
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102);
    assert_eq!(submitted(&engine).len(), 2);

    // Halt, so the next intents are refused by the gate rather than filled.
    engine
        .on_inbound(Inbound::Command(event::CommandEvent {
            command: event::Command::Halt,
            receive_time: Timestamp::from_nanos(1_100),
        }))
        .expect("halt");
    quote(&mut engine, 2_000, 110, 112); // pulls, then is refused on re-post
    quote(&mut engine, 3_000, 110, 112);

    let refusals = engine
        .log()
        .records()
        .iter()
        .filter(|r| matches!(r.event, Event::Out(Outbound::IntentRejected { .. })))
        .count();
    assert!(refusals > 0, "the halt should have refused the re-post");

    // Resume, and it must be quoting again rather than stuck.
    engine
        .on_inbound(Inbound::Command(event::CommandEvent {
            command: event::Command::Resume,
            receive_time: Timestamp::from_nanos(3_100),
        }))
        .expect("resume");
    let before = submitted(&engine).len();
    quote(&mut engine, 4_000, 120, 122);
    assert!(
        submitted(&engine).len() > before,
        "a refused side must quote again once it can"
    );
}

#[test]
fn a_fill_on_a_resting_order_moves_the_position() {
    let mut engine = wire(2, 1, 100);
    quote(&mut engine, 1_000, 100, 102);
    engine
        .on_inbound(Inbound::Market(MarketEvent {
            instrument: I,
            exchange_time: Timestamp::from_nanos(1_500),
            receive_time: Timestamp::from_nanos(1_500),
            kind: MarketKind::Trade {
                px: px(98),
                qty: qty(5),
                aggressor: Side::Sell,
            },
        }))
        .expect("trade");

    let held = engine.positions().get(I).expect("position").qty();
    assert_eq!(held, qty(1), "the resting bid was hit");

    let filled = engine
        .log()
        .records()
        .iter()
        .any(|r| matches!(r.event, Event::In(Inbound::Venue(v)) if matches!(v.kind, VenueKind::Filled { .. })));
    assert!(filled, "the fill must be in the log");
}
