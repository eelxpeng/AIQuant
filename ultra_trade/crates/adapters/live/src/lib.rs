//! A live feed: market data arriving as it happens, on a real clock.
//!
//! The other of the two edges (`docs/ARCHITECTURE.md` seam 2). Binding this
//! instead of a recorded feed is what makes a run *paper* rather than a
//! backtest, and nothing downstream can tell.
//!
//! # Why this reads a line protocol rather than talking to an exchange
//!
//! Every venue's wire format is different, which is why the crate table says
//! `adapters/<venue>` — one crate per real venue. What is *common* to all of
//! them is what comes out: normalized quotes and trades. So this adapter takes
//! that, from any reader, and the venue-specific half stays outside.
//!
//! It also means the transport decision — a TLS and WebSocket stack in a
//! real-money repository — stays a decision someone makes on purpose, rather
//! than one that arrives with a feed adapter.
//!
//! A bridge is small in any language: connect to the exchange, print a line per
//! update, pipe it in.
//!
//! # The protocol
//!
//! One event per line. Blank lines and lines starting with `#` are ignored.
//!
//! ```text
//! Q <symbol> <exchange_nanos> <bid_px> <bid_qty> <ask_px> <ask_qty>
//! T <symbol> <exchange_nanos> <px> <qty> <B|S>
//! ```
//!
//! Prices and quantities are **decimals**, read exactly — no float is involved
//! at any point (Constitution VII). `exchange_nanos` is the venue's own
//! timestamp; the receive timestamp is stamped here, from the clock, because
//! that is what "when this process saw it" means.
//!
//! A malformed line **ends the session**. A feed producing garbage is a feed to
//! stop on: a paper session that shrugs it off is the same code that will shrug
//! it off in live.
//!
//! # Time
//!
//! This is the only place in the system that reads a wall clock. Everything
//! below it is handed timestamps (`docs/ARCHITECTURE.md` seam 4).
//!
//! It is also the only place that converts between clock kinds, in
//! [`project_exchange_time`] — the types make that impossible to do by
//! accident, so doing it deliberately means saying so out loud.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use engine::FeedAdapter;
use event::{Command, CommandEvent, Inbound, MarketEvent, MarketKind, TimerEvent};
use std::io::{BufRead, BufReader, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use strategy::TimerRequest;
use types::{
    Clock, DecimalError, ExchangeSpan, ExchangeTime, InstrumentId, MonotonicTime, Px, Qty,
    ReceiveTime, Side, Timestamp,
};

/// How long the feed waits on market data before checking commands and timers
/// again.
///
/// The cost of a quiet market is that a command or a timer waits at most this
/// long. Small enough to feel immediate, large enough not to spin a core.
const POLL: Duration = Duration::from_millis(5);

// ---- clocks --------------------------------------------------------------

/// The operating system's clocks.
///
/// The one implementation that reads ambient time, and it lives at the edge
/// where that is allowed. Nothing below the feed constructs one.
#[derive(Debug, Clone)]
pub struct SystemClock {
    anchor: Instant,
}

impl SystemClock {
    /// Anchors a monotonic origin at this moment.
    pub fn new() -> SystemClock {
        SystemClock {
            anchor: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> SystemClock {
        SystemClock::new()
    }
}

impl Clock for SystemClock {
    fn receive_time(&self) -> ReceiveTime {
        // Before 1970 is not a case worth carrying; a clock that far wrong has
        // bigger problems than this timestamp.
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0);
        Timestamp::from_nanos(nanos)
    }

    fn monotonic(&self) -> MonotonicTime {
        Timestamp::from_nanos(self.anchor.elapsed().as_nanos().min(i64::MAX as u128) as i64)
    }
}

/// A clock a test drives by hand.
///
/// Here rather than in `simkit` because it belongs beside the implementation it
/// stands in for: anything testing clock-dependent behaviour needs one, and a
/// clock that only moves when told is the only way to test a timeout without
/// sleeping through it.
#[derive(Debug, Clone, Default)]
pub struct ManualClock {
    nanos: Arc<AtomicI64>,
}

impl ManualClock {
    /// A clock stopped at `nanos`.
    pub fn at(nanos: i64) -> ManualClock {
        ManualClock {
            nanos: Arc::new(AtomicI64::new(nanos)),
        }
    }

    /// Moves it forward.
    pub fn advance(&self, nanos: i64) {
        self.nanos.fetch_add(nanos, Ordering::Release);
    }
}

impl Clock for ManualClock {
    fn receive_time(&self) -> ReceiveTime {
        Timestamp::from_nanos(self.nanos.load(Ordering::Acquire))
    }
    fn monotonic(&self) -> MonotonicTime {
        Timestamp::from_nanos(self.nanos.load(Ordering::Acquire))
    }
}

/// Projects the venue's clock forward by locally-measured elapsed time.
///
/// **The one cross-clock conversion in the system.** The types make this
/// impossible to do by accident — an exchange timestamp and a receive span do
/// not add — so doing it deliberately means writing it down.
///
/// It is an approximation, and the way it is wrong matters: the venue's clock
/// and ours do not run at exactly the same rate, so the longer the market has
/// been quiet, the further this drifts. It is used for one thing only —
/// deciding when a strategy's timer is due — where being a few milliseconds out
/// is not a correctness problem. It is **not** fit for ordering market events
/// or for anything a limit is checked against.
pub fn project_exchange_time(
    last_exchange: ExchangeTime,
    last_receive: ReceiveTime,
    now: ReceiveTime,
) -> ExchangeTime {
    let elapsed = now - last_receive;
    // The conversion, in one visible line.
    let same_length = ExchangeSpan::from_nanos(elapsed.to_nanos());
    last_exchange
        .checked_add(same_length)
        .unwrap_or(ExchangeTime::MAX)
}

// ---- the protocol --------------------------------------------------------

/// Why a line was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedError {
    /// The line did not have the shape of an event.
    Malformed {
        /// Which line, counting from one.
        line: u64,
        /// What was wrong with it.
        reason: &'static str,
    },
    /// The symbol is not one this session is configured to trade.
    UnknownSymbol {
        /// Which line.
        line: u64,
        /// What it said.
        symbol: String,
    },
    /// A price or quantity could not be read exactly.
    BadNumber {
        /// Which line.
        line: u64,
        /// What went wrong.
        error: DecimalError,
    },
    /// The source stopped being readable.
    Source(String),
}

impl std::fmt::Display for FeedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FeedError::Malformed { line, reason } => write!(f, "line {line}: {reason}"),
            FeedError::UnknownSymbol { line, symbol } => {
                write!(f, "line {line}: unknown symbol {symbol:?}")
            }
            FeedError::BadNumber { line, error } => write!(f, "line {line}: {error}"),
            FeedError::Source(e) => write!(f, "the feed source failed: {e}"),
        }
    }
}

