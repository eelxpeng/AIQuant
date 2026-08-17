//! Run many configurations over one recording and compare them.
//!
//! ```text
//! sweep <recorded.log> <base.conf> [--vary SYM.setting=a,b,c]... [--split F]
//! ```
//!
//! One `backtest` answers "what would this policy have done". A sweep answers
//! "which of these policies", which is the question anyone actually has, and
//! doing it by hand means running the same command twenty times and comparing
//! numbers by eye.
//!
//! # The uncomfortable part
//!
//! **Picking the best row of a sweep is how strategies get overfitted.** Run
//! enough variants over one recording and the top one is partly measuring the
//! noise in that particular stretch of market. This tool cannot stop that, so
//! it does the next best thing: `--split` measures every variant on a stretch
//! it was not chosen on, and prints both columns side by side. When the
//! ranking rearranges between them — and it usually does — that is the tool
//! telling you the in-sample winner was luck.
//!
//! A sweep without `--split` prints a warning saying so. That is deliberate
//! nagging.
//!
//! # What can be varied
//!
//! Any setting the config file names, on any strategy in it:
//!
//! ```text
//! sweep session.log base.conf --vary BTCUSD.window=10,20,30
//! sweep session.log base.conf --vary ETHUSD.half-spread=0.25,0.5 --vary ETHUSD.size=1,2
//! ```
//!
//! Several `--vary` flags multiply out: two settings with three values each is
//! nine runs. A setting the named strategy does not take is refused, because a
//! silently ignored variation prints a grid of runs that were all identical.

#![forbid(unsafe_code)]

use std::io::{self, Write};

use config::{SessionConfig, StrategyConfig};
use event::codec::LogHeader;
use event::{Record, Segments};
use harness::{Costs, backtest, split_by_market_events};
use marketdata::MarkRule;
use report::SessionReport;
use types::{Notional, RoundDir};

/// How an open position is valued, matching `backtest` and the engine's own
/// exposure check so two tools cannot disagree about what a position is worth.
const MARK_RULE: MarkRule = MarkRule::Mid(RoundDir::Down);

/// One setting, and the values to try for it.
struct Axis {
    symbol: String,
    setting: String,
    values: Vec<String>,
}

/// One point in the grid: which value each axis took.
type Point = Vec<String>;

struct Args {
    path: String,
    conf: String,
    axes: Vec<Axis>,
    split: Option<f64>,
    json: bool,
}

fn usage() -> ! {
    eprintln!("usage: sweep <recorded.log> <base.conf> [--vary SYM.setting=a,b,c]... [--split F]");
    eprintln!();
    eprintln!("  --vary   a setting to try several values of; repeatable, and");
    eprintln!("           several flags multiply out into a grid");
    eprintln!("  --json   the ranked grid as JSON, for a caller that sorts it itself");
    eprintln!("  --split  fraction of the recording to choose on, measuring the");
    eprintln!("           rest out of sample. 0.7 is a reasonable default.");
    eprintln!();
    eprintln!("  sweep session.log base.conf --vary BTCUSD.window=10,20,30 --split 0.7");
    std::process::exit(2);
}

fn fail(context: &str, e: impl std::fmt::Display) -> ! {
    eprintln!("sweep: {context}: {e}");
    std::process::exit(1);
}

fn parse_args() -> Args {
    let mut positional: Vec<String> = Vec::new();
    let mut axes = Vec::new();
    let mut split = None;
    let mut json = false;

    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--vary" => {
                let spec = argv.next().unwrap_or_else(|| usage());
                let (target, values) = spec
                    .split_once('=')
                    .unwrap_or_else(|| fail("--vary wants SYM.setting=a,b,c", &spec));
                let (symbol, setting) = target
                    .split_once('.')
                    .unwrap_or_else(|| fail("--vary wants SYM.setting=a,b,c", &spec));
                let values: Vec<String> = values.split(',').map(str::to_string).collect();
                if values.iter().any(|v| v.is_empty()) {
                    fail("--vary has an empty value", &spec);
                }
                axes.push(Axis {
                    symbol: symbol.to_string(),
                    setting: setting.to_string(),
                    values,
                });
            }
            "--json" => json = true,
            "--split" => {
                let raw = argv.next().unwrap_or_else(|| usage());
                let value: f64 = raw
                    .parse()
                    .unwrap_or_else(|_| fail("--split wants a fraction like 0.7", &raw));
                if !(value > 0.0 && value < 1.0) {
                    fail("--split must be between 0 and 1, exclusive", &raw);
                }
                split = Some(value);
            }
            other if other.starts_with("--") => fail("unknown option", other),
            other => positional.push(other.to_string()),
        }
    }
    if positional.len() != 2 {
        usage();
    }
    Args {
        path: positional[0].clone(),
        conf: positional[1].clone(),
        axes,
        split,
        json,
    }
}

