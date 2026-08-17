//! Reads a recording back and says what happened.
//!
//! ```text
//! journal <recorded.log> [--all | --fills | --curve] [--from N] [--to N]
//! ```
//!
//! The design claims a recording is the audit trail: every input is in it,
//! every decision names the record that caused it, and the numbers are
//! re-derivable from it alone. Until now nothing in the repository read one
//! back, so the claim was only ever exercised by the code that wrote it — a
//! session was a summary and a black box behind it. This opens the box.
//!
//! A pure consumer. It opens the file read-only, holds no engine state, and
//! re-derives position and profit through `oms`'s accounting rather than
//! reimplementing it. A second answer to "what did this fill do to the books"
//! is a second answer waiting to disagree with the first (`report`).
//!
//! # The four views
//!
//! - default — decisions and what the venue said back. What the system *did*.
//! - `--all` — every record, market data included. What the system *saw*.
//! - `--fills` — the money: each fill with the running position and profit.
//! - `--curve` — the same, as CSV, for plotting elsewhere.
//!
//! # A torn file is a result, not an error
//!
//! A recording whose tail was lost to a crash still reads: the reader stops at
//! the last intact record and reports what it dropped. That is the state this
//! tool most needs to survive, because a crashed session is exactly when
//! somebody wants to read the log.

#![forbid(unsafe_code)]

use std::io::{self, Write};

use event::{Command, Event, Inbound, MarketKind, Outbound, Record, Segments, Seq, VenueKind};
use marketdata::MarkRule;
use oms::Positions;
use report::summarize;

/// How an open position is valued. The engine's own exposure rule, so a
/// reading of a session cannot disagree with what the session believed.
const MARK_RULE: MarkRule = MarkRule::Mid(types::RoundDir::Down);
use types::{InstrumentId, Notional, OrderId, Px, Qty, Side};

/// Which view was asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum View {
    /// Decisions and venue reports.
    Decisions,
    /// Every record.
    All,
    /// The fills, with running accounting.
    Fills,
    /// The realized-profit curve, as CSV.
    Curve,
}

/// Why the tool stopped.
enum Fault {
    /// Whatever was reading us went away — `journal … | head` is the usual
    /// one. Not a failure: the reader got what it asked for.
    Pipe,
    /// Something the operator should see.
    Said(String),
}

impl From<io::Error> for Fault {
    fn from(e: io::Error) -> Fault {
        match e.kind() {
            io::ErrorKind::BrokenPipe => Fault::Pipe,
            _ => Fault::Said(e.to_string()),
        }
    }
}

impl From<String> for Fault {
    fn from(message: String) -> Fault {
        Fault::Said(message)
    }
}

impl From<&str> for Fault {
    fn from(message: &str) -> Fault {
        Fault::Said(message.to_string())
    }
}

struct Args {
    path: String,
    view: View,
    from: u64,
    to: u64,
    /// Emit machine-readable output instead of a table.
    json: bool,
    /// Most fills to include in JSON output.
    ///
    /// A session that runs for days has millions, and a caller polling every
    /// second does not want them all down the wire every time. The summary is
    /// always complete; only the array is trimmed, and it says by how much.
    tail: usize,
}

fn parse_args() -> Result<Args, Fault> {
    let mut path = None;
    let mut view = View::Decisions;
    let mut from = 0u64;
    let mut to = u64::MAX;
    let mut json = false;
    let mut tail = 2_000usize;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--all" => view = View::All,
            "--fills" => view = View::Fills,
            "--curve" => view = View::Curve,
            "--json" => json = true,
            "--tail" => {
                let raw = argv.next().ok_or("--tail needs a count")?;
                tail = raw
                    .parse()
                    .map_err(|_| format!("--tail wants a count, got {raw:?}"))?;
            }
            "--from" | "--to" => {
                let raw = argv
                    .next()
                    .ok_or_else(|| format!("{arg} needs a sequence number"))?;
                let value = raw
                    .trim_start_matches('#')
                    .parse::<u64>()
                    .map_err(|_| format!("{arg} wants a sequence number, got {raw:?}"))?;
                if arg == "--from" {
                    from = value
                } else {
                    to = value
                }
            }
            other if other.starts_with("--") => {
                return Err(format!("unknown option {other}").into());
            }
            other => {
                if path.replace(other.to_string()).is_some() {
                    return Err("only one recording at a time".into());
                }
            }
        }
    }

    Ok(Args {
        path: path
            .ok_or("usage: journal <recorded.log> [--all|--fills|--curve] [--json] [--tail N]")?,
        view,
        from,
        to,
        json,
        tail,
    })
}

