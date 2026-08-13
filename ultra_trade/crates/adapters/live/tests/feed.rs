//! The live feed: the protocol, the clock, and the merge of market data,
//! operator commands, and timers.

use engine::FeedAdapter;
use event::{Command, Inbound, MarketKind, TimerToken};
use live_feed::{
    FeedError, LiveFeed, ManualClock, Symbols, commands_from, parse_line, project_exchange_time,
};
use std::time::Duration;
use strategy::TimerRequest;
use types::{
    Clock, DecimalError, ExchangeTime, InstrumentId, Px, Qty, ReceiveTime, Side, StrategyId,
    Timestamp,
};

const I: InstrumentId = InstrumentId::new(0);

fn symbols() -> Symbols {
    let mut s = Symbols::new();
    s.add("BTCUSD", I);
    s
}

fn at(n: i64) -> ReceiveTime {
    Timestamp::from_nanos(n)
}

// ---- the protocol --------------------------------------------------------

#[test]
fn a_quote_line_becomes_a_quote() {
    let event = parse_line(
        "Q BTCUSD 1700000000000000000 64000.25 0.5 64000.75 1.25",
        1,
        &symbols(),
        at(42),
    )
    .expect("parse")
    .expect("an event");

    assert_eq!(event.instrument, I);
    assert_eq!(event.exchange_time.to_nanos(), 1_700_000_000_000_000_000);
    // Stamped by the caller's clock, not taken from the line.
    assert_eq!(event.receive_time, at(42));
    match event.kind {
        MarketKind::Quote {
            bid_px,
            bid_qty,
            ask_px,
            ask_qty,
        } => {
            // Exact, through no float at any point.
            assert_eq!(bid_px, Px::from_scaled(64_000_250_000_000));
            assert_eq!(bid_qty, Qty::from_scaled(500_000_000));
            assert_eq!(ask_px, Px::from_scaled(64_000_750_000_000));
            assert_eq!(ask_qty, Qty::from_scaled(1_250_000_000));
        }
        other => panic!("expected a quote, got {other:?}"),
    }
}

#[test]
fn a_trade_line_becomes_a_trade() {
    let event = parse_line(
        "T BTCUSD 1700000000000000001 64000.5 0.1 S",
        1,
        &symbols(),
        at(1),
    )
    .expect("parse")
    .expect("an event");
    match event.kind {
        MarketKind::Trade { px, qty, aggressor } => {
            assert_eq!(px, Px::from_scaled(64_000_500_000_000));
            assert_eq!(qty, Qty::from_scaled(100_000_000));
            assert_eq!(aggressor, Side::Sell);
        }
        other => panic!("expected a trade, got {other:?}"),
    }
}

#[test]
fn blank_lines_and_comments_are_skipped() {
    for line in ["", "   ", "# a comment", "\t"] {
        assert_eq!(parse_line(line, 1, &symbols(), at(0)), Ok(None), "{line:?}");
    }
}

#[test]
fn a_malformed_line_is_refused_with_its_line_number() {
    let cases: [(&str, &str); 6] = [
        ("X BTCUSD 1 2 3 4 5", "first field must be Q or T"),
        ("Q BTCUSD", "no exchange timestamp"),
        (
            "Q BTCUSD notanumber 1 1 1 1",
            "exchange timestamp is not an integer number of nanoseconds",
        ),
        ("T BTCUSD 1 1 1 X", "aggressor must be B or S"),
        ("Q BTCUSD 1 1 1 1 1 extra", "trailing fields"),
        ("Q", "no symbol"),
    ];
    for (line, reason) in cases {
        assert_eq!(
            parse_line(line, 7, &symbols(), at(0)),
            Err(FeedError::Malformed { line: 7, reason }),
            "{line:?}"
        );
    }
}

#[test]
fn an_unknown_symbol_is_refused_rather_than_guessed() {
    assert_eq!(
        parse_line("Q ETHUSD 1 1 1 1 1", 3, &symbols(), at(0)),
        Err(FeedError::UnknownSymbol {
            line: 3,
            symbol: "ETHUSD".to_string()
        })
    );
}

