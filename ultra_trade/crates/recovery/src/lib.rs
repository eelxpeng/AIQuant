//! Bringing a crashed session back.
//!
//! A session that dies leaves a recording and a question: what did it hold
//! when it stopped, and is it safe to carry on? This answers the first and
//! refuses to guess at the second.
//!
//! # How the state comes back
//!
//! Not by installing it. The engine replays its own recording — the same path
//! that produced the state the first time — and arrives where it was
//! (Constitution II). A second way to reach a position is a second answer
//! waiting to disagree with the first.
//!
//! What is replayed is everything the outside world said: market data,
//! operator commands, timer firings. The venue's own reports are **not**,
//! because a simulated venue regenerates them from the same market and the
//! same orders. That is what rebuilds the venue as well as the engine — its
//! book, its resting orders — instead of leaving it empty while the engine
//! believes it is trading.
//!
//! # What makes it a check rather than an assumption
//!
//! Because the venue speaks for itself, the replayed session and the recording
//! are two independent accounts of the same thing: one derived by trading
//! forward through the venue, one by walking the recorded fills. Recovery
//! compares them per instrument. Agreement means the reconstruction is sound.
//! Disagreement means something is wrong that nobody has explained yet, and
//! the answer is to stop rather than to pick a side (contract D-4,
//! Constitution V).
//!
//! # What this does not cover
//!
//! A **real** venue. Its reports cannot be regenerated, so recovery against
//! one asks it what it holds and cancels by its list (contract D-3), and none
//! of that is built — there is no real venue to ask. The comparison here is
//! sound precisely because the venue is deterministic, and that is a property
//! of a simulated venue rather than of venues.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use core::fmt;

use engine::{Engine, EngineError};
use event::{EventLog, Inbound, LogFileError, Record, Recovery, Segments, Seq};
use marketdata::MarkRule;
use oms::{Positions, VenueAdapter};
use report::{ReportError, summarize};
use types::{InstrumentId, Qty, RoundDir};

/// What a recovery concluded.
#[derive(Debug, Clone)]
pub struct Recovered {
    /// Records the chain held.
    pub records: u64,
    /// The sequence the resumed session's first record will carry.
    pub next_seq: Seq,
    /// What reading the recording said the books were.
    pub from_log: Positions,
    /// What the recording lost, if anything.
    ///
    /// A crash that tore the tail off a segment is the ordinary case, and the
    /// session it describes still recovers — but the fact is carried out so a
    /// caller reports it rather than presenting a salvaged session as a whole
    /// one.
    pub recovery: Recovery,
}

/// Why a session could not be brought back.
#[derive(Debug)]
pub enum RecoveryError {
    /// The recording could not be read.
    Log(LogFileError),
    /// The recording does not add up on its own terms.
    Report(ReportError),
    /// The engine refused an event while replaying it.
    ///
    /// The recording contains something this build cannot process — a
    /// different instrument table, or a limit that no longer admits an order
    /// that was placed. Continuing would trade on a state that was never
    /// reached.
    Engine(EngineError),
    /// Replaying the session did not reproduce the books the recording shows.
    ///
    /// The dangerous one. Two independent readings of the same session
    /// disagree, and neither is known to be right, so trading stops until a
    /// person says otherwise (contract D-4).
    Diverged {
        /// Which instrument disagrees.
        instrument: InstrumentId,
        /// What walking the recorded fills says.
        from_log: Qty,
        /// What replaying the session through the venue says.
        from_replay: Qty,
    },
}

impl fmt::Display for RecoveryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RecoveryError::Log(e) => write!(f, "{e}"),
            RecoveryError::Report(e) => write!(f, "this recording does not add up: {e:?}"),
            RecoveryError::Engine(e) => write!(f, "replaying this recording failed: {e:?}"),
            RecoveryError::Diverged {
                instrument,
                from_log,
                from_replay,
            } => write!(
                f,
                "instrument {instrument}: the recording says {from_log} and replaying it says \
                 {from_replay}. Neither is known to be right, so this session will not resume."
            ),
        }
    }
}

impl core::error::Error for RecoveryError {}

