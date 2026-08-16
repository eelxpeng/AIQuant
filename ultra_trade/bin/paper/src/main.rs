//! Paper trading: a live feed, a simulated venue, and no venue risk.
//!
//! ```text
//! paper <session.conf> <market-source> <output.log>
//! ```
//!
//! `session.conf` says what to trade, under what limits, with which strategies
//! (`crates/config`). `market-source` is a path — a file, or a FIFO a bridge
//! writes into — or `-` for standard input. Operator commands are read from standard input, one word
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

use config::SessionConfig;
use engine::{Engine, EngineConfig, FeedAdapter};
use event::codec::{InstrumentEntry, LogHeader};
use event::{BackgroundLog, EngineState, Segments};
use live_feed::{LiveFeed, Symbols, SystemClock, commands_from};
use recovery::recover;
use sim_venue::{Fees, FillModel, SimVenue};
use std::fs::File;
use std::time::{Duration, Instant};
use types::{Clock as _, ExchangeSpan, OrderId, Px, StrategyId, Timestamp};

/// How often the session says what it is doing.
const STATUS_EVERY: Duration = Duration::from_secs(5);

fn usage() -> ! {
    eprintln!("usage: paper <session.conf> <market-source> <output.log>");
    eprintln!();
    eprintln!("  session.conf    what to trade, under what limits, with which strategies");
    eprintln!("  market-source   a path to read events from, or - for stdin");
    eprintln!("  output.log      where to record the session; must not exist");
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
    if args.len() != 3 {
        usage();
    }
    let (config_path, source_path, log_path) = (&args[0], &args[1], &args[2]);

    let session = SessionConfig::load(config_path)
        .unwrap_or_else(|e| fail(&format!("cannot read {config_path}"), e));
    let instruments = session.instrument_list();

    let clock = SystemClock::new();
    let started = clock.receive_time();

    // The header carries every instrument, so the recording is interpretable
    // without this config file — and a backtest over it will refuse a config
    // that disagrees.
    let entries: Vec<InstrumentEntry> = session
        .instruments
        .iter()
        .map(|i| {
            InstrumentEntry::new(i.instrument, &i.symbol)
                .unwrap_or_else(|e| fail(&format!("symbol {}", i.symbol), e))
        })
        .collect();
    let header = LogHeader::new(
        started.to_nanos().unsigned_abs(),
        Timestamp::from_nanos(0),
        OrderId::new(0),
        entries,
    );
    // A session is a chain of segments, so `session.log` names the session and
    // the records land in `session.0.log` (`Segments`). If a session is already
    // there, this is a restart after a crash: continue the chain rather than
    // refusing, and rebuild what it held before trading again.
    let root = std::path::Path::new(log_path);
    let resuming = !Segments::paths(root)
        .unwrap_or_else(|e| fail(&format!("cannot read {log_path}"), e))
        .is_empty();
    let log = if resuming {
        BackgroundLog::resume(root, 1 << 16)
            .unwrap_or_else(|e| fail(&format!("cannot continue {log_path}"), e))
    } else {
        let first = Segments::create_path(root)
            .unwrap_or_else(|e| fail(&format!("cannot create {log_path}"), e));
        BackgroundLog::create(&first, header, 1 << 16)
            .unwrap_or_else(|e| fail(&format!("cannot create {}", first.display()), e))
    };

    let mut symbols = Symbols::new();
    for instrument in &session.instruments {
        symbols.add(instrument.symbol.clone(), instrument.id);
    }

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
        instruments.len(),
        FillModel::TouchDisplayed,
        Fees {
            maker: Px::ZERO,
            taker: Px::ZERO,
        },
        ExchangeSpan::from_nanos(0),
    );

    let config = EngineConfig::new(
        instruments.clone(),
        session.limits.clone(),
        OrderId::new(0),
        Timestamp::from_nanos(0),
    );
    let mut engine = Engine::new(config, venue, log);
    for (index, spec) in session.strategies.iter().enumerate() {
        // The config builds its own strategy: which kind it is, and which
        // settings that kind takes, are its business rather than every
        // binary's.
        let strategy = spec.build(StrategyId::new(index as u16), |aggregator| {
            engine.add_aggregator(aggregator)
        });
        engine
            .add_strategy(strategy)
            .unwrap_or_else(|e| fail("strategy", format!("{e:?}")));
    }

    // Rebuild what the crashed session held, by replaying its own recording
    // through this engine and this venue (`recovery`). It ends halted, and
    // only an operator resumes it (contract D-2).
    let recovered = if resuming {
        match recover(&mut engine, root) {
            Ok(recovered) => Some(recovered),
            Err(e) => fail(&format!("cannot recover {log_path}"), e),
        }
    } else {
        None
    };

    println!("paper session from {config_path}");
    println!("  market source  {source_path}");
    println!("  recording to   {log_path}");
    for i in &session.instruments {
        println!(
            "  instrument     {} (id {})  tick {}  lot {}",
            i.symbol,
            i.id.raw(),
            i.instrument.tick(),
            i.instrument.lot()
        );
    }
    println!("  strategies     {}", session.strategies.len());
    if let Some(recovered) = &recovered {
        println!(
            "  RESUMED        after a crash, from {} records",
            recovered.records
        );
        if !recovered.recovery.is_clean() {
            println!(
                "                 {} — the tail was lost",
                recovered.recovery
            );
        }
        for position in recovered.from_log.iter() {
            if !position.is_flat() {
                println!(
                    "                 holding {} of instrument {}",
                    position.qty(),
                    position.instrument().raw()
                );
            }
        }
        println!("  state          HALTED — type `resume` to trade again");
    }
    if source_path == "-" {
        println!("  commands       disabled (stdin is the market source)");
    } else {
        println!("  commands       halt | resume | kill | flatten");
    }
    println!();

    // The pump, written here rather than using `engine::run`, so the session
    // can say what it is doing while it runs. It is the same three steps.
    let mut requests = Vec::new();
    // Orders a recovery replayed already happened and were announced by the
    // session that placed them. Starting the counter past them keeps the
    // resumed session from reporting a crash's history as things it just did.
    let mut reported_orders = engine.orders().len();
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
                    order.qty(),
                    order.kind()
                );
            }
            reported_orders += 1;
        }

        if last_status.elapsed() >= STATUS_EVERY {
            last_status = Instant::now();
            let held: Vec<String> = session
                .instruments
                .iter()
                .map(|i| {
                    let p = engine.positions().get(i.id).expect("configured");
                    format!("{} {}", i.symbol, p.qty())
                })
                .collect();
            println!(
                "  [{:?}] {} events, {} orders, {}",
                engine.state(),
                feed.market_events(),
                engine.orders().len(),
                held.join("  "),
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

    let state = engine.state();
    let market_events = feed.market_events();
    let commands_seen = feed.commands_seen();
    let orders = engine.orders().len();

    println!();
    println!("session over");
    println!("  market events   {market_events}");
    println!("  commands        {commands_seen}");
    println!("  orders          {orders}");
    for i in &session.instruments {
        let p = engine.positions().get(i.id).expect("configured");
        println!(
            "  {:<14}  realized {}  position {}",
            i.symbol,
            p.realized(),
            p.qty()
        );
    }
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