impl std::error::Error for FeedError {}

/// What symbol means which instrument.
///
/// A short list searched linearly, which is both fast enough for the number of
/// instruments a session trades and deterministic, unlike a hash map.
#[derive(Debug, Clone, Default)]
pub struct Symbols {
    entries: Vec<(String, InstrumentId)>,
}

impl Symbols {
    /// An empty table.
    pub fn new() -> Symbols {
        Symbols::default()
    }

    /// Maps a symbol to an instrument.
    pub fn add(&mut self, symbol: impl Into<String>, instrument: InstrumentId) -> &mut Symbols {
        self.entries.push((symbol.into(), instrument));
        self
    }

    /// Looks a symbol up.
    pub fn get(&self, symbol: &str) -> Option<InstrumentId> {
        self.entries
            .iter()
            .find(|(name, _)| name == symbol)
            .map(|(_, id)| *id)
    }

    /// How many symbols are mapped.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is mapped.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Reads one line of the protocol.
///
/// `Ok(None)` for a blank line or a comment. The receive timestamp is supplied
/// rather than read here, so the caller decides which clock stamped it.
pub fn parse_line(
    line: &str,
    number: u64,
    symbols: &Symbols,
    receive_time: ReceiveTime,
) -> Result<Option<MarketEvent>, FeedError> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return Ok(None);
    }

    let mut fields = trimmed.split_whitespace();
    let kind = fields.next().ok_or(FeedError::Malformed {
        line: number,
        reason: "empty",
    })?;
    // Checked before the symbol on purpose. A line that is not an event at all
    // should be diagnosed as that, not as an unknown symbol in field two.
    if !matches!(kind, "Q" | "T" | "L" | "A" | "R") {
        return Err(FeedError::Malformed {
            line: number,
            reason: "first field must be Q, T, L, A or R",
        });
    }

    let symbol = fields.next().ok_or(FeedError::Malformed {
        line: number,
        reason: "no symbol",
    })?;
    let instrument = symbols
        .get(symbol)
        .ok_or_else(|| FeedError::UnknownSymbol {
            line: number,
            symbol: symbol.to_string(),
        })?;

    let exchange_nanos: i64 = fields
        .next()
        .ok_or(FeedError::Malformed {
            line: number,
            reason: "no exchange timestamp",
        })?
        .parse()
        .map_err(|_| FeedError::Malformed {
            line: number,
            reason: "exchange timestamp is not an integer number of nanoseconds",
        })?;

    let number_at = |text: Option<&str>, what: &'static str| -> Result<i64, FeedError> {
        let text = text.ok_or(FeedError::Malformed {
            line: number,
            reason: what,
        })?;
        types::parse_scaled(text).map_err(|error| FeedError::BadNumber {
            line: number,
            error,
        })
    };

    let kind = match kind {
        "Q" => {
            let bid_px = Px::from_scaled(number_at(fields.next(), "no bid price")?);
            let bid_qty = Qty::from_scaled(number_at(fields.next(), "no bid quantity")?);
            let ask_px = Px::from_scaled(number_at(fields.next(), "no ask price")?);
            let ask_qty = Qty::from_scaled(number_at(fields.next(), "no ask quantity")?);
            MarketKind::Quote {
                bid_px,
                bid_qty,
                ask_px,
                ask_qty,
            }
        }
        "T" => {
            let px = Px::from_scaled(number_at(fields.next(), "no trade price")?);
            let qty = Qty::from_scaled(number_at(fields.next(), "no trade quantity")?);
            let aggressor = match fields.next() {
                Some("B") => Side::Buy,
                Some("S") => Side::Sell,
                _ => {
                    return Err(FeedError::Malformed {
                        line: number,
                        reason: "aggressor must be B or S",
                    });
                }
            };
            MarketKind::Trade { px, qty, aggressor }
        }
        // One changed level of the book. A run of these is one update and only
        // becomes visible at the `A` that closes it (ADR, order-book depth).
        "L" => {
            let side = match fields.next() {
                Some("B") => Side::Buy,
                Some("S") => Side::Sell,
                _ => {
                    return Err(FeedError::Malformed {
                        line: number,
                        reason: "level side must be B or S",
                    });
                }
            };
            let px = Px::from_scaled(number_at(fields.next(), "no level price")?);
            // Zero is not missing: it is how a venue removes a level.
            let qty = Qty::from_scaled(number_at(fields.next(), "no level quantity")?);
            MarketKind::Level { side, px, qty }
        }
        "A" => MarketKind::BookApplied,
        // The bridge caught our book disagreeing with the venue's checksum, or
        // is opening a subscription. Either way what is held is discarded and a
        // snapshot follows.
        "R" => MarketKind::BookReset,
        // Unreachable: the kind was checked above, before the symbol.
        other => unreachable!("unchecked record kind {other:?}"),
    };

    if fields.next().is_some() {
        return Err(FeedError::Malformed {
            line: number,
            reason: "trailing fields",
        });
    }

    Ok(Some(MarketEvent {
        instrument,
        exchange_time: Timestamp::from_nanos(exchange_nanos),
        receive_time,
        kind,
    }))
}

