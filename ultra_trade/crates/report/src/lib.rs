//! Turns a finished session's log into numbers.
//!
//! A log consumer, nothing more. It reaches no engine state and holds no
//! reference to a running session — it walks records and re-derives everything
//! (`docs/ARCHITECTURE.md`).
//!
//! It re-derives position and PnL through `oms`'s accounting rather than
//! reimplementing it. A second implementation of "what did this fill do to the
//! books" is a second answer waiting to disagree with the first.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use event::{EngineState, Event, Inbound, Outbound, Record, RiskReason, VenueKind};
use marketdata::{Books, MarkRule};
use oms::{PositionError, Positions};
use types::{ExchangeTime, InstrumentId, Notional, OrderId, Px, Qty, Side, feed_lag_nanos};

/// What a finished session did.
#[derive(Debug, Clone)]
pub struct SessionReport {
    /// Records in the log.
    pub records: usize,
    /// Inputs the session received.
    pub inputs: usize,
    /// Decisions the session made.
    pub decisions: usize,
    /// Orders that reached the venue.
    pub orders_submitted: usize,
    /// Cancels that reached the venue.
    pub cancels_submitted: usize,
    /// Intents the risk gate refused.
    pub intents_rejected: usize,
    /// Fills the venue reported.
    pub fills: usize,
    /// Total quantity filled, as a magnitude.
    pub filled_qty: Qty,
    /// Profit from closed quantity, fees included.
    pub realized: Notional,
    /// Fees paid.
    pub fees: Notional,
    /// The deepest fall from a peak, marked to market.
    ///
    /// Sampled on `realized + unrealized` at every event that can move either:
    /// a fill, and a market event that moves the mark. Realized-only was the
    /// earlier measure and it understated every drawdown a session rode out in
    /// an open position — which is most of them, because a strategy that is
    /// down usually still holds the thing it is down on.
    ///
    /// Sampling begins once a mark exists. Before that the position cannot be
    /// valued, and a fabricated early point would put a cliff in the curve at
    /// the moment the first quote arrived.
    pub max_drawdown: Notional,
    /// Where the engine ended up.
    pub final_state: EngineState,
    /// How often each refusal fired, in `RiskReason` order.
    ///
    /// A refusal with a surprising count is usually a misconfiguration rather
    /// than a market condition, which is why they are counted separately
    /// rather than summed.
    pub rejections: Vec<(RiskReason, usize)>,
    /// Per-instrument accounting, re-derived from the fills in the log.
    pub positions: Positions,
    /// What the open positions are worth.
    pub valuation: Valuation,
    /// How stale this recording's market data was when it arrived.
    ///
    /// `None` when the recording holds no market events, which is a different
    /// condition from "the feed was instant" and must not read as it.
    pub feed_lag: Option<FeedLag>,
}

/// A valuation price, and the moment it was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mark {
    /// The price the position is valued at.
    pub px: Px,
    /// The exchange time of the event that produced it.
    ///
    /// A mark without an "as of" is not checkable, and a stale one looks
    /// exactly like a fresh one until somebody asks.
    pub at: ExchangeTime,
}

/// What the open positions are worth.
///
/// Realized profit alone is not a result. A session that closed out for a
/// small gain and one sitting on a large loss can print the same realized
/// number, and ranking two strategies on it gets the order wrong. Every
/// session recorded so far has ended holding something.
#[derive(Debug, Clone)]
pub struct Valuation {
    /// The rule the caller chose.
    ///
    /// Carried so a report can say which it used. Two rules give two different
    /// answers over the same log, so the number means nothing without it.
    pub rule: MarkRule,
    /// The mark per instrument, in id order. `None` where the log never
    /// established one.
    pub marks: Vec<Option<Mark>>,
    /// Profit on each instrument's open position, in id order.
    ///
    /// `None`, not zero, where there was no mark. Comparing two strategies is
    /// the main thing anyone does with these, and that comparison is only
    /// meaningful per instrument — a session total hides which one earned it.
    pub unrealized_each: Vec<Option<Notional>>,
    /// Profit on the open positions, over instruments that have a mark.
    pub unrealized: Notional,
    /// Realized plus unrealized, over instruments that have a mark.
    pub total: Notional,
    /// Instruments holding a position that could not be valued.
    ///
    /// Named rather than counted as zero. Zero is a number someone will add
    /// up, and a missing mark is not worth nothing (Constitution V). An
    /// instrument that is flat is not listed: it has nothing to value, and a
    /// list that cries wolf is a list people stop reading.
    pub unmarked: Vec<InstrumentId>,
}