#[test]
fn a_price_with_more_precision_than_the_scale_holds_is_refused() {
    // Not rounded. A feed sending more digits than the representation has is a
    // fact worth stopping on.
    assert_eq!(
        parse_line("Q BTCUSD 1 1.0000000001 1 2 1", 4, &symbols(), at(0)),
        Err(FeedError::BadNumber {
            line: 4,
            error: DecimalError::TooPrecise { limit: 9 }
        })
    );
}

// ---- clocks --------------------------------------------------------------

#[test]
fn a_manual_clock_only_moves_when_told() {
    let clock = ManualClock::at(1_000);
    assert_eq!(clock.receive_time().to_nanos(), 1_000);
    clock.advance(500);
    assert_eq!(clock.receive_time().to_nanos(), 1_500);
    assert_eq!(clock.monotonic().to_nanos(), 1_500);
}

#[test]
fn projecting_the_venues_clock_adds_locally_measured_elapsed_time() {
    let projected = project_exchange_time(
        ExchangeTime::from_nanos(1_000_000),
        at(500),
        at(500 + 250_000),
    );
    assert_eq!(projected.to_nanos(), 1_250_000);
}

#[test]
fn projecting_past_the_end_of_time_saturates_rather_than_wrapping() {
    let projected = project_exchange_time(ExchangeTime::MAX, at(0), at(1_000));
    assert_eq!(projected, ExchangeTime::MAX);
}

// ---- the merge -----------------------------------------------------------

fn feed_over(text: &'static str, clock: ManualClock) -> LiveFeed<ManualClock> {
    LiveFeed::spawn(text.as_bytes(), symbols(), clock, None)
}

#[test]
fn market_data_arrives_in_order() {
    let mut feed = feed_over(
        "Q BTCUSD 1000 100 1 101 1\nT BTCUSD 2000 100.5 1 B\nQ BTCUSD 3000 99 1 102 1\n",
        ManualClock::at(0),
    );
    let mut seen = Vec::new();
    while let Some(event) = feed.next_event() {
        seen.push(
            event
                .exchange_time()
                .expect("market data has one")
                .to_nanos(),
        );
    }
    assert_eq!(seen, vec![1_000, 2_000, 3_000]);
    assert_eq!(feed.market_events(), 3);
    assert!(feed.error().is_none());
}

#[test]
fn a_malformed_line_ends_the_session_and_says_why() {
    // A feed producing garbage is a feed to stop on: a paper session that
    // shrugs it off is the same code that will shrug it off in live.
    let mut feed = feed_over(
        "Q BTCUSD 1000 100 1 101 1\nthis is not an event\nQ BTCUSD 3000 99 1 102 1\n",
        ManualClock::at(0),
    );
    assert!(feed.next_event().is_some(), "the first line is fine");
    assert!(feed.next_event().is_none(), "the second should end it");
    assert!(matches!(
        feed.error(),
        Some(FeedError::Malformed { line: 2, .. })
    ));
    // And it stays ended.
    assert!(feed.next_event().is_none());
}

#[test]
fn an_operator_command_is_delivered_before_waiting_market_data() {
    // Commands are the most urgent input there is, and one of them is the kill
    // switch.
    let commands = commands_from("flatten\n".as_bytes());
    // Give the control thread a moment to queue it.
    std::thread::sleep(Duration::from_millis(50));
    let mut feed = LiveFeed::spawn(
        "Q BTCUSD 1000 100 1 101 1\n".as_bytes(),
        symbols(),
        ManualClock::at(7),
        Some(commands),
    );

    let first = feed.next_event().expect("an event");
    match first {
        Inbound::Command(c) => {
            assert_eq!(c.command, Command::Flatten);
            assert_eq!(c.receive_time.to_nanos(), 7, "stamped from the clock");
        }
        other => panic!("expected the command first, got {other:?}"),
    }
    assert_eq!(feed.commands_seen(), 1);
}

