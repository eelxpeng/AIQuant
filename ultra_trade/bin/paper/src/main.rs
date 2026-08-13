//! Paper trading: a live feed, a simulated venue, and no venue risk.
//!
//! ```text
//! paper <symbol> <market-source> <output.log> [tick] [lot]
//! ```
//!
//! `market-source` is a path — a file, or a FIFO a bridge writes into — or `-`
//! for standard input. Operator commands are read from standard input, one word
//! per line: `halt`, `resume`, `kill`, `flatten`. (With `-` as the source there
//! is no console left to type into, so commands are disabled.)
//!
//! A thin binary. It binds a feed, a venue, a strategy, a clock, and a log, and
//! runs the loop — no domain logic (`docs/ARCHITECTURE.md`).
//!
//! What makes this *paper* rather than live is one binding: the venue is
//! simulated. What makes it paper rather than a backtest is the other: the feed
//! is live and the clock is real. Swap the venue for a real one and this is
//! live trading, with the same engine, the same strategy, and the same risk
//! gate (Constitution III).
//!
//! # Getting data into it
//!
//! The feed reads one event per line (`live_feed`'s protocol). A bridge from
//! any exchange is small in any language: connect, print a line per update.
//!
//! ```text
//! mkfifo /tmp/md
//! ./my-bridge > /tmp/md &
//! paper BTCUSD /tmp/md session.log 0.01 0.00000001
//! ```

use engine::{Engine, EngineConfig, FeedAdapter};
use event::codec::{InstrumentEntry, LogHeader};
use event::{BackgroundLog, EngineState};
use live_feed::{LiveFeed, Symbols, SystemClock, commands_from};
use marketdata::{Aggregator, BarSpec};
use risk::{LimitBook, Limits};
use sim_venue::{Fees, FillModel, SimVenue};
use std::fs::File;
use std::time::{Duration, Instant};
use strategy::MovingAverageCrossover;
use types::{
    Clock as _, ExchangeSpan, Instrument, InstrumentId, Notional, OrderId, Px, Qty, SCALE,
    StrategyId, Timestamp,
};

const INSTRUMENT: InstrumentId = InstrumentId::new(0);
/// How often the session says what it is doing.
const STATUS_EVERY: Duration = Duration::from_secs(5);

fn qty(whole: i64) -> Qty {
    Qty::from_scaled(whole * SCALE)
}

fn money(whole: i64) -> Notional {
    Notional::from_scaled(whole as i128 * SCALE as i128)
}

fn decimal(scaled: i128) -> String {
    let unit = SCALE as i128;
    let sign = if scaled < 0 { "-" } else { "" };
    let magnitude = scaled.unsigned_abs();
    format!(
        "{sign}{}.{:09}",
        magnitude / unit as u128,
        magnitude % unit as u128
    )
}

fn usage() -> ! {
    eprintln!("usage: paper <symbol> <market-source> <output.log> [tick] [lot]");
    eprintln!();
    eprintln!("  symbol          what the feed calls the instrument");
    eprintln!("  market-source   a path to read events from, or - for stdin");
    eprintln!("  output.log      where to record the session; must not exist");
    eprintln!("  tick            price increment as a decimal (default 0.01)");
    eprintln!("  lot             quantity increment as a decimal (default 0.000001)");
    eprintln!();
    eprintln!("operator commands are read from stdin: halt, resume, kill, flatten");
    eprintln!();
    eprintln!("feed protocol, one event per line:");
    eprintln!("  Q <symbol> <exchange_nanos> <bid_px> <bid_qty> <ask_px> <ask_qty>");
    eprintln!("  T <symbol> <exchange_nanos> <px> <qty> <B|S>");
    std::process::exit(2);
}