/// Every combination of the axes, in a stable order.
fn grid(axes: &[Axis]) -> Vec<Point> {
    let mut points: Vec<Point> = vec![Vec::new()];
    for axis in axes {
        let mut next = Vec::with_capacity(points.len() * axis.values.len());
        for point in &points {
            for value in &axis.values {
                let mut extended = point.clone();
                extended.push(value.clone());
                next.push(extended);
            }
        }
        points = next;
    }
    points
}

/// The base config with this point's settings applied.
fn configure(base: &SessionConfig, axes: &[Axis], point: &Point) -> Result<SessionConfig, String> {
    let mut session = base.clone();
    for (axis, value) in axes.iter().zip(point) {
        let mut hit = false;
        let mut updated: Vec<StrategyConfig> = Vec::with_capacity(session.strategies.len());
        for spec in &session.strategies {
            if spec.symbol == axis.symbol {
                hit = true;
                updated.push(
                    spec.with(&axis.setting, value)
                        .map_err(|e| format!("{}.{}: {e}", axis.symbol, axis.setting))?,
                );
            } else {
                updated.push(spec.clone());
            }
        }
        if !hit {
            // A typo in a symbol would otherwise produce a grid of runs that
            // were all the base config, ranked against each other.
            return Err(format!("no strategy in the config trades {}", axis.symbol));
        }
        session.strategies = updated;
    }
    Ok(session)
}

/// What one run is worth, and how much it hurt getting there.
struct Row {
    label: String,
    inside: SessionReport,
    outside: Option<SessionReport>,
}

fn label(axes: &[Axis], point: &Point) -> String {
    if axes.is_empty() {
        return "base".to_string();
    }
    axes.iter()
        .zip(point)
        .map(|(axis, value)| format!("{}={}", axis.setting, value))
        .collect::<Vec<_>>()
        .join(" ")
}

fn main() {
    let args = parse_args();

    let root = std::path::Path::new(&args.path);
    let header: LogHeader = match Segments::header(root) {
        Ok(h) => h,
        Err(e) => fail(&format!("cannot read {}", args.path), e),
    };
    let (records, recovery) = match Segments::read_all(root) {
        Ok(pair) => pair,
        Err(e) => fail(&format!("cannot read {}", args.path), e),
    };
    if !recovery.is_clean() {
        // A sweep over a truncated recording still ranks, but every row is
        // measured on less market than the operator thinks.
        eprintln!("sweep: warning: {recovery}");
    }

    let base = SessionConfig::load(&args.conf)
        .unwrap_or_else(|e| fail(&format!("cannot read {}", args.conf), e));

    let (inside, outside): (&[Record], Option<&[Record]>) = match args.split {
        None => (&records, None),
        Some(fraction) => match split_by_market_events(&records, fraction) {
            Some((a, b)) => (a, Some(b)),
            None => fail(
                "cannot split this recording",
                "it does not hold enough market data for two halves",
            ),
        },
    };

    let points = grid(&args.axes);
    let mut rows: Vec<Row> = Vec::with_capacity(points.len());
    for point in &points {
        let session = configure(&base, &args.axes, point)
            .unwrap_or_else(|e| fail("cannot apply a variation", e));
        let run_inside = backtest(&header, inside, &session, Costs::DEFAULT, MARK_RULE)
            .unwrap_or_else(|e| fail("a run failed", e));
        let run_outside = outside.map(|records| {
            backtest(&header, records, &session, Costs::DEFAULT, MARK_RULE)
                .unwrap_or_else(|e| fail("an out-of-sample run failed", e))
        });
        rows.push(Row {
            label: label(&args.axes, point),
            inside: run_inside,
            outside: run_outside,
        });
    }

    // A closed reader is not a failure: `sweep … | head` got what it asked for.
    if let Err(e) = print(&args, &header, &records, inside, outside, &mut rows)
        && e.kind() != io::ErrorKind::BrokenPipe
    {
        fail("cannot write the table", e);
    }
}

