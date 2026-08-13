//! A recorded session, replayed as a feed.
//!
//! One of the two edges where backtest, paper, and live differ
//! (`docs/ARCHITECTURE.md` seam 2). Binding this instead of a live feed is what
//! makes a run a backtest, and nothing downstream can tell.
//!
//! # Backtest and replay are different things
//!
//! Both read the same file, and confusing them produces results that look fine
//! and are not. [`Replaying`] makes the caller say which one they mean.
//!
//! **Backtest** ([`Replaying::MarketDataOnly`]) feeds back the market data and
//! nothing else. The strategy trades afresh through whatever venue is bound,
//! and the recorded session's fills, commands, and timers stay in the file
//! where they belong — they were the *previous* session's, and its operator was
//! responding to a state this run will never be in.
//!
//! **Replay** ([`Replaying::EveryInput`]) feeds back every input, including the
//! recorded venue reports. It reproduces the original session exactly, and only
//! makes sense with a venue that says nothing back. Bind a simulated venue to
//! it and every fill arrives twice — once from the file, once from the venue.
//!
//! # Timers
//!
//! In backtest mode this feed schedules the timers a strategy asks for and
//! interleaves them by exchange time, exactly as a scripted feed does. A
//! strategy whose timers silently never fired in backtest would behave
//! differently in live, which is the class of difference Constitution III
//! exists to prevent.
//!
//! In replay mode it **ignores** timer requests, because the original firings
//! are already records in the file. Honouring them as well would fire each
//! timer twice.
//!
//! # What this does not do yet
//!
//! It reads the whole session into memory when it opens. A recorded trading day
//! of ten million records is roughly eight hundred megabytes, so a long session
//! wants streaming — the format supports it, since seeking to a record is
//! arithmetic, but this does not do it yet.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use engine::FeedAdapter;
use event::codec::LogHeader;
use event::{Inbound, LogFileError, LogReader, Outbound, Record, Recovery};
use std::path::Path;
use strategy::TimerRequest;
use types::{ExchangeTime, Instrument, Timestamp};

/// Which of the two readings of a recorded session this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Replaying {
    /// Market data only — a **backtest**.
    ///
    /// The strategy trades afresh against the recorded market through whatever
    /// venue is bound. Recorded venue reports, operator commands, and timer
    /// firings are not replayed: they belonged to a session this one is not.
    MarketDataOnly,
    /// Every recorded input — a **replay**.
    ///
    /// Reproduces the original session. Bind a venue that says nothing back, or
    /// every fill will arrive twice.
    EveryInput,
}

impl Replaying {
    /// Whether this reading includes an input.
    fn includes(self, inbound: &Inbound) -> bool {
        match self {
            Replaying::EveryInput => true,
            Replaying::MarketDataOnly => matches!(inbound, Inbound::Market(_)),
        }
    }
}

/// A recorded session, presented as a feed.
#[derive(Debug)]
pub struct HistoricalFeed {
    header: LogHeader,
    /// Inputs this reading includes, in recorded order.
    inputs: Vec<Inbound>,
    /// What the recorded session decided. Not replayed — kept so a caller can
    /// compare a new run against the old one.
    decisions: Vec<Outbound>,
    next: usize,
    mode: Replaying,
    recovery: Recovery,
    /// Kept sorted by `(at, strategy, token)`: a total order, so two runs fire
    /// simultaneous timers in the same sequence.
    timers: Vec<TimerRequest>,
}

impl HistoricalFeed {
    /// Opens a recorded session, refusing a damaged file.
    ///
    /// `instruments` is the configuration this session will run under, and it
    /// **must** match what the file was recorded with. Without that check,
    /// `InstrumentId(3)` in the file silently means whatever instrument 3
    /// happens to be now, and the backtest reads one contract's prices as
    /// another's.
    pub fn open(
        path: impl AsRef<Path>,
        instruments: &[Instrument],
        mode: Replaying,
    ) -> Result<HistoricalFeed, LogFileError> {
        let mut reader = LogReader::open(path)?;
        reader.header().check_against(instruments)?;
        let records = reader.read_all_intact()?;
        let recovery = Recovery {
            records: records.len() as u64,
            discarded_bytes: 0,
            stopped: None,
        };
        Ok(Self::from_parts(
            reader.header().clone(),
            records,
            mode,
            recovery,
        ))
    }