fn fail(context: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("paper: {context}: {e}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() < 3 || args.len() > 5 {
        usage();
    }
    let symbol = &args[0];
    let source_path = &args[1];
    let log_path = &args[2];
    let tick = Px::from_decimal(args.get(3).map(String::as_str).unwrap_or("0.01"))
        .unwrap_or_else(|e| fail("tick", e));
    let lot = Qty::from_decimal(args.get(4).map(String::as_str).unwrap_or("0.000001"))
        .unwrap_or_else(|e| fail("lot", e));

    let instrument = Instrument::new(INSTRUMENT, tick, lot, lot)
        .unwrap_or_else(|e| fail("instrument conventions", e));

    // Limits are this session's policy. Generous, because paper carries no
    // venue risk — a live binding would want them argued over.
    let mut limits = LimitBook::with_instruments(1);
    limits
        .set(
            INSTRUMENT,
            Limits {
                max_position: qty(1_000),
                max_exposure: money(10_000_000),
                max_order_notional: money(1_000_000),
                max_orders_in_window: 60,
                rate_window: ExchangeSpan::from_nanos(60_000_000_000),
                max_quote_age: ExchangeSpan::from_nanos(30_000_000_000),
            },
        )
        .unwrap_or_else(|e| fail("limits", format!("{e:?}")));

    let clock = SystemClock::new();
    let started = clock.receive_time();

    let header = LogHeader::new(
        started.to_nanos().unsigned_abs(),
        Timestamp::from_nanos(0),
        OrderId::new(0),
        vec![InstrumentEntry::new(instrument, symbol).unwrap_or_else(|e| fail("symbol", e))],
    );
    let log = BackgroundLog::create(log_path, header, 1 << 16)
        .unwrap_or_else(|e| fail(&format!("cannot create {log_path}"), e));

    let mut symbols = Symbols::new();
    symbols.add(symbol.clone(), INSTRUMENT);

    // Standard input is either the market data or the operator's console. It
    // cannot be both, and pretending otherwise would silently eat commands.
    let (source, commands): (Box<dyn std::io::Read + Send>, _) = if source_path == "-" {
        (Box::new(std::io::stdin()), None)
    } else {
        let file = File::open(source_path)
            .unwrap_or_else(|e| fail(&format!("cannot open {source_path}"), e));
        (Box::new(file), Some(commands_from(std::io::stdin())))
    };

    let mut feed = LiveFeed::spawn(source, symbols, clock.clone(), commands);

    // The binding that makes this paper rather than live.
    let venue = SimVenue::new(
        1,
        FillModel::TouchDisplayed,
        Fees {
            maker: Px::ZERO,
            taker: Px::ZERO,
        },
        ExchangeSpan::from_nanos(0),
    );

    let config = EngineConfig::new(
        vec![instrument],
        limits,
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let mut engine = Engine::new(config, venue, log);
    let subscription = engine
        .add_aggregator(Aggregator::new(INSTRUMENT, BarSpec::Tick { threshold: 1 }).expect("spec"));
    engine
        .add_strategy(Box::new(MovingAverageCrossover::new(
            StrategyId::new(0),
            INSTRUMENT,
            subscription,
            20,
            qty(1),
        )))
        .expect("strategy");

    println!("paper session on {symbol}");
    println!("  market source  {source_path}");
    println!("  recording to   {log_path}");
    println!(
        "  tick / lot     {} / {}",
        decimal(tick.to_scaled() as i128),
        decimal(lot.to_scaled() as i128)
    );
    if source_path == "-" {
        println!("  commands       disabled (stdin is the market source)");
    } else {
        println!("  commands       halt | resume | kill | flatten");
    }
    println!();

    // The pump, written here rather than using `engine::run`, so the session
    // can say what it is doing while it runs. It is the same three steps.
    let mut requests = Vec::new();
    let mut reported_orders = 0usize;
    let mut last_status = Instant::now();

    while let Some(event) = feed.next_event() {
        if let Err(e) = engine.on_inbound(event) {
            eprintln!("paper: session stopped: {e:?}");
            break;
        }
        engine.take_timer_requests(&mut requests);
        for request in requests.drain(..) {
            feed.schedule_timer(request);
        }

        // Anything new is worth seeing as it happens.
        while reported_orders < engine.orders().len() {
            if let Some(order) = engine.orders().iter().nth(reported_orders) {
                println!(
                    "  order {} {:?} {} @ {:?}",
                    order.id(),
                    order.side(),
                    decimal(order.qty().to_scaled() as i128),
                    order.kind()
                );
            }
            reported_orders += 1;
        }

        if last_status.elapsed() >= STATUS_EVERY {
            last_status = Instant::now();
            let position = engine.positions().get(INSTRUMENT).expect("configured");
            println!(
                "  [{:?}] {} events, {} orders, position {}",
                engine.state(),
                feed.market_events(),
                engine.orders().len(),
                decimal(position.qty().to_scaled() as i128),
            );
        }

        if engine.state() == EngineState::Killed {
            println!("  killed; stopping");
            break;
        }
    }

    if let Some(e) = feed.error() {
        eprintln!("paper: the feed stopped: {e}");
    }

    let position = engine.positions().get(INSTRUMENT).expect("configured");
    let realized = position.realized();
    let held = position.qty();
    let state = engine.state();
    let market_events = feed.market_events();
    let commands_seen = feed.commands_seen();
    let orders = engine.orders().len();

    println!();
    println!("session over");
    println!("  market events   {market_events}");
    println!("  commands        {commands_seen}");
    println!("  orders          {orders}");
    println!("  realized        {}", decimal(realized.to_scaled()));
    println!("  position        {}", decimal(held.to_scaled() as i128));
    println!("  final state     {state:?}");

    // Persist and say whether it worked, rather than dropping the answer.
    match engine.into_log().shutdown() {
        Ok(report) => {
            println!("  recorded        {} records to {log_path}", report.written);
            if report.high_water * 2 > report.capacity {
                println!(
                    "  WARNING         the log ring reached {} of {} — a slower disk would have halted the session",
                    report.high_water, report.capacity
                );
            }
        }
        Err(e) => fail("could not persist the recording", e),
    }
}