/// Reads operator commands, one word per line, from a reader.
///
/// Returns the channel the feed merges. Spawns a thread, because reading a
/// terminal blocks and the session has to keep running while nobody is typing.
pub fn commands_from(source: impl Read + Send + 'static) -> Receiver<Command> {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("ultra_trade-control".into())
        .spawn(move || {
            for line in BufReader::new(source).lines() {
                let Ok(line) = line else { break };
                let command = match line.trim().to_ascii_lowercase().as_str() {
                    "halt" => Command::Halt,
                    "resume" => Command::Resume,
                    "kill" => Command::Kill,
                    "flatten" => Command::Flatten,
                    "" => continue,
                    other => {
                        eprintln!("control: {other:?} is not one of halt, resume, kill, flatten");
                        continue;
                    }
                };
                let stop = command == Command::Kill;
                if tx.send(command).is_err() || stop {
                    break;
                }
            }
        })
        .expect("spawn the control reader");
    rx
}

// ---- the feed ------------------------------------------------------------

/// Market data as it arrives, merged with operator commands and timers.
#[derive(Debug)]
pub struct LiveFeed<C: Clock> {
    market: Receiver<Result<MarketEvent, FeedError>>,
    commands: Option<Receiver<Command>>,
    clock: C,
    /// Kept sorted by `(at, strategy, token)`, a total order.
    timers: Vec<TimerRequest>,
    /// The most recent market event's clocks, for projecting timer due-ness.
    last_seen: Option<(ExchangeTime, ReceiveTime)>,
    ended: bool,
    error: Option<FeedError>,
    market_events: u64,
    commands_seen: u64,
}

