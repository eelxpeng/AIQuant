//! What size costs, once the book below the touch is known.
//!
//! The point of depth. `TouchDisplayed` fills an order up to the size at the
//! touch and gives up on the rest, because the recording did not say what was
//! behind it. `WalkBook` eats levels outwards and pays each one's own price,
//! which is the first time a backtest can answer "what would trading that much
//! have cost".

use event::{Inbound, MarketEvent, MarketKind, OrderKind, VenueKind};
use oms::{Order, VenueAdapter};
use sim_venue::{Fees, FillModel, SimVenue};
use types::{ExchangeSpan, InstrumentId, OrderId, Px, Qty, SCALE, Side, StrategyId, Timestamp};

const I: InstrumentId = InstrumentId::new(0);

fn px(whole: i64) -> Px {
    Px::from_scaled(whole * SCALE)
}

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn venue(model: FillModel) -> SimVenue {
    SimVenue::new(1, model, Fees::NONE, ExchangeSpan::from_nanos(0))
}

fn market(at: i64, kind: MarketKind) -> MarketEvent {
    MarketEvent {
        instrument: I,
        exchange_time: Timestamp::from_nanos(at),
        receive_time: Timestamp::from_nanos(at),
        kind,
    }
}

/// An ask ladder: 2 at 101, 3 at 102, 5 at 103. Bids just so the book is sane.
fn ladder(v: &mut SimVenue) {
    for (side, price, size) in [
        (Side::Buy, 100, 4),
        (Side::Buy, 99, 9),
        (Side::Sell, 101, 2),
        (Side::Sell, 102, 3),
        (Side::Sell, 103, 5),
    ] {
        v.observe_market(&market(
            10,
            MarketKind::Level {
                side,
                px: px(price),
                qty: qty(size),
            },
        ));
    }
    v.observe_market(&market(10, MarketKind::BookApplied));
}

fn buy(id: u64, size: i64, kind: OrderKind) -> Order {
    Order::new(
        OrderId::new(id),
        StrategyId::new(0),
        I,
        Side::Buy,
        qty(size),
        kind,
        false,
        Timestamp::from_nanos(10),
    )
}

/// Every fill the venue reported, as (price, quantity) in whole units.
fn fills(v: &mut SimVenue) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    v.drain(&mut out);
    out.iter()
        .filter_map(|e| match e {
            Inbound::Venue(report) => match report.kind {
                VenueKind::Filled { px, qty, .. } => {
                    Some((px.to_scaled() / SCALE, qty.to_scaled() / SCALE))
                }
                _ => None,
            },
            _ => None,
        })
        .collect()
}

// ---- the cost of size ----------------------------------------------------

#[test]
fn an_order_larger_than_the_touch_pays_for_the_levels_it_eats() {
    // 7 units against 2@101, 3@102, 5@103. It should take 2, 3, then 2.
    let mut v = venue(FillModel::WalkBook);
    ladder(&mut v);
    v.submit(&buy(0, 7, OrderKind::Market)).expect("submit");
    assert_eq!(fills(&mut v), vec![(101, 2), (102, 3), (103, 2)]);
}

#[test]
fn the_same_order_under_the_old_model_stops_at_the_touch() {
    // The comparison that says why this exists. Five of the seven units simply
    // do not fill, and nothing in the result says what they would have cost.
    let mut v = venue(FillModel::TouchDisplayed);
    ladder(&mut v);
    v.submit(&buy(0, 7, OrderKind::Market)).expect("submit");
    assert_eq!(fills(&mut v), vec![(101, 2)]);
}

#[test]
fn an_order_inside_the_touch_is_unaffected_by_depth() {
    // Depth must not change what was already right, or every existing result
    // moves for no reason.
    let mut walk = venue(FillModel::WalkBook);
    let mut touch = venue(FillModel::TouchDisplayed);
    ladder(&mut walk);
    ladder(&mut touch);
    walk.submit(&buy(0, 1, OrderKind::Market)).expect("submit");
    touch.submit(&buy(0, 1, OrderKind::Market)).expect("submit");
    // Captured once each: draining is destructive.
    let walked = fills(&mut walk);
    let touched = fills(&mut touch);
    assert_eq!(walked, vec![(101, 1)]);
    assert_eq!(walked, touched);
}

#[test]
fn a_limit_order_stops_at_its_own_price_rather_than_paying_through_it() {
    // The whole point of a limit. It may have 2@101 and 3@102 but not 103.
    let mut v = venue(FillModel::WalkBook);
    ladder(&mut v);
    v.submit(&buy(0, 7, OrderKind::Limit(px(102))))
        .expect("submit");
    assert_eq!(fills(&mut v), vec![(101, 2), (102, 3)]);
}

#[test]
fn an_order_bigger_than_the_whole_book_takes_what_there_is() {
    // 20 units against 10 on the book. No level is invented for the remaining
    // ten; they are simply not filled.
    let mut v = venue(FillModel::WalkBook);
    ladder(&mut v);
    v.submit(&buy(0, 20, OrderKind::Market)).expect("submit");
    assert_eq!(fills(&mut v), vec![(101, 2), (102, 3), (103, 5)]);
}

#[test]
fn selling_walks_the_bids_downwards() {
    // The mirror. Bids are 4@100 then 9@99, so six units take 4 then 2.
    let mut v = venue(FillModel::WalkBook);
    ladder(&mut v);
    let sell = Order::new(
        OrderId::new(0),
        StrategyId::new(0),
        I,
        Side::Sell,
        qty(6),
        OrderKind::Market,
        false,
        Timestamp::from_nanos(10),
    );
    v.submit(&sell).expect("submit");
    assert_eq!(fills(&mut v), vec![(100, 4), (99, 2)]);
}

// ---- what it refuses to invent -------------------------------------------

#[test]
fn with_no_depth_recorded_it_behaves_exactly_like_the_touch_model() {
    // A recording from before depth existed must still backtest, and it must
    // not gain liquidity it never had just because the model changed.
    let quote = market(
        10,
        MarketKind::Quote {
            bid_px: px(100),
            bid_qty: qty(4),
            ask_px: px(101),
            ask_qty: qty(2),
        },
    );
    let mut walk = venue(FillModel::WalkBook);
    let mut touch = venue(FillModel::TouchDisplayed);
    walk.observe_market(&quote);
    touch.observe_market(&quote);
    walk.submit(&buy(0, 7, OrderKind::Market)).expect("submit");
    touch.submit(&buy(0, 7, OrderKind::Market)).expect("submit");

    assert_eq!(fills(&mut walk), vec![(101, 2)], "the touch and no further");
    assert_eq!(fills(&mut touch), vec![(101, 2)]);
}

#[test]
fn a_limit_that_does_not_reach_the_touch_fills_nothing() {
    let mut v = venue(FillModel::WalkBook);
    ladder(&mut v);
    v.submit(&buy(0, 7, OrderKind::Limit(px(100))))
        .expect("submit");
    assert!(
        fills(&mut v).is_empty(),
        "a bid below the ask takes nothing"
    );
}