fn main() {
    match run() {
        Ok(()) | Err(Fault::Pipe) => {}
        Err(Fault::Said(message)) => {
            eprintln!("journal: {message}");
            std::process::exit(1);
        }
    }
}

fn run() -> Result<(), Fault> {
    let args = parse_args()?;

    // A session is its whole chain of segments, not one file: a session that
    // crashed and continued is still one session (`Segments`).
    let root = std::path::Path::new(&args.path);
    let header = Segments::header(root).map_err(|e| format!("cannot read {}: {e}", args.path))?;
    let (records, recovery) =
        Segments::read_all(root).map_err(|e| format!("cannot read {}: {e}", args.path))?;
    let segments = Segments::paths(root).map_err(|e| format!("cannot read {}: {e}", args.path))?;

    // Symbols by instrument id, so every later line can name an instrument
    // instead of numbering it. The header carries them precisely so a
    // recording is readable without the config that produced it.
    let symbols: Vec<String> = header
        .instruments
        .iter()
        .map(|entry| {
            let end = entry
                .symbol
                .iter()
                .position(|&b| b == 0)
                .unwrap_or(entry.symbol.len());
            String::from_utf8_lossy(&entry.symbol[..end]).into_owned()
        })
        .collect();

    let stdout = io::stdout();
    let mut out = stdout.lock();

    if args.json {
        let summary = summarize(&records, header.instruments.len(), MARK_RULE)
            .map_err(|e| format!("this recording does not add up: {e:?}"))?;
        print_json(
            &mut out,
            &args,
            &header,
            &symbols,
            &records,
            &recovery,
            segments.len(),
            &summary,
        )?;
        return Ok(out.flush()?);
    }

    if args.view == View::Curve {
        print_curve(&mut out, &records, &symbols)?;
        return Ok(out.flush()?);
    }

    writeln!(
        out,
        "session {} · format {} · {}",
        header.session_id, header.format_version, recovery
    )?;
    // Two different kinds of damage, which need different words. A read that
    // *stopped* means the session is cut off there and everything below is a
    // partial session — a conclusion someone will act on. Bytes discarded
    // without a stop means a crash lost records and the session carried on in
    // the next segment: real, worth saying, and not the same thing at all.
    if recovery.stopped.is_some() {
        writeln!(
            out,
            "  !! this session is cut off here — everything below is a partial session"
        )?;
    } else if recovery.discarded_bytes > 0 {
        writeln!(
            out,
            "  !! {} bytes were lost to a crash; the session continued after them",
            recovery.discarded_bytes
        )?;
    }
    writeln!(out, "  first order {}", header.first_order_id)?;
    if segments.len() > 1 {
        // Worth saying plainly: more than one segment means the session was
        // continued after a crash, which is context for everything below it.
        writeln!(
            out,
            "  {} segments — this session was continued after a crash",
            segments.len()
        )?;
    }

    // The header has a `session_start`, and today both writers leave it at
    // zero: at the moment a header is written no exchange clock has been
    // observed yet, and inventing one would be a receive time wearing an
    // exchange time's name. So the span below is derived from the records,
    // which is the only place the log actually establishes it.
    let mut span: Option<(i64, i64)> = None;
    for record in &records {
        if let Some(at) = record.event.as_inbound().and_then(|i| i.exchange_time()) {
            let nanos = at.to_nanos();
            span = Some(match span {
                None => (nanos, nanos),
                Some((first, last)) => (first, nanos.max(last)),
            });
        }
    }
    match span {
        Some((first, last)) => writeln!(
            out,
            "  exchange clock {first} .. {last}  ({} seconds of market)",
            (last - first) / 1_000_000_000
        )?,
        None => writeln!(out, "  no event in this log carries an exchange time")?,
    }
    for entry in &header.instruments {
        writeln!(
            out,
            "  {:>2}  {:<10} tick {}  lot {}  min {}",
            entry.id.raw(),
            symbol_of(&symbols, entry.id),
            entry.tick,
            entry.lot,
            entry.min_qty,
        )?;
    }
    writeln!(out)?;

    match args.view {
        View::Fills => print_fills(&mut out, &records, &symbols, args.from, args.to)?,
        View::All | View::Decisions => {
            print_trace(&mut out, &records, &symbols, args.view, args.from, args.to)?
        }
        View::Curve => unreachable!("handled above"),
    }

    // The summary comes from `report`, not from a second count here. Two
    // implementations of "how many orders was that" is how a report and a
    // session start disagreeing.
    let summary = summarize(&records, header.instruments.len(), MARK_RULE)
        .map_err(|e| format!("this recording does not add up: {e:?}"))?;
    writeln!(out)?;
    writeln!(out, "in total")?;
    writeln!(out, "  records            {}", summary.records)?;
    writeln!(out, "  inputs             {}", summary.inputs)?;
    writeln!(out, "  decisions          {}", summary.decisions)?;
    writeln!(out, "  orders submitted   {}", summary.orders_submitted)?;
    writeln!(out, "  fills              {}", summary.fills)?;
    writeln!(out, "  intents refused    {}", summary.intents_rejected)?;
    for (reason, count) in &summary.rejections {
        writeln!(out, "    {reason:?}: {count}")?;
    }
    let v = &summary.valuation;
    writeln!(out, "  realized           {}", summary.realized)?;
    writeln!(out, "  unrealized         {}", v.unrealized)?;
    writeln!(out, "  fees               {}", summary.fees)?;
    writeln!(out, "  TOTAL              {}", v.total)?;
    writeln!(out, "  marked at          {:?}", v.rule)?;
    if !v.is_complete() {
        writeln!(out, "  !! INCOMPLETE      no mark for:")?;
        for instrument in &v.unmarked {
            writeln!(
                out,
                "                       {}",
                symbol_of(&symbols, *instrument)
            )?;
        }
        writeln!(
            out,
            "                     the total above excludes those positions"
        )?;
    }
    writeln!(out, "  max drawdown       {}", summary.max_drawdown)?;
    if summary.book_resets > 0 {
        writeln!(out, "  book resets        {}", summary.book_resets)?;
    }
    if summary.book_resyncs > 0 {
        // A reset after data has already flowed is a checksum catching our
        // book disagreeing with the venue's. Every decision before it was made
        // against a book that was wrong, which is worth saying loudly.
        writeln!(
            out,
            "  !! BOOK RESYNCS    {} — the venue caught a wrong book that many times",
            summary.book_resyncs
        )?;
    }
    match summary.feed_lag {
        Some(lag) => {
            // How stale the market data was when it arrived. A property of the
            // feed that produced this recording, not of the run.
            writeln!(
                out,
                "  feed lag           p50 {}  p90 {}  p99 {}  max {}  over {} events",
                millis(lag.p50),
                millis(lag.p90),
                millis(lag.p99),
                millis(lag.max),
                lag.events
            )?;
            // An hour is far beyond any feed problem. It means the exchange
            // timestamps did not come from the session that recorded them —
            // synthetic data, or a file replayed long after it was captured —
            // and the figures above are the gap between two unrelated days
            // rather than a measurement of a feed.
            const AN_HOUR: i64 = 3_600 * 1_000_000_000;
            if lag.p50 > AN_HOUR {
                writeln!(
                    out,
                    "  !! NOT A LIVE FEED these stamps are {} days apart, so this",
                    lag.p50 / (86_400 * 1_000_000_000)
                )?;
                writeln!(
                    out,
                    "                     recording's market data was synthetic or replayed"
                )?;
                writeln!(
                    out,
                    "                     and the lag above measures nothing about a feed"
                )?;
            }
            if lag.min < 0 {
                // The one condition that makes every other figure here a lie.
                writeln!(
                    out,
                    "  !! CLOCK SKEW      an event arrived {} before the venue stamped it",
                    millis(-lag.min)
                )?;
            }
        }
        None => writeln!(out, "  feed lag           no market events to measure")?,
    }
    writeln!(out, "  final state        {:?}", summary.final_state)?;
    for position in summary.positions.iter() {
        if position.is_flat() && position.realized() == Notional::ZERO {
            continue;
        }
        let index = position.instrument().raw() as usize;
        match v.unrealized_each.get(index).copied().flatten() {
            Some(unrealized) => writeln!(
                out,
                "  {:<10} position {}  realized {}  unrealized {}  total {}",
                symbol_of(&symbols, position.instrument()),
                position.qty(),
                position.realized(),
                unrealized,
                position.realized() + unrealized,
            )?,
            None => writeln!(
                out,
                "  {:<10} position {}  realized {}  unrealized UNKNOWN (no mark)",
                symbol_of(&symbols, position.instrument()),
                position.qty(),
                position.realized(),
            )?,
        }
    }
    Ok(out.flush()?)
}