impl Valuation {
    /// Whether every position could be valued.
    ///
    /// `false` means [`total`] is missing something, and presenting it as the
    /// session's result would overstate how much is known.
    ///
    /// [`total`]: Valuation::total
    #[inline]
    pub fn is_complete(&self) -> bool {
        self.unmarked.is_empty()
    }
}

/// How far behind the venue this recording's market data arrived.
///
/// A property of the **feed that produced the recording**, not of the session
/// that traded on it — a backtest over the same file reports the same numbers,
/// because it replays the same receive times.
///
/// Every event has carried both clocks since the first release and nothing
/// ever compared them, so this is the first time the question "how stale is
/// our view of the market" has an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedLag {
    /// Market events measured.
    pub events: usize,
    /// The smallest lag seen, in nanoseconds. Negative means clock skew.
    pub min: i64,
    /// Median.
    pub p50: i64,
    /// Ninth decile.
    pub p90: i64,
    /// Ninety-ninth percentile — the tail that a latency budget lives or dies
    /// on, and the one an average hides.
    pub p99: i64,
    /// The worst single event.
    pub max: i64,
}

/// Why a log could not be summarized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReportError {
    /// A fill named an order the log never recorded being submitted.
    ///
    /// The log is internally inconsistent. Guessing what the fill meant would
    /// produce a number that looks authoritative and is not.
    OrphanFill(OrderId),
    /// Position accounting refused a fill from the log.
    Position(PositionError),
}

impl From<PositionError> for ReportError {
    fn from(e: PositionError) -> ReportError {
        ReportError::Position(e)
    }
}

