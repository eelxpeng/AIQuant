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
use oms::{PositionError, Positions};
use types::{InstrumentId, Notional, OrderId, Qty, Side};

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
    /// The deepest fall from a peak in cumulative realized profit.
    ///
    /// Measured on realized profit alone, because that is what the log
    /// establishes without choosing a mark. A mark-to-market drawdown needs a
    /// mark rule and a valuation timestamp, which is a different report (#1).
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
    };

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

                // Drawdown is sampled at each fill, which is every instant the
                // realized number can change.
                let realized = total_realized(&report.positions);
                if realized.to_scaled() > peak.to_scaled() {
                    peak = realized;
                }
                let fall =
                    Notional::from_scaled(peak.to_scaled().saturating_sub(realized.to_scaled()));
                if fall.to_scaled() > report.max_drawdown.to_scaled() {
                    report.max_drawdown = fall;
                }
            }
        }
    }

    report.realized = total_realized(&report.positions);
    report.fees = report
        .positions
        .iter()
        .fold(Notional::ZERO, |acc, p| acc + p.fees());
    Ok(report)
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
        let r = summarize(&[], 1).expect("summary");
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
        let r = summarize(&log, 1).expect("summary");
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
        let r = summarize(&log, 1).expect("summary");
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
        let r = summarize(&log, 1).expect("summary");
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
            summarize(&log, 1).err(),
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
            summarize(&log, 1).expect("summary").final_state,
            EngineState::Killed
        );
    }
}
