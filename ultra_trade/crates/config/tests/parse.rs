//! Reading a session config, and every way one can be wrong.
//!
//! The refusals matter more than the happy path. A config that is silently
//! misread is a session trading under limits nobody chose.

use config::{ConfigError, SessionConfig};
use marketdata::BarSpec;
use types::{InstrumentId, Px, Qty};

const GOOD: &str = "\
# two instruments, both tradable
instrument BTCUSD  tick 0.1   lot 0.00000001  min 0.0001
instrument ETHUSD  tick 0.01  lot 0.001       min 0.01

limits BTCUSD  max-position 5   max-exposure 500000  max-order-notional 100000 max-orders 60 rate-window 60s max-quote-age 30s
limits ETHUSD  max-position 50  max-exposure 200000  max-order-notional 50000  max-orders 30 rate-window 30s max-quote-age 500ms

strategy crossover BTCUSD  window 20  size 0.01
strategy crossover ETHUSD  window 30  size 0.5  bars 4
";

fn good() -> SessionConfig {
    SessionConfig::parse(GOOD).expect("the example config should parse")
}

fn err(text: &str) -> ConfigError {
    SessionConfig::parse(text).expect_err("this config should have been refused")
}

#[test]
fn it_reads_instruments_in_declaration_order() {
    let c = good();
    assert_eq!(c.instruments.len(), 2);
    assert_eq!(c.instruments[0].symbol, "BTCUSD");
    assert_eq!(c.instruments[0].id, InstrumentId::new(0));
    assert_eq!(c.instruments[1].symbol, "ETHUSD");
    assert_eq!(c.instruments[1].id, InstrumentId::new(1));
}

#[test]
fn conventions_are_read_exactly() {
    let c = good();
    let btc = c.by_symbol("BTCUSD").expect("declared");
    assert_eq!(btc.instrument.tick(), Px::from_scaled(100_000_000));
    assert_eq!(btc.instrument.lot(), Qty::from_scaled(10));
    assert_eq!(btc.instrument.min_qty(), Qty::from_scaled(100_000));
}

#[test]
fn limits_land_on_the_right_instrument() {
    let c = good();
    let btc = c.limits.get(InstrumentId::new(0)).expect("BTCUSD limits");
    assert_eq!(btc.max_position, Qty::from_scaled(5_000_000_000));
    assert_eq!(btc.max_orders_in_window, 60);
    assert_eq!(btc.rate_window.to_nanos(), 60_000_000_000);
    assert_eq!(btc.max_quote_age.to_nanos(), 30_000_000_000);

    let eth = c.limits.get(InstrumentId::new(1)).expect("ETHUSD limits");
    assert_eq!(eth.max_orders_in_window, 30);
    // 500ms, not 500 seconds.
    assert_eq!(eth.max_quote_age.to_nanos(), 500_000_000);
}

#[test]
fn strategies_resolve_to_their_instrument() {
    let c = good();
    assert_eq!(c.strategies.len(), 2);
    assert_eq!(c.strategies[0].instrument, InstrumentId::new(0));
    assert_eq!(c.strategies[0].window, 20);
    assert_eq!(c.strategies[0].size, Qty::from_scaled(10_000_000));
    assert_eq!(c.strategies[0].bars, BarSpec::Tick { threshold: 1 });
    assert_eq!(c.strategies[1].bars, BarSpec::Tick { threshold: 4 });
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let c = SessionConfig::parse(
        "\n# a comment\n\ninstrument X tick 1 lot 1 min 1  # trailing comment\n\
         limits X max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s\n",
    )
    .expect("parse");
    assert_eq!(c.instruments.len(), 1);
}

// ---- refusals ------------------------------------------------------------

#[test]
fn an_instrument_with_no_limits_is_refused_at_startup() {
    // Not discovered one refused order at a time.
    let e = err("instrument BTCUSD tick 0.1 lot 1 min 1\n");
    assert!(e.message.contains("no limits line"), "{e}");
    assert!(e.message.contains("BTCUSD"), "{e}");
}

#[test]
fn a_config_with_no_instruments_is_refused() {
    assert!(err("# nothing here\n").message.contains("no instruments"));
}

