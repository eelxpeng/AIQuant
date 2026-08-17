//! Running one configured session over a recording.
//!
//! `backtest` runs one of these and prints it. `sweep` runs hundreds and
//! compares them. Before this crate the run lived inside `bin/backtest`'s
//! `main`, so the second caller would have had to copy it — and two copies of
//! "what a backtest is" is two answers waiting to disagree about fees, or the
//! mark rule, or which replay mode makes it a backtest at all.
//!
//! Nothing here decides anything. The bindings that make a run a backtest —
//! a recorded feed and a simulated venue — are here because they are what a
//! backtest *is*, not because this crate chose them (Constitution III).

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use config::SessionConfig;
use engine::{Engine, EngineConfig, run};
use event::codec::LogHeader;
use event::{MemoryLog, Record};
use historical::{HistoricalFeed, Replaying};
use marketdata::MarkRule;
use report::{SessionReport, summarize};
use sim_venue::{Fees, FillModel, Queue, SimVenue};
use types::{ExchangeSpan, Instrument, OrderId, Px, SCALE, StrategyId, ValueError};

/// What a run charges and how it fills.
///
/// Separate from the session config because it describes the *venue*, not the
/// policy: two configs compared over one recording must be charged the same
/// way or the comparison measures the fee schedule.
#[derive(Debug, Clone, Copy)]
pub struct Costs {
    /// Charged per unit on a resting fill.
    pub maker: Px,
    /// Charged per unit on a taking fill.
    pub taker: Px,
    /// Where a resting order sits in the queue at its price.
    ///
    /// The most optimistic thing the simulator does is assume it is first, and
    /// that flatters passive strategies specifically. Here beside the fees for
    /// the same reason: two policies compared over one recording must meet the
    /// same venue.
    pub queue: Queue,
    /// How much of an order the book lets through.
    ///
    /// Here rather than in the session config for the same reason as the fees:
    /// two policies compared over one recording must meet the same venue, or
    /// the comparison measures the fill model.
    pub model: FillModel,
}

impl Costs {
    /// The default a backtest charges: nothing to rest, 0.01 per unit to take.
    ///
    /// A number rather than zero, because a strategy evaluated at zero cost is
    /// a strategy nobody can run. It is a *placeholder* until a venue's real
    /// schedule is configured, and it is stated here rather than buried.
    pub const DEFAULT: Costs = Costs {
        maker: Px::ZERO,
        taker: Px::from_scaled(SCALE / 100),
        // The conservative one. `WalkBook` needs depth in the recording and
        // silently equals this without it, so defaulting to it would tell some
        // recordings they were being walked when they were not.
        model: FillModel::TouchDisplayed,
        // Front of the queue, matching what every result so far was measured
        // under. Changing a default silently would move every number in the
        // repository at once and make this change look like a strategy result.
        queue: Queue::Front,
    };
}

/// Why a run could not be made.
#[derive(Debug, Clone)]
pub enum HarnessError {
    /// The recording and the config disagree about what exists.
    Mismatch(String),
    /// The recording's instrument table is not usable.
    Instrument(ValueError),
    /// The engine refused an event.
    Engine(String),
    /// The run could not be summarized.
    Report(String),
}

impl core::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            HarnessError::Mismatch(m) => f.write_str(m),
            HarnessError::Instrument(e) => write!(f, "{e:?}"),
            HarnessError::Engine(m) => write!(f, "the session stopped: {m}"),
            HarnessError::Report(m) => write!(f, "cannot summarize the run: {m}"),
        }
    }
}

impl core::error::Error for HarnessError {}

/// The instruments a recording was made under.
///
/// Taken from the header rather than the config, because a run that configured
/// its own tick and lot sizes could disagree with what was recorded — and then
/// `InstrumentId(0)` would silently mean a different contract.
pub fn instruments_of(header: &LogHeader) -> Result<Vec<Instrument>, HarnessError> {
    header
        .instruments
        .iter()
        .map(|e| Instrument::new(e.id, e.tick, e.lot, e.min_qty).map_err(HarnessError::Instrument))
        .collect()
}

/// Runs `session` over `records` and reports what it did.
///
/// `records` is a recording's records, or a slice of them — a caller measuring
/// out of sample passes the second half.
pub fn backtest(
    header: &LogHeader,
    records: &[Record],
    session: &SessionConfig,
    costs: Costs,
    mark: MarkRule,
) -> Result<SessionReport, HarnessError> {
    let instruments = instruments_of(header)?;
    if instruments.is_empty() {
        return Err(HarnessError::Mismatch(
            "the recording has no instruments".to_string(),
        ));
    }
    if session.instruments.len() != instruments.len() {
        return Err(HarnessError::Mismatch(format!(
            "the recording holds {} instruments, the config declares {}",
            instruments.len(),
            session.instruments.len()
        )));
    }

    // The two bindings that make this a backtest: a recorded feed, and a
    // simulated venue. Market data only — feeding the recorded venue reports
    // back in *and* binding a venue would deliver every fill twice.
    let mut feed =
        HistoricalFeed::from_records(header.clone(), records.to_vec(), Replaying::MarketDataOnly);
    let venue = SimVenue::new(
        instruments.len(),
        costs.model,
        costs.queue,
        Fees {
            maker: costs.maker,
            taker: costs.taker,
        },
        ExchangeSpan::from_nanos(0),
    );

    let engine_config = EngineConfig::new(
        instruments.clone(),
        session.limits.clone(),
        OrderId::new(0),
        header.session_start,
    );
    let mut engine = Engine::new(
        engine_config,
        venue,
        MemoryLog::with_capacity(records.len().max(1) * 4),
    );
    for (index, spec) in session.strategies.iter().enumerate() {
        let strategy = spec.build(StrategyId::new(index as u16), |aggregator| {
            engine.add_aggregator(aggregator)
        });
        engine
            .add_strategy(strategy)
            .map_err(|e| HarnessError::Engine(format!("{e:?}")))?;
    }

    run(&mut feed, &mut engine).map_err(|e| HarnessError::Engine(format!("{e:?}")))?;

    summarize(engine.log().records(), instruments.len(), mark)
        .map_err(|e| HarnessError::Report(format!("{e:?}")))
}

/// Splits a recording's records into an in-sample and an out-of-sample half.
///
/// The split is by **market event**, not by record index: decisions and venue
/// reports cluster around active moments, so splitting on records would hand
/// the busier half of the session to whichever side had more trading in it.
///
/// `fraction` is the share that goes in sample, clamped to leave at least one
/// event on each side. Returns `None` when there is not enough market data to
/// make two halves, which is a refusal rather than a one-sided split nobody
/// asked for.
pub fn split_by_market_events(records: &[Record], fraction: f64) -> Option<(&[Record], &[Record])> {
    let total = records
        .iter()
        .filter(|r| matches!(r.event, event::Event::In(event::Inbound::Market(_))))
        .count();
    if total < 2 {
        return None;
    }

    let want = ((total as f64) * fraction).round() as usize;
    let want = want.clamp(1, total - 1);

    let mut seen = 0usize;
    for (index, record) in records.iter().enumerate() {
        if matches!(record.event, event::Event::In(event::Inbound::Market(_))) {
            seen += 1;
            if seen == want {
                return Some(records.split_at(index + 1));
            }
        }
    }
    None
}