/// Walks a log and reports what the session did.
///
/// `instrument_count` sizes the accounting, and must cover every instrument the
/// session traded.
pub fn summarize(
    records: &[Record],
    instrument_count: usize,
    rule: MarkRule,
) -> Result<SessionReport, ReportError> {
    let mut report = SessionReport {
        records: records.len(),
        inputs: 0,
        decisions: 0,
        orders_submitted: 0,
        cancels_submitted: 0,
        intents_rejected: 0,
        fills: 0,
        filled_qty: Qty::ZERO,
        realized: Notional::ZERO,
        fees: Notional::ZERO,
        max_drawdown: Notional::ZERO,
        final_state: EngineState::Running,
        rejections: Vec::new(),
        positions: Positions::with_instruments(instrument_count, 64),
        feed_lag: None,
        valuation: Valuation {
            rule,
            marks: vec![None; instrument_count],
            unrealized_each: vec![None; instrument_count],
            unrealized: Notional::ZERO,
            total: Notional::ZERO,
            unmarked: Vec::new(),
        },
    };

    // Kept unsorted while walking and sorted once at the end: a percentile
    // needs the whole set, and this is a report rather than a hot path.
    let mut lags: Vec<i64> = Vec::new();

    // The book is rebuilt rather than tracked by hand, so "what is the mark"
    // has one definition and the report cannot disagree with the session about
    // it — including which events the book *refused* as out of order.
    let mut books = Books::with_instruments(instrument_count);

    // Order ids are a dense run from a base, so remembering what each order
    // traded is a slice rather than a map — and it stays in log order.
    let mut base: Option<u64> = None;
    let mut submitted: Vec<Option<(InstrumentId, Side)>> = Vec::new();

    let mut peak = Notional::ZERO;

    for record in records {
        match &record.event {
            Event::Out(out) => {
                report.decisions += 1;
                match *out {
                    Outbound::OrderSubmitted {
                        order,
                        instrument,
                        side,
                        ..
                    } => {
                        report.orders_submitted += 1;
                        let start = *base.get_or_insert(order.raw());
                        let index = (order.raw().saturating_sub(start)) as usize;
                        if submitted.len() <= index {
                            submitted.resize(index + 1, None);
                        }
                        submitted[index] = Some((instrument, side));
                    }
                    Outbound::CancelSubmitted { .. } => report.cancels_submitted += 1,
                    Outbound::IntentRejected { reason, .. } => {
                        report.intents_rejected += 1;
                        bump(&mut report.rejections, reason);
                    }
                    Outbound::StateChanged { to, .. } => report.final_state = to,
                    Outbound::TimerRequested { .. } => {}
                }
            }

            Event::In(inbound) => {
                report.inputs += 1;
                if let Inbound::Market(market) = inbound {
                    lags.push(feed_lag_nanos(market.exchange_time, market.receive_time));
                    // The result is deliberately ignored: a refusal here is
                    // the same refusal the session made, and `Books` counts it.
                    let _ = books.apply(market);
                    // A mark that moved changes what the open position is
                    // worth, so the equity curve has a new point even though
                    // nothing traded.
                    sample(
                        &mut report.max_drawdown,
                        &mut peak,
                        &books,
                        &report.positions,
                        rule,
                    );
                }
                let Inbound::Venue(venue) = inbound else {
                    continue;
                };
                let VenueKind::Filled { px, qty, fee } = venue.kind else {
                    continue;
                };

                let index = base
                    .map(|start| venue.order.raw().saturating_sub(start) as usize)
                    .unwrap_or(usize::MAX);
                let Some(Some((instrument, side))) = submitted.get(index).copied() else {
                    return Err(ReportError::OrphanFill(venue.order));
                };

                report
                    .positions
                    .apply_fill(instrument, side, qty, px, fee)?;
                report.fills += 1;
                report.filled_qty = Qty::from_scaled(
                    report
                        .filled_qty
                        .to_scaled()
                        .saturating_add(qty.to_scaled()),
                );

                sample(
                    &mut report.max_drawdown,
                    &mut peak,
                    &books,
                    &report.positions,
                    rule,
                );
            }
        }
    }

    report.realized = total_realized(&report.positions);
    report.fees = report
        .positions
        .iter()
        .fold(Notional::ZERO, |acc, p| acc + p.fees());

    // Value what is still open. An instrument with no usable mark is named
    // rather than counted as zero, and only if it is actually holding
    // something — a flat instrument has nothing to value and listing it would
    // train people to ignore the list.
    for index in 0..instrument_count {
        let instrument = InstrumentId::new(index as u32);
        let Some(position) = report.positions.get(instrument) else {
            continue;
        };
        let mark = books.mark(instrument, rule).map(|px| Mark {
            px,
            // The mark and its timestamp come from the same event, so they
            // cannot describe different moments.
            at: mark_time(&books, instrument, rule),
        });
        report.valuation.marks[index] = mark;

        match mark {
            Some(mark) => {
                let unrealized = position.unrealized(mark.px)?;
                report.valuation.unrealized_each[index] = Some(unrealized);
                report.valuation.unrealized = report.valuation.unrealized + unrealized;
            }
            None if !position.is_flat() => report.valuation.unmarked.push(instrument),
            None => {}
        }
    }
    report.valuation.total = report.realized + report.valuation.unrealized;
    report.feed_lag = summarize_lag(&mut lags);
    Ok(report)
}

/// Percentiles of the feed lag, or `None` if nothing was measured.
///
/// Nearest-rank, on the sorted set: with a handful of events an interpolated
/// percentile invents a number between two real ones, and every figure here
/// should be a lag some event actually had.
fn summarize_lag(lags: &mut [i64]) -> Option<FeedLag> {
    if lags.is_empty() {
        return None;
    }
    lags.sort_unstable();
    let at = |q: f64| -> i64 {
        let rank = ((lags.len() as f64) * q).ceil() as usize;
        lags[rank.clamp(1, lags.len()) - 1]
    };
    Some(FeedLag {
        events: lags.len(),
        min: lags[0],
        p50: at(0.50),
        p90: at(0.90),
        p99: at(0.99),
        max: lags[lags.len() - 1],
    })
}