/// Nanoseconds as milliseconds, which is the scale a feed lag lives at.
///
/// Three decimal places kept: a streaming feed's lag is single-digit
/// milliseconds and rounding to whole ones would print most of it as zero.
fn millis(nanos: i64) -> String {
    let sign = if nanos < 0 { "-" } else { "" };
    let n = nanos.unsigned_abs();
    format!("{sign}{}.{:03}ms", n / 1_000_000, (n % 1_000_000) / 1_000)
}

fn symbol_of(symbols: &[String], instrument: InstrumentId) -> &str {
    symbols
        .get(instrument.raw() as usize)
        .map(String::as_str)
        .unwrap_or("?")
}

/// Walks the log and prints one line per record.
fn print_trace(
    out: &mut impl Write,
    records: &[Record],
    symbols: &[String],
    view: View,
    from: u64,
    to: u64,
) -> io::Result<()> {
    writeln!(out, "{:>7}  {:<8} {:<10} what", "seq", "kind", "instrument")?;
    for record in records {
        let seq = record.seq.raw();
        if seq < from || seq > to {
            continue;
        }
        let Some((kind, instrument, what, caused_by)) = describe(record, symbols, view) else {
            continue;
        };
        let cause = caused_by.map(|c| format!("   <- {c}")).unwrap_or_default();
        // `Seq` renders itself as `#12` without honouring a width, so the
        // column is padded here rather than in the format string.
        writeln!(
            out,
            "{:>7}  {kind:<8} {instrument:<10} {what}{cause}",
            record.seq.to_string()
        )?;
    }
    Ok(())
}