    /// Opens a recorded session, accepting a damaged one.
    ///
    /// Returns what could be salvaged. The caller gets the [`Recovery`] and is
    /// expected to say so in whatever it reports — a backtest over a truncated
    /// session is a legitimate thing to run and an illegitimate thing to
    /// present as a whole one.
    pub fn open_salvaged(
        path: impl AsRef<Path>,
        instruments: &[Instrument],
        mode: Replaying,
    ) -> Result<HistoricalFeed, LogFileError> {
        let mut reader = LogReader::open(path)?;
        reader.header().check_against(instruments)?;
        let (records, recovery) = reader.read_all()?;
        Ok(Self::from_parts(
            reader.header().clone(),
            records,
            mode,
            recovery,
        ))
    }

    fn from_parts(
        header: LogHeader,
        records: Vec<Record>,
        mode: Replaying,
        recovery: Recovery,
    ) -> HistoricalFeed {
        let mut inputs = Vec::new();
        let mut decisions = Vec::new();
        for record in records {
            match record.event {
                event::Event::In(inbound) if mode.includes(&inbound) => inputs.push(inbound),
                event::Event::In(_) => {}
                event::Event::Out(outbound) => decisions.push(outbound),
            }
        }
        HistoricalFeed {
            header,
            inputs,
            decisions,
            next: 0,
            mode,
            recovery,
            timers: Vec::new(),
        }
    }

    /// What the session was recorded under.
    #[inline]
    pub fn header(&self) -> &LogHeader {
        &self.header
    }

    /// Which reading this feed is.
    #[inline]
    pub const fn mode(&self) -> Replaying {
        self.mode
    }

    /// What the file gave up, if anything.
    #[inline]
    pub const fn recovery(&self) -> Recovery {
        self.recovery
    }

    /// How many inputs this reading will deliver in total.
    #[inline]
    pub fn len(&self) -> usize {
        self.inputs.len()
    }

    /// Whether the recorded session held nothing this reading wants.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty()
    }

    /// How many inputs are left.
    #[inline]
    pub fn remaining(&self) -> usize {
        self.inputs.len().saturating_sub(self.next)
    }

    /// What the recorded session decided.
    ///
    /// Never replayed — a new run makes its own decisions. Kept so a caller can
    /// ask how a changed strategy differs from the recorded one, which is most
    /// of what a backtest is for.
    #[inline]
    pub fn recorded_decisions(&self) -> &[Outbound] {
        &self.decisions
    }

    /// The earliest scheduled timer, if it is due at or before `limit`.
    fn take_due_by(&mut self, limit: ExchangeTime) -> Option<TimerRequest> {
        // A timer due at exactly the next input's timestamp fires first. Either
        // order is defensible; what matters is that it is the same every run.
        if self.timers.first()?.at.to_nanos() > limit.to_nanos() {
            return None;
        }
        Some(self.timers.remove(0))
    }
}

impl FeedAdapter for HistoricalFeed {
    fn next_event(&mut self) -> Option<Inbound> {
        let due = match self.inputs.get(self.next) {
            // The session is spent, so nothing can come before a timer.
            None => {
                if self.timers.is_empty() {
                    None
                } else {
                    Some(self.timers.remove(0))
                }
            }
            Some(next) => match next.exchange_time() {
                Some(limit) => self.take_due_by(limit),
                // An operator command has no exchange time, so no timer can be
                // shown to be due before it.
                None => None,
            },
        };

        if let Some(due) = due {
            return Some(Inbound::Timer(event::TimerEvent {
                strategy: due.strategy,
                token: due.token,
                fires_at: due.at,
                // There is no network between a replayed timer and the process,
                // so the two clocks coincide. They are still different kinds and
                // are never subtracted from each other.
                receive_time: Timestamp::from_nanos(due.at.to_nanos()),
            }));
        }

        let event = self.inputs.get(self.next).copied()?;
        self.next += 1;
        Some(event)
    }

    fn schedule_timer(&mut self, request: TimerRequest) {
        // In replay the original firings are already records in the file.
        // Scheduling them again would fire each timer twice.
        if self.mode == Replaying::EveryInput {
            return;
        }
        self.timers.push(request);
        self.timers
            .sort_by_key(|t| (t.at.to_nanos(), t.strategy.raw(), t.token.raw()));
    }
}