fn print(
    args: &Args,
    header: &LogHeader,
    all: &[Record],
    inside: &[Record],
    outside: Option<&[Record]>,
    rows: &mut [Row],
) -> io::Result<()> {
    let stdout = io::stdout();
    let mut out = stdout.lock();

    // Ranked by what the run was actually worth, not by realized profit: a
    // variant that ends holding a large position has not finished yet.
    rows.sort_by(|a, b| {
        b.inside
            .valuation
            .total
            .to_scaled()
            .cmp(&a.inside.valuation.total.to_scaled())
    });

    if args.json {
        return print_json(&mut out, args, rows).and_then(|()| out.flush());
    }

    writeln!(out, "sweep over {}", args.path)?;
    writeln!(out, "  session id     {}", header.session_id)?;
    writeln!(out, "  records        {}", all.len())?;
    writeln!(out, "  runs           {}", rows.len())?;
    match outside {
        Some(rest) => writeln!(
            out,
            "  split          {} records in sample, {} out",
            inside.len(),
            rest.len()
        )?,
        None => writeln!(out, "  split          none — see the warning below")?,
    }
    writeln!(out)?;

    let width = rows.iter().map(|r| r.label.len()).max().unwrap_or(4).max(4);
    if outside.is_some() {
        writeln!(
            out,
            "{:<width$}  {:>14} {:>12} {:>7} {:>6}   {:>14} {:>12}",
            "variant", "in total", "in drawdown", "fills", "ref", "OUT total", "out drawdown"
        )?;
    } else {
        writeln!(
            out,
            "{:<width$}  {:>14} {:>12} {:>7} {:>6}",
            "variant", "total", "drawdown", "fills", "ref"
        )?;
    }

    for row in rows.iter() {
        let v = &row.inside;
        let incomplete = if v.valuation.is_complete() {
            ""
        } else {
            "  !! unmarked"
        };
        match &row.outside {
            Some(o) => writeln!(
                out,
                "{:<width$}  {:>14} {:>12} {:>7} {:>6}   {:>14} {:>12}{}",
                row.label,
                v.valuation.total.to_string(),
                v.max_drawdown.to_string(),
                v.fills,
                v.intents_rejected,
                o.valuation.total.to_string(),
                o.max_drawdown.to_string(),
                incomplete,
            )?,
            None => writeln!(
                out,
                "{:<width$}  {:>14} {:>12} {:>7} {:>6}{}",
                row.label,
                v.valuation.total.to_string(),
                v.max_drawdown.to_string(),
                v.fills,
                v.intents_rejected,
                incomplete,
            )?,
        }
    }

    writeln!(out)?;
    match outside {
        Some(_) => report_rank_change(&mut out, rows)?,
        None => {
            writeln!(
                out,
                "!! No out-of-sample split. Picking the top row of this table is how a\n\
                 !! strategy gets overfitted: with enough variants the winner is partly\n\
                 !! measuring the noise in this particular recording. Re-run with\n\
                 !! --split 0.7 to measure every variant on market it was not chosen on."
            )?;
        }
    }
    out.flush()
}

/// Says whether the in-sample winner survived out of sample.
///
/// The single most useful line in the output, and the reason `--split` exists.
/// The ranked grid as JSON, for a caller that wants to sort it themselves.
///
/// Money is emitted as strings for the same reason `journal` does it: a JSON
/// number is a double, and a fixed-point value that goes through one is not
/// the value any more.
fn print_json(out: &mut impl Write, args: &Args, rows: &[Row]) -> io::Result<()> {
    writeln!(out, "{{")?;
    writeln!(out, "  \"recording\": {:?},", args.path)?;
    writeln!(
        out,
        "  \"split\": {},",
        args.split
            .map(|f| format!("{f}"))
            .unwrap_or_else(|| "null".to_string())
    )?;
    writeln!(out, "  \"runs\": [")?;
    for (n, row) in rows.iter().enumerate() {
        let comma = if n + 1 == rows.len() { "" } else { "," };
        let out_total = row
            .outside
            .as_ref()
            .map(|o| format!("\"{}\"", o.valuation.total))
            .unwrap_or_else(|| "null".to_string());
        let out_dd = row
            .outside
            .as_ref()
            .map(|o| format!("\"{}\"", o.max_drawdown))
            .unwrap_or_else(|| "null".to_string());
        writeln!(
            out,
            "    {{\"variant\": {:?}, \"in_total\": \"{}\", \"in_drawdown\": \"{}\",              \"fills\": {}, \"refused\": {}, \"out_total\": {out_total},              \"out_drawdown\": {out_dd}, \"complete\": {}}}{comma}",
            row.label,
            row.inside.valuation.total,
            row.inside.max_drawdown,
            row.inside.fills,
            row.inside.intents_rejected,
            row.inside.valuation.is_complete(),
        )?;
    }
    writeln!(out, "  ]")?;
    writeln!(out, "}}")?;
    Ok(())
}

fn report_rank_change(out: &mut impl Write, rows: &[Row]) -> io::Result<()> {
    let Some(best_inside) = rows.first() else {
        return Ok(());
    };
    let Some(best_outside) = rows.iter().max_by_key(|r| {
        r.outside
            .as_ref()
            .map(|o| o.valuation.total.to_scaled())
            .unwrap_or(i128::MIN)
    }) else {
        return Ok(());
    };

    if best_inside.label == best_outside.label {
        writeln!(
            out,
            "The best variant in sample was also the best out of sample ({}).\n\
             That is weak evidence it is real, not proof: one split is one experiment.",
            best_inside.label
        )?;
    } else {
        let inside_out = best_inside
            .outside
            .as_ref()
            .map(|o| o.valuation.total)
            .unwrap_or(Notional::ZERO);
        writeln!(
            out,
            "!! The ranking rearranged. In sample the best was {}; out of sample it\n\
             !! returned {} and {} did better. Choosing on the in-sample column\n\
             !! would have picked the wrong one, which is what overfitting looks like.",
            best_inside.label, inside_out, best_outside.label
        )?;
    }
    Ok(())
}
