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
}

fn parse_args() -> Result<Args, Fault> {
    let mut path = None;
    let mut view = View::Decisions;
    let mut from = 0u64;
    let mut to = u64::MAX;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--all" => view = View::All,
            "--fills" => view = View::Fills,
            "--curve" => view = View::Curve,
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
        path: path.ok_or("usage: journal <recorded.log> [--all|--fills|--curve]")?,
        view,
        from,
        to,
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