/// One line's worth of a record, or `None` if this view hides it.
fn describe(
    record: &Record,
    symbols: &[String],
    view: View,
) -> Option<(&'static str, String, String, Option<Seq>)> {
    let any = view == View::All;
    match &record.event {
        Event::Out(Outbound::OrderSubmitted {
            caused_by,
            order,
            instrument,
            side,
            qty,
            kind,
            reduce_only,
            ..
        }) => {
            let price = match kind {
                event::OrderKind::Market => "market".to_string(),
                event::OrderKind::Limit(px) => format!("limit {px}"),
            };
            let flag = if *reduce_only { "  reduce-only" } else { "" };
            Some((
                "order",
                symbol_of(symbols, *instrument).to_string(),
                format!("{order} {side:?} {qty} {price}{flag}"),
                Some(*caused_by),
            ))
        }
        Event::Out(Outbound::CancelSubmitted { caused_by, order }) => Some((
            "cancel",
            String::new(),
            format!("{order}"),
            Some(*caused_by),
        )),
        Event::Out(Outbound::IntentRejected {
            caused_by,
            instrument,
            side,
            qty,
            reason,
            ..
        }) => Some((
            "refused",
            symbol_of(symbols, *instrument).to_string(),
            format!("{side:?} {qty} — {reason:?}"),
            Some(*caused_by),
        )),
        Event::Out(Outbound::StateChanged {
            caused_by,
            from,
            to,
            reason,
        }) => Some((
            "state",
            String::new(),
            format!("{from:?} -> {to:?} ({reason:?})"),
            Some(*caused_by),
        )),
        Event::Out(Outbound::TimerRequested { caused_by, at, .. }) => any.then(|| {
            (
                "timer",
                String::new(),
                format!("wake at {}", at.to_nanos()),
                Some(*caused_by),
            )
        }),

        Event::In(Inbound::Venue(venue)) => {
            let what = match venue.kind {
                VenueKind::Accepted => "accepted".to_string(),
                VenueKind::Rejected { reason } => format!("rejected — {reason:?}"),
                VenueKind::Filled { px, qty, fee } => format!("filled {qty} @ {px}  fee {fee}"),
                VenueKind::Cancelled => "cancelled".to_string(),
                VenueKind::CancelRejected { reason } => format!("cancel rejected — {reason:?}"),
                VenueKind::Expired => "expired".to_string(),
            };
            Some((
                "venue",
                String::new(),
                format!("{} {what}", venue.order),
                None,
            ))
        }
        Event::In(Inbound::Command(command)) => Some((
            "command",
            String::new(),
            match command.command {
                Command::Halt => "halt",
                Command::Resume => "resume",
                Command::Kill => "kill",
                Command::Flatten => "flatten",
            }
            .to_string(),
            None,
        )),
        Event::In(Inbound::VenuePosition(report)) => Some((
            "recon",
            symbol_of(symbols, report.instrument).to_string(),
            format!("venue says {}", report.venue_qty),
            None,
        )),

        // Market data and timers are the bulk of any recording, so they stay
        // out of the way unless asked for. Everything above is rare enough
        // that hiding it would just mean missing it.
        Event::In(Inbound::Market(market)) => any.then(|| {
            let what = match market.kind {
                MarketKind::Quote {
                    bid_px,
                    bid_qty,
                    ask_px,
                    ask_qty,
                } => format!("quote {bid_px} x {bid_qty}  /  {ask_px} x {ask_qty}"),
                MarketKind::Trade { px, qty, aggressor } => {
                    format!("trade {qty} @ {px}, {aggressor:?} took")
                }
                MarketKind::Level { side, px, qty } if qty == Qty::ZERO => {
                    format!("level {side:?} {px} removed")
                }
                MarketKind::Level { side, px, qty } => format!("level {side:?} {px} x {qty}"),
                MarketKind::BookApplied => "book update applied".to_string(),
                MarketKind::BookReset => "book reset, a snapshot follows".to_string(),
            };
            (
                "market",
                symbol_of(symbols, market.instrument).to_string(),
                what,
                None,
            )
        }),
        Event::In(Inbound::Timer(timer)) => any.then(|| {
            (
                "timer",
                String::new(),
                format!(
                    "fired {} at {}",
                    timer.token.raw(),
                    timer.fires_at.to_nanos()
                ),
                None,
            )
        }),
    }
}