#[test]
fn a_kill_ends_the_session() {
    let commands = commands_from("kill\n".as_bytes());
    std::thread::sleep(Duration::from_millis(50));
    let mut feed = LiveFeed::spawn(
        "Q BTCUSD 1000 100 1 101 1\nQ BTCUSD 2000 100 1 101 1\n".as_bytes(),
        symbols(),
        ManualClock::at(0),
        Some(commands),
    );
    assert!(matches!(feed.next_event(), Some(Inbound::Command(_))));
    assert!(
        feed.next_event().is_none(),
        "nothing follows an operator kill"
    );
}

#[test]
fn an_unrecognised_command_word_is_ignored_rather_than_guessed() {
    let commands = commands_from("wibble\nhalt\n".as_bytes());
    std::thread::sleep(Duration::from_millis(50));
    let mut feed = LiveFeed::spawn("".as_bytes(), symbols(), ManualClock::at(0), Some(commands));
    match feed.next_event() {
        Some(Inbound::Command(c)) => assert_eq!(c.command, Command::Halt),
        other => panic!("expected halt, got {other:?}"),
    }
}

#[test]
fn a_timer_fires_once_the_projected_venue_clock_reaches_it() {
    let clock = ManualClock::at(1_000);
    let mut feed = LiveFeed::spawn(
        // One quote, then the source stays open by having nothing more to read.
        "Q BTCUSD 5000 100 1 101 1\n".as_bytes(),
        symbols(),
        clock.clone(),
        None,
    );

    // The quote anchors the projection: exchange 5_000 at receive 1_000.
    let first = feed.next_event().expect("the quote");
    assert!(matches!(first, Inbound::Market(_)));

    feed.schedule_timer(TimerRequest {
        strategy: StrategyId::new(0),
        at: ExchangeTime::from_nanos(6_000),
        token: TimerToken::new(3),
    });
    assert_eq!(feed.pending_timers(), 1);

    // Not due yet: only 500ns of local time has passed against a 1000ns wait.
    clock.advance(500);
    assert_eq!(feed.pending_timers(), 1);
    // Past it now. The clock is manual, so there is nothing to wait for.
    clock.advance(600);
    match feed.next_event() {
        Some(Inbound::Timer(t)) => {
            assert_eq!(t.token, TimerToken::new(3));
            assert_eq!(t.fires_at.to_nanos(), 6_000);
        }
        other => panic!("expected the timer, got {other:?}"),
    }
}

#[test]
fn a_timer_cannot_be_due_before_any_market_data_has_anchored_the_clock() {
    // The degenerate case: with nothing to project from, a timer waits for the
    // first quote rather than firing on a guess.
    let clock = ManualClock::at(0);
    let mut feed = LiveFeed::spawn("".as_bytes(), symbols(), clock.clone(), None);
    feed.schedule_timer(TimerRequest {
        strategy: StrategyId::new(0),
        at: ExchangeTime::from_nanos(1),
        token: TimerToken::new(1),
    });
    clock.advance(1_000_000);
    // The source is empty, so the feed ends rather than inventing a firing.
    assert!(feed.next_event().is_none());
    assert_eq!(feed.pending_timers(), 1, "the timer never became due");
}

#[test]
fn simultaneous_timers_fire_in_a_stated_order() {
    let clock = ManualClock::at(0);
    let mut feed = LiveFeed::spawn(
        "Q BTCUSD 0 100 1 101 1\n".as_bytes(),
        symbols(),
        clock.clone(),
        None,
    );
    feed.next_event().expect("the quote anchors the clock");

    for token in [9u64, 2, 5] {
        feed.schedule_timer(TimerRequest {
            strategy: StrategyId::new(0),
            at: ExchangeTime::from_nanos(10),
            token: TimerToken::new(token),
        });
    }
    clock.advance(100);

    let mut fired = Vec::new();
    while let Some(Inbound::Timer(t)) = feed.next_event() {
        fired.push(t.token.raw());
    }
    // Sorted by (at, strategy, token): a total order, so two runs agree.
    assert_eq!(fired, vec![2, 5, 9]);
}