/// When the event behind a mark happened.
///
/// Split out so the price and its timestamp are read from the same source
/// under the same rule; deriving one from the top of book and the other from
/// the last trade would produce a mark that describes two different moments.
fn mark_time(books: &Books, instrument: InstrumentId, rule: MarkRule) -> ExchangeTime {
    match rule {
        MarkRule::Mid(_) => books
            .top(instrument)
            .map(|t| t.exchange_time)
            .unwrap_or(ExchangeTime::MIN),
        MarkRule::LastTrade => books
            .last_trade(instrument)
            .map(|t| t.exchange_time)
            .unwrap_or(ExchangeTime::MIN),
    }
}

/// Adds one point to the equity curve and widens the drawdown if it fell.
///
/// Marked to market, so a position that moves against the session counts even
/// though nothing traded. An instrument with no mark yet contributes its
/// realized profit only — the alternative is to skip the whole sample, which
/// would hide the drawdown on every *other* instrument.
fn sample(
    deepest: &mut Notional,
    peak: &mut Notional,
    books: &Books,
    positions: &Positions,
    rule: MarkRule,
) {
    let mut equity = Notional::ZERO;
    for position in positions.iter() {
        equity = equity + position.realized();
        if let Some(mark) = books.mark(position.instrument(), rule)
            && let Ok(unrealized) = position.unrealized(mark)
        {
            equity = equity + unrealized;
        }
    }
    if equity.to_scaled() > peak.to_scaled() {
        *peak = equity;
    }
    let fall = Notional::from_scaled(peak.to_scaled().saturating_sub(equity.to_scaled()));
    if fall.to_scaled() > deepest.to_scaled() {
        *deepest = fall;
    }
}

fn total_realized(positions: &Positions) -> Notional {
    positions
        .iter()
        .fold(Notional::ZERO, |acc, p| acc + p.realized())
}