#[test]
fn a_misspelled_setting_is_refused_rather_than_ignored() {
    // A limit silently dropped is a limit that is not enforced.
    let e = err("\
instrument X tick 1 lot 1 min 1
limits X max-postion 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
");
    assert!(e.message.contains("max-postion"), "{e}");
    assert_eq!(e.line, 2);
}

#[test]
fn a_missing_setting_names_itself() {
    let e = err("instrument X tick 1 lot 1\n");
    assert!(e.message.contains("min"), "{e}");
    assert_eq!(e.line, 1);
}

#[test]
fn a_setting_with_no_value_is_refused() {
    let e = err("instrument X tick 1 lot 1 min\n");
    assert!(e.message.contains("no value"), "{e}");
}

#[test]
fn a_duplicate_instrument_is_refused() {
    let e = err("instrument X tick 1 lot 1 min 1\ninstrument X tick 1 lot 1 min 1\n");
    assert!(e.message.contains("twice"), "{e}");
    assert_eq!(e.line, 2);
}

#[test]
fn duplicate_limits_for_one_instrument_are_refused() {
    let e = err("\
instrument X tick 1 lot 1 min 1
limits X max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
limits X max-position 2 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
");
    assert!(e.message.contains("twice"), "{e}");
}

#[test]
fn limits_for_an_undeclared_instrument_are_refused() {
    let e = err("\
instrument X tick 1 lot 1 min 1
limits Y max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
");
    assert!(e.message.contains("Y has no instrument declaration"), "{e}");
}

#[test]
fn a_strategy_on_an_undeclared_instrument_is_refused() {
    let e = err("\
instrument X tick 1 lot 1 min 1
limits X max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
strategy crossover Z window 5 size 1
");
    assert!(e.message.contains("Z has no instrument declaration"), "{e}");
}

#[test]
fn an_unknown_strategy_is_refused_rather_than_skipped() {
    let e = err("\
instrument X tick 1 lot 1 min 1
limits X max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
strategy magic X window 5 size 1
");
    assert!(e.message.contains("not a strategy this build knows"), "{e}");
}

#[test]
fn an_unusable_convention_is_refused_with_its_reason() {
    let e = err("instrument X tick 0 lot 1 min 1\n");
    assert!(e.message.contains("unusable conventions"), "{e}");
}

#[test]
fn a_limit_that_could_never_bind_is_refused() {
    let e = err("\
instrument X tick 1 lot 1 min 1
limits X max-position 0 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
");
    assert!(e.message.contains("unusable limits"), "{e}");
}

#[test]
fn a_duration_without_a_unit_is_refused() {
    let e = err("\
instrument X tick 1 lot 1 min 1
limits X max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 60 max-quote-age 1s
");
    assert!(e.message.contains("needs a unit"), "{e}");
}

#[test]
fn an_unknown_directive_says_what_was_expected() {
    let e = err("wibble X\n");
    assert!(e.message.contains("instrument, limits, or strategy"), "{e}");
    assert_eq!(e.line, 1);
}

#[test]
fn a_price_with_more_precision_than_the_scale_holds_is_refused() {
    let e = err("instrument X tick 0.0000000001 lot 1 min 1\n");
    assert!(e.message.contains("precision"), "{e}");
}

#[test]
fn a_zero_window_or_size_is_refused() {
    let base = "\
instrument X tick 1 lot 1 min 1
limits X max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s
";
    assert!(
        err(&format!("{base}strategy crossover X window 0 size 1\n"))
            .message
            .contains("at least one bar")
    );
    assert!(
        err(&format!("{base}strategy crossover X window 5 size 0\n"))
            .message
            .contains("size must be positive")
    );
}

#[test]
fn every_error_carries_the_line_it_was_on() {
    let e = err("\
instrument X tick 1 lot 1 min 1
limits X max-position 1 max-exposure 1 max-order-notional 1 max-orders 1 rate-window 1s max-quote-age 1s

wibble
");
    assert_eq!(e.line, 4, "{e}");
    assert_eq!(e.to_string(), format!("line 4: {}", e.message));
}