/// Every fill, with the position and profit it produced.
fn print_fills(
    out: &mut impl Write,
    records: &[Record],
    symbols: &[String],
    from: u64,
    to: u64,
) -> Result<(), Fault> {
    writeln!(
        out,
        "{:>7}  {:<10} {:<5} {:>16} {:>18} {:>14} {:>16} {:>18}",
        "seq", "instrument", "side", "qty", "price", "fee", "position", "realized"
    )?;
    let mut books = Books::new(symbols.len());
    for record in records {
        let Some(fill) = books.apply(record)? else {
            continue;
        };
        let seq = record.seq.raw();
        if seq < from || seq > to {
            continue;
        }
        writeln!(
            out,
            "{:>7}  {:<10} {:<5} {:>16} {:>18} {:>14} {:>16} {:>18}",
            record.seq.to_string(),
            symbol_of(symbols, fill.instrument),
            format!("{:?}", fill.side),
            fill.qty,
            fill.px,
            fill.fee,
            fill.position,
            fill.realized,
        )?;
    }
    Ok(())
}

/// The realized-profit curve as CSV, one row per fill.
fn print_curve(out: &mut impl Write, records: &[Record], symbols: &[String]) -> Result<(), Fault> {
    writeln!(
        out,
        "seq,instrument,side,qty,price,fee,position,realized,total_realized"
    )?;
    let mut books = Books::new(symbols.len());
    for record in records {
        let Some(fill) = books.apply(record)? else {
            continue;
        };
        writeln!(
            out,
            "{},{},{:?},{},{},{},{},{},{}",
            record.seq.raw(),
            symbol_of(symbols, fill.instrument),
            fill.side,
            fill.qty,
            fill.px,
            fill.fee,
            fill.position,
            fill.realized,
            fill.total_realized,
        )?;
    }
    Ok(())
}