impl<C: Clock + Clone + Send + 'static> LiveFeed<C> {
    /// Starts reading a source.
    ///
    /// The reader runs on its own thread so a quiet market does not stop
    /// commands and timers from being served.
    pub fn spawn(
        source: impl Read + Send + 'static,
        symbols: Symbols,
        clock: C,
        commands: Option<Receiver<Command>>,
    ) -> LiveFeed<C> {
        let (tx, market) = channel();
        let reader_clock = clock.clone();
        thread::Builder::new()
            .name("ultra_trade-feed".into())
            .spawn(move || read_lines(source, symbols, reader_clock, tx))
            .expect("spawn the feed reader");

        LiveFeed {
            market,
            commands,
            clock,
            timers: Vec::new(),
            last_seen: None,
            ended: false,
            error: None,
            market_events: 0,
            commands_seen: 0,
        }
    }
}

impl<C: Clock> LiveFeed<C> {
    /// Why the feed stopped, if it stopped because of a problem.
    #[inline]
    pub fn error(&self) -> Option<&FeedError> {
        self.error.as_ref()
    }

    /// Market events delivered.
    #[inline]
    pub fn market_events(&self) -> u64 {
        self.market_events
    }

    /// Operator commands delivered.
    #[inline]
    pub fn commands_seen(&self) -> u64 {
        self.commands_seen
    }

    /// Timers waiting to fire.
    #[inline]
    pub fn pending_timers(&self) -> usize {
        self.timers.len()
    }

    /// The earliest timer, if the venue's clock has reached it.
    fn take_due(&mut self) -> Option<TimerRequest> {
        let first = *self.timers.first()?;
        // Before any market data there is nothing to project from, so a timer
        // cannot be shown to be due. It waits for the first quote.
        let (last_exchange, last_receive) = self.last_seen?;
        let now = project_exchange_time(last_exchange, last_receive, self.clock.receive_time());
        if first.at.to_nanos() > now.to_nanos() {
            return None;
        }
        Some(self.timers.remove(0))
    }
}

impl<C: Clock> FeedAdapter for LiveFeed<C> {
    fn next_event(&mut self) -> Option<Inbound> {
        if self.ended {
            return None;
        }
        loop {
            // Operator commands first: they are the most urgent input there is,
            // and one of them is the kill switch.
            if let Some(commands) = &self.commands
                && let Ok(command) = commands.try_recv()
            {
                self.commands_seen += 1;
                if command == Command::Kill {
                    // The operator ended the session. Nothing after this.
                    self.ended = true;
                }
                return Some(Inbound::Command(CommandEvent {
                    command,
                    receive_time: self.clock.receive_time(),
                }));
            }

            if let Some(due) = self.take_due() {
                return Some(Inbound::Timer(TimerEvent {
                    strategy: due.strategy,
                    token: due.token,
                    fires_at: due.at,
                    receive_time: self.clock.receive_time(),
                }));
            }

            match self.market.recv_timeout(POLL) {
                Ok(Ok(event)) => {
                    self.last_seen = Some((event.exchange_time, event.receive_time));
                    self.market_events += 1;
                    return Some(Inbound::Market(event));
                }
                Ok(Err(e)) => {
                    // A feed producing garbage is a feed to stop on.
                    self.error = Some(e);
                    self.ended = true;
                    return None;
                }
                // Nothing yet. Go round: a command or a timer may be due.
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => {
                    self.ended = true;
                    return None;
                }
            }
        }
    }

    fn schedule_timer(&mut self, request: TimerRequest) {
        self.timers.push(request);
        self.timers
            .sort_by_key(|t| (t.at.to_nanos(), t.strategy.raw(), t.token.raw()));
    }
}

/// The reader thread.
fn read_lines<C: Clock>(
    source: impl Read,
    symbols: Symbols,
    clock: C,
    tx: Sender<Result<MarketEvent, FeedError>>,
) {
    let mut number = 0u64;
    for line in BufReader::new(source).lines() {
        number += 1;
        let line = match line {
            Ok(line) => line,
            Err(e) => {
                let _ = tx.send(Err(FeedError::Source(e.to_string())));
                return;
            }
        };
        // Stamped here, on arrival, which is what a receive timestamp means.
        match parse_line(&line, number, &symbols, clock.receive_time()) {
            Ok(None) => continue,
            Ok(Some(event)) => {
                if tx.send(Ok(event)).is_err() {
                    return;
                }
            }
            Err(e) => {
                let _ = tx.send(Err(e));
                return;
            }
        }
    }
}