impl From<LogFileError> for RecoveryError {
    fn from(e: LogFileError) -> RecoveryError {
        RecoveryError::Log(e)
    }
}

impl From<EngineError> for RecoveryError {
    fn from(e: EngineError) -> RecoveryError {
        RecoveryError::Engine(e)
    }
}

impl From<ReportError> for RecoveryError {
    fn from(e: ReportError) -> RecoveryError {
        RecoveryError::Report(e)
    }
}

/// Rebuilds a session's state into `engine` from the recording at `root`.
///
/// The engine must be fresh — newly built, with its strategies registered and
/// nothing processed. On success it holds the position the session held and is
/// **halted**; only an operator resumes it.
pub fn recover<V, L>(
    engine: &mut Engine<V, L>,
    root: &std::path::Path,
) -> Result<Recovered, RecoveryError>
where
    V: VenueAdapter,
    L: EventLog,
{
    // A session with no segments is not an empty session — it is a session
    // that was never started. Recovering it to "flat, nothing happened" would
    // be a resume quietly becoming a fresh session, which loses exactly the
    // position the operator was trying to get back. `header` refuses it;
    // `read_all` would hand back an empty list that looks like success.
    Segments::header(root)?;

    let (records, recovery) = Segments::read_all(root)?;
    recover_from(engine, &records, recovery)
}

/// The same, over records already in hand.
///
/// Separate so a caller that has read the chain for its own reasons does not
/// read it twice, and so the comparison can be tested without a filesystem.
pub fn recover_from<V, L>(
    engine: &mut Engine<V, L>,
    records: &[Record],
    recovery: Recovery,
) -> Result<Recovered, RecoveryError>
where
    V: VenueAdapter,
    L: EventLog,
{
    // What the recording says, read straight off the fills. This is the
    // account the replay will be checked against, and it is derived through
    // `oms`'s accounting rather than a second implementation of it.
    let instruments = engine.positions().len();
    // The mark rule is immaterial here: recovery compares *positions*, and a
    // valuation would be an unused number that could still fail to compute.
    let from_log = summarize(records, instruments, MarkRule::Mid(RoundDir::Down))?.positions;

    engine.begin_replay();
    let outcome = replay(engine, records);
    // Ends the replay even if it failed part-way. An engine left in replay
    // would silently stop writing its log, which is the worst way to fail:
    // it would look like it was trading and record none of it.
    engine.end_replay();
    outcome?;

    for expected in from_log.iter() {
        let instrument = expected.instrument();
        let replayed = engine
            .positions()
            .get(instrument)
            .map(|p| p.qty())
            .unwrap_or(Qty::ZERO);
        if replayed != expected.qty() {
            return Err(RecoveryError::Diverged {
                instrument,
                from_log: expected.qty(),
                from_replay: replayed,
            });
        }
    }

    let next_seq = records
        .last()
        .map(|r| Seq::new(r.seq.raw() + 1))
        .unwrap_or(Seq::FIRST);

    // Pull whatever the session had resting. Reconstructing the venue brings
    // those orders back live, and left alone they keep filling while the
    // session is halted — the position would move and nobody would have
    // decided that it should. Losing the queue position is the price of
    // coming back to a state somebody chose (contract D-3).
    //
    // Recorded in the new segment as decisions of the resumed session, which
    // is what they are.
    engine.cancel_resting(next_seq)?;
    Ok(Recovered {
        records: records.len() as u64,
        next_seq,
        from_log,
        recovery,
    })
}

/// Feeds the recorded inputs back through the engine.
fn replay<V, L>(engine: &mut Engine<V, L>, records: &[Record]) -> Result<(), RecoveryError>
where
    V: VenueAdapter,
    L: EventLog,
{
    for record in records {
        let Some(inbound) = record.event.as_inbound() else {
            continue;
        };
        // The venue's own words are left out: it will say them again. Feeding
        // both would deliver every fill twice.
        if matches!(inbound, Inbound::Venue(_) | Inbound::VenuePosition(_)) {
            continue;
        }
        engine.on_inbound(*inbound)?;
    }
    Ok(())
}