/// What one fill did.
struct Fill {
    instrument: InstrumentId,
    side: Side,
    qty: Qty,
    px: Px,
    fee: Notional,
    position: Qty,
    realized: Notional,
    total_realized: Notional,
}

/// Re-derives the books from the log, one record at a time.
///
/// The accounting is `oms`'s, not a second copy of it. All this adds is the
/// order-to-instrument mapping, which fill reports do not carry and only the
/// log can supply.
struct Books {
    positions: Positions,
    /// Order ids are a dense run from a base, so this is a slice rather than a
    /// map — and it stays in log order.
    base: Option<u64>,
    submitted: Vec<Option<(InstrumentId, Side)>>,
}

impl Books {
    fn new(instruments: usize) -> Books {
        Books {
            positions: Positions::with_instruments(instruments, 64),
            base: None,
            submitted: Vec::new(),
        }
    }

    /// Applies a record, returning what it did if it was a fill.
    fn apply(&mut self, record: &Record) -> Result<Option<Fill>, Fault> {
        match &record.event {
            Event::Out(Outbound::OrderSubmitted {
                order,
                instrument,
                side,
                ..
            }) => {
                let start = *self.base.get_or_insert(order.raw());
                let index = order.raw().saturating_sub(start) as usize;
                if self.submitted.len() <= index {
                    self.submitted.resize(index + 1, None);
                }
                self.submitted[index] = Some((*instrument, *side));
                Ok(None)
            }
            Event::In(Inbound::Venue(venue)) => {
                let VenueKind::Filled { px, qty, fee } = venue.kind else {
                    return Ok(None);
                };
                let (instrument, side) = self.order(venue.order)?;
                self.positions
                    .apply_fill(instrument, side, qty, px, fee)
                    .map_err(|e| format!("record {} does not add up: {e:?}", record.seq))?;
                let position = self
                    .positions
                    .get(instrument)
                    .ok_or_else(|| format!("record {} names an unknown instrument", record.seq))?;
                Ok(Some(Fill {
                    instrument,
                    side,
                    qty,
                    px,
                    fee,
                    position: position.qty(),
                    realized: position.realized(),
                    total_realized: self
                        .positions
                        .iter()
                        .fold(Notional::ZERO, |acc, p| acc + p.realized()),
                }))
            }
            _ => Ok(None),
        }
    }

    /// What an order traded, from the submission the log recorded.
    ///
    /// A fill naming an order that was never submitted means the log is
    /// internally inconsistent. Guessing would produce a number that looks
    /// authoritative and is not, so this refuses instead (`report`).
    fn order(&self, order: OrderId) -> Result<(InstrumentId, Side), Fault> {
        let index = self
            .base
            .map(|start| order.raw().saturating_sub(start) as usize)
            .unwrap_or(usize::MAX);
        match self.submitted.get(index) {
            Some(Some(pair)) => Ok(*pair),
            _ => Err(
                format!("{order} filled, but this log never recorded it being submitted").into(),
            ),
        }
    }
}

// ---- machine-readable output ---------------------------------------------