/// Counts one refusal, keeping the list in a stable order.
fn bump(counts: &mut Vec<(RiskReason, usize)>, reason: RiskReason) {
    if let Some(entry) = counts.iter_mut().find(|(r, _)| *r == reason) {
        entry.1 += 1;
    } else {
        counts.push((reason, 1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use event::{FORMAT_VERSION, OrderKind, Seq, VenueEvent};
    use types::{ExchangeTime, Px, StrategyId, Timestamp};

    const SCALE: i64 = types::SCALE;
    /// These tests are about counting and accounting, not valuation; the rule
    /// has to be *a* rule and this is the one the engine uses.
    const MID: MarkRule = MarkRule::Mid(types::RoundDir::Down);
    const I: InstrumentId = InstrumentId::new(0);

    fn px(whole: i64) -> Px {
        Px::from_scaled(whole * SCALE)
    }

    fn qty(whole: i64) -> Qty {
        Qty::from_scaled(whole * SCALE)
    }

    fn money(whole: i64) -> Notional {
        Notional::from_scaled(whole as i128 * SCALE as i128)
    }

    fn at(n: i64) -> ExchangeTime {
        Timestamp::from_nanos(n)
    }

    fn record(seq: u64, event: Event) -> Record {
        Record {
            seq: Seq::new(seq),
            version: FORMAT_VERSION,
            event,
        }
    }

    fn submitted(seq: u64, order: u64, side: Side) -> Record {
        record(
            seq,
            Event::Out(Outbound::OrderSubmitted {
                caused_by: Seq::new(0),
                order: OrderId::new(order),
                strategy: StrategyId::new(0),
                instrument: I,
                side,
                qty: qty(10),
                kind: OrderKind::Market,
                reduce_only: false,
            }),
        )
    }

    fn filled(seq: u64, order: u64, price: i64, size: i64) -> Record {
        record(
            seq,
            Event::In(Inbound::Venue(VenueEvent {
                order: OrderId::new(order),
                venue_time: at(seq as i64),
                receive_time: Timestamp::from_nanos(seq as i64),
                kind: VenueKind::Filled {
                    px: px(price),
                    qty: qty(size),
                    fee: Notional::ZERO,
                },
            })),
        )
    }

    #[test]
    fn an_empty_log_summarizes_to_nothing() {
        let r = summarize(&[], 1, MID).expect("summary");
        assert_eq!(r.records, 0);
        assert_eq!(r.orders_submitted, 0);
        assert_eq!(r.realized, Notional::ZERO);
        assert_eq!(r.final_state, EngineState::Running);
    }

    #[test]
    fn a_round_trip_reports_its_profit() {
        let log = [
            submitted(0, 0, Side::Buy),
            filled(1, 0, 100, 10),
            submitted(2, 1, Side::Sell),
            filled(3, 1, 110, 10),
        ];
        let r = summarize(&log, 1, MID).expect("summary");
        assert_eq!(r.orders_submitted, 2);
        assert_eq!(r.fills, 2);
        assert_eq!(r.filled_qty, qty(20));
        assert_eq!(r.realized, money(100));
        assert!(r.positions.get(I).expect("position").is_flat());
    }

    #[test]
    fn drawdown_measures_the_deepest_fall_from_a_peak() {
        let log = [
            // +100
            submitted(0, 0, Side::Buy),
            filled(1, 0, 100, 10),
            submitted(2, 1, Side::Sell),
            filled(3, 1, 110, 10),
            // -300, so realized goes 100 -> -200
            submitted(4, 2, Side::Buy),
            filled(5, 2, 100, 10),
            submitted(6, 3, Side::Sell),
            filled(7, 3, 70, 10),
            // +50, so realized recovers to -150 and the trough stands
            submitted(8, 4, Side::Buy),
            filled(9, 4, 100, 10),
            submitted(10, 5, Side::Sell),
            filled(11, 5, 105, 10),
        ];
        let r = summarize(&log, 1, MID).expect("summary");
        assert_eq!(r.realized, money(-150));
        assert_eq!(r.max_drawdown, money(300));
    }

    #[test]
    fn refusals_are_counted_by_reason() {
        let reject = |seq: u64, reason| {
            record(
                seq,
                Event::Out(Outbound::IntentRejected {
                    caused_by: Seq::new(0),
                    strategy: StrategyId::new(0),
                    instrument: I,
                    side: Side::Buy,
                    qty: qty(1),
                    reason,
                }),
            )
        };
        let log = [
            reject(0, RiskReason::PositionLimit),
            reject(1, RiskReason::StaleMarketData),
            reject(2, RiskReason::PositionLimit),
        ];
        let r = summarize(&log, 1, MID).expect("summary");
        assert_eq!(r.intents_rejected, 3);
        assert_eq!(
            r.rejections,
            vec![
                (RiskReason::PositionLimit, 2),
                (RiskReason::StaleMarketData, 1)
            ]
        );
    }

    #[test]
    fn a_fill_for_an_order_the_log_never_recorded_is_refused_not_guessed() {
        let log = [submitted(0, 0, Side::Buy), filled(1, 99, 100, 1)];
        assert_eq!(
            summarize(&log, 1, MID).err(),
            Some(ReportError::OrphanFill(OrderId::new(99)))
        );
    }

    #[test]
    fn the_final_state_is_the_last_one_the_log_recorded() {
        let log = [
            record(
                0,
                Event::Out(Outbound::StateChanged {
                    caused_by: Seq::new(0),
                    from: EngineState::Running,
                    to: EngineState::Halted,
                    reason: event::StateReason::OperatorCommand,
                }),
            ),
            record(
                1,
                Event::Out(Outbound::StateChanged {
                    caused_by: Seq::new(0),
                    from: EngineState::Halted,
                    to: EngineState::Killed,
                    reason: event::StateReason::OperatorCommand,
                }),
            ),
        ];
        assert_eq!(
            summarize(&log, 1, MID).expect("summary").final_state,
            EngineState::Killed
        );
    }
}