/// Quotes a string for JSON.
///
/// Symbols come from a recording's header, which is bytes a venue chose, so
/// this escapes rather than assuming they are tame.
fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The whole report as one JSON object.
///
/// **Every money and quantity value is a string**, never a JSON number. JSON
/// numbers are doubles, and putting a fixed-point price through one is exactly
/// the round trip the `Px` and `Qty` types exist to prevent — `0.00018080`
/// comes back `0.0001808`. A consumer that wants to plot a value parses it
/// itself and takes that on knowingly.
#[allow(clippy::too_many_arguments)]
fn print_json(
    out: &mut impl Write,
    args: &Args,
    header: &event::codec::LogHeader,
    symbols: &[String],
    records: &[Record],
    recovery: &event::Recovery,
    segments: usize,
    summary: &report::SessionReport,
) -> Result<(), Fault> {
    let v = &summary.valuation;

    writeln!(out, "{{")?;
    writeln!(out, "  \"session\": {{")?;
    writeln!(out, "    \"id\": \"{}\",", header.session_id)?;
    writeln!(out, "    \"format\": {},", header.format_version)?;
    writeln!(out, "    \"segments\": {segments},")?;
    writeln!(out, "    \"records\": {},", summary.records)?;
    writeln!(out, "    \"intact\": {},", recovery.is_clean())?;
    writeln!(
        out,
        "    \"discarded_bytes\": {},",
        recovery.discarded_bytes
    )?;
    // The span of market the recording covers, which is also how a viewer
    // decides whether a session is still running: the last one grows.
    let mut first = None;
    let mut last = None;
    for record in records {
        if let Some(at) = record.event.as_inbound().and_then(|i| i.exchange_time()) {
            first.get_or_insert(at.to_nanos());
            last = Some(at.to_nanos().max(last.unwrap_or(i64::MIN)));
        }
    }
    writeln!(out, "    \"first_event_nanos\": {},", first.unwrap_or(0))?;
    writeln!(out, "    \"last_event_nanos\": {}", last.unwrap_or(0))?;
    writeln!(out, "  }},")?;

    writeln!(out, "  \"instruments\": [")?;
    for (n, entry) in header.instruments.iter().enumerate() {
        let comma = if n + 1 == header.instruments.len() {
            ""
        } else {
            ","
        };
        writeln!(
            out,
            "    {{\"id\": {}, \"symbol\": {}, \"tick\": \"{}\", \"lot\": \"{}\"}}{comma}",
            entry.id.raw(),
            quoted(symbol_of(symbols, entry.id)),
            entry.tick,
            entry.lot
        )?;
    }
    writeln!(out, "  ],")?;

    writeln!(out, "  \"totals\": {{")?;
    writeln!(out, "    \"orders\": {},", summary.orders_submitted)?;
    writeln!(out, "    \"cancels\": {},", summary.cancels_submitted)?;
    writeln!(out, "    \"fills\": {},", summary.fills)?;
    writeln!(out, "    \"refused\": {},", summary.intents_rejected)?;
    writeln!(out, "    \"realized\": \"{}\",", summary.realized)?;
    writeln!(out, "    \"unrealized\": \"{}\",", v.unrealized)?;
    writeln!(out, "    \"fees\": \"{}\",", summary.fees)?;
    writeln!(out, "    \"total\": \"{}\",", v.total)?;
    writeln!(out, "    \"max_drawdown\": \"{}\",", summary.max_drawdown)?;
    writeln!(
        out,
        "    \"mark_rule\": {},",
        quoted(&format!("{:?}", v.rule))
    )?;
    writeln!(out, "    \"complete\": {},", v.is_complete())?;
    writeln!(
        out,
        "    \"state\": {}",
        quoted(&format!("{:?}", summary.final_state))
    )?;
    writeln!(out, "  }},")?;

    writeln!(out, "  \"refusals\": [")?;
    for (n, (reason, count)) in summary.rejections.iter().enumerate() {
        let comma = if n + 1 == summary.rejections.len() {
            ""
        } else {
            ","
        };
        writeln!(
            out,
            "    {{\"reason\": {}, \"count\": {count}}}{comma}",
            quoted(&format!("{reason:?}"))
        )?;
    }
    writeln!(out, "  ],")?;

    writeln!(out, "  \"positions\": [")?;
    let held: Vec<_> = summary.positions.iter().collect();
    for (n, position) in held.iter().enumerate() {
        let comma = if n + 1 == held.len() { "" } else { "," };
        let index = position.instrument().raw() as usize;
        let unrealized = v
            .unrealized_each
            .get(index)
            .copied()
            .flatten()
            .map(|u| format!("\"{u}\""))
            .unwrap_or_else(|| "null".to_string());
        let mark = v
            .marks
            .get(index)
            .copied()
            .flatten()
            .map(|m| format!("\"{}\"", m.px))
            .unwrap_or_else(|| "null".to_string());
        writeln!(
            out,
            "    {{\"symbol\": {}, \"position\": \"{}\", \"realized\": \"{}\", \
             \"unrealized\": {unrealized}, \"mark\": {mark}}}{comma}",
            quoted(symbol_of(symbols, position.instrument())),
            position.qty(),
            position.realized()
        )?;
    }
    writeln!(out, "  ],")?;

    writeln!(out, "  \"book\": {{")?;
    writeln!(out, "    \"resets\": {},", summary.book_resets)?;
    writeln!(out, "    \"resyncs\": {}", summary.book_resyncs)?;
    writeln!(out, "  }},")?;

    match summary.feed_lag {
        Some(lag) => {
            writeln!(out, "  \"feed_lag_nanos\": {{")?;
            writeln!(out, "    \"events\": {},", lag.events)?;
            writeln!(out, "    \"min\": {},", lag.min)?;
            writeln!(out, "    \"p50\": {},", lag.p50)?;
            writeln!(out, "    \"p90\": {},", lag.p90)?;
            writeln!(out, "    \"p99\": {},", lag.p99)?;
            writeln!(out, "    \"max\": {}", lag.max)?;
            write!(out, "  }}")?;
        }
        None => write!(out, "  \"feed_lag_nanos\": null")?,
    }

    // The book as the recording leaves it, with this session's own resting
    // orders marked. "Where are my quotes against everyone else's" is the
    // question a ladder is for, and neither half answers it alone.
    writeln!(out, ",")?;
    // `ladder`, not `book`: the reset counts above already claim that name, and
    // two keys with one name in a JSON object is a key silently lost. This one
    // was — every parser keeps the last, so the reset alert read `undefined`
    // from the moment the ladder was added.
    writeln!(out, "  \"ladder\": [")?;
    let mut books: Vec<String> = Vec::new();
    for entry in &header.instruments {
        let side_json = |side: types::Side| -> String {
            summary
                .book
                .depth(entry.id, side)
                .map(|d| {
                    d.levels()
                        .iter()
                        .map(|l| format!("{{\"px\": \"{}\", \"qty\": \"{}\"}}", l.px, l.qty))
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        };
        let mine: Vec<String> = summary
            .working
            .iter()
            .filter(|o| o.instrument == entry.id)
            .filter_map(|o| {
                o.limit.map(|px| {
                    format!(
                        "{{\"order\": \"{}\", \"side\": {}, \"px\": \"{px}\", \"qty\": \"{}\"}}",
                        o.order,
                        quoted(&format!("{:?}", o.side)),
                        o.remaining
                    )
                })
            })
            .collect();
        books.push(format!(
            "    {{\"symbol\": {}, \"bids\": [{}], \"asks\": [{}], \"working\": [{}]}}",
            quoted(symbol_of(symbols, entry.id)),
            side_json(types::Side::Buy),
            side_json(types::Side::Sell),
            mine.join(", ")
        ));
    }
    writeln!(out, "{}", books.join(",\n"))?;
    write!(out, "  ]")?;

    // The fills, when asked for. Left out by default because a long session
    // has tens of thousands and a caller that only wants the totals should not
    // pay for them.
    if matches!(args.view, View::Fills | View::Curve) {
        writeln!(out, ",")?;
        let mut books = Books::new(symbols.len());
        let mut rows: Vec<String> = Vec::new();
        for record in records {
            let Some(fill) = books.apply(record)? else {
                continue;
            };
            rows.push(format!(
                "    {{\"seq\": {}, \"symbol\": {}, \"side\": {}, \"qty\": \"{}\", \
                 \"px\": \"{}\", \"fee\": \"{}\", \"position\": \"{}\", \
                 \"realized\": \"{}\", \"total_realized\": \"{}\"}}",
                record.seq.raw(),
                quoted(symbol_of(symbols, fill.instrument)),
                quoted(&format!("{:?}", fill.side)),
                fill.qty,
                fill.px,
                fill.fee,
                fill.position,
                fill.realized,
                fill.total_realized,
            ));
        }
        // Only the most recent, and the count of what was left out. A trimmed
        // array that did not say so would look like a session that traded less
        // than it did.
        let omitted = rows.len().saturating_sub(args.tail);
        if omitted > 0 {
            rows.drain(..omitted);
        }
        writeln!(out, "  \"fills_omitted\": {omitted},")?;
        writeln!(out, "  \"fills\": [")?;
        writeln!(out, "{}", rows.join(",\n"))?;
        write!(out, "  ]")?;
    }

    writeln!(out)?;
    writeln!(out, "}}")?;
    Ok(())
}
