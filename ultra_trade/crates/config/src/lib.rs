//! What a session trades, under what limits, with which strategies.
//!
//! Before this, the binaries hardcoded one instrument and a set of constants,
//! so trading a second thing or changing a limit meant editing Rust. This is
//! the file that replaces them.
//!
//! # The format
//!
//! One directive per line. Blank lines and `#` comments are ignored. After the
//! directive and its subject come named `key value` pairs, so order does not
//! matter and a missing one is caught by name rather than by position.
//!
//! ```text
//! instrument BTCUSD  tick 0.1   lot 0.00000001  min 0.0001
//! instrument ETHUSD  tick 0.01  lot 0.001       min 0.01
//!
//! limits BTCUSD  max-position 5  max-exposure 500000  max-order-notional 100000
//!                max-orders 60   rate-window 60s      max-quote-age 30s
//!
//! strategy crossover BTCUSD  window 20  size 0.01
//! strategy quote     ETHUSD  half-spread 0.5  size 1  max-inventory 10
//! ```
//!
//! Hand-parsed rather than TOML or JSON, for the same reason the feed speaks
//! lines: neither would earn its dependency in a real-money binary, and the
//! errors here can be better than a generic parser's — every one names the line
//! and what was expected.
//!
//! # Two rules that fail closed
//!
//! **Every instrument needs limits.** An instrument with none cannot be traded,
//! and that is a startup failure rather than something discovered one refused
//! order at a time (Constitution V).
//!
//! **Instrument ids come from declaration order**, and that is what ties a
//! config to a recording. Reorder the `instrument` lines and the ids shift; a
//! recording made under the old order will be refused by the header check
//! rather than silently reinterpreted.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use marketdata::{Aggregator, BarSpec, BarSubscription};
use risk::{LimitBook, Limits};
use std::fmt;
use std::path::Path;
use strategy::{MovingAverageCrossover, Quoter, Strategy};
use types::{ExchangeSpan, Instrument, InstrumentId, Notional, Px, Qty, SCALE, StrategyId};

/// Why a config could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    /// Which line, counting from one. Zero for a whole-file problem.
    pub line: u64,
    /// What was wrong, in words someone can act on.
    pub message: String,
}

impl ConfigError {
    fn at(line: u64, message: impl Into<String>) -> ConfigError {
        ConfigError {
            line,
            message: message.into(),
        }
    }

    fn whole(message: impl Into<String>) -> ConfigError {
        ConfigError {
            line: 0,
            message: message.into(),
        }
    }
}

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.line == 0 {
            f.write_str(&self.message)
        } else {
            write!(f, "line {}: {}", self.line, self.message)
        }
    }
}

impl std::error::Error for ConfigError {}

/// One tradable contract, as configured.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstrumentConfig {
    /// What the feed calls it.
    pub symbol: String,
    /// Its id, from declaration order.
    pub id: InstrumentId,
    /// The venue's conventions.
    pub instrument: Instrument,
}

/// A strategy to run, and what it trades.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrategyConfig {
    /// Which instrument.
    pub instrument: InstrumentId,
    /// The symbol, for messages.
    pub symbol: String,
    /// Which strategy, and its settings.
    pub kind: StrategyKind,
}

/// A strategy this build knows, with the settings only it takes.
///
/// An enum rather than one struct with every field, because the settings do
/// not overlap: a quoter has no window and a crossover has no half-spread.
/// Sharing one shape would mean fields that are meaningless for the strategy
/// in hand, and a config could then set one and be silently ignored.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StrategyKind {
    /// Takes liquidity when a fast average crosses a slow one.
    Crossover {
        /// Bars to average over.
        window: usize,
        /// The position magnitude it holds while a signal is on.
        size: Qty,
        /// Trades per bar.
        bars: BarSpec,
    },
    /// Rests a bid and an ask around the midpoint.
    Quote {
        /// How far either side of the midpoint to rest.
        half_spread: Px,
        /// Size on each side.
        size: Qty,
        /// How far the wanted price must move before a resting order is pulled.
        reprice: Px,
        /// The position beyond which it stops adding on that side.
        max_inventory: Qty,
    },
}

impl StrategyConfig {
    /// Builds the strategy this describes.
    ///
    /// `register` is how the caller turns an [`Aggregator`] into a
    /// [`BarSubscription`] — in practice `|a| engine.add_aggregator(a)`. It is
    /// a closure rather than a subscription passed in because only some
    /// strategies need bars: a quoter works off quotes and never calls it.
    /// Handing one in would mean the caller deciding whether this strategy
    /// wants bars, which is a decision it can get wrong and this type cannot.
    pub fn build(
        &self,
        id: StrategyId,
        mut register: impl FnMut(Aggregator) -> BarSubscription,
    ) -> Box<dyn Strategy> {
        match self.kind {
            StrategyKind::Crossover { window, size, bars } => {
                let aggregator =
                    Aggregator::new(self.instrument, bars).expect("the parser validated this");
                let subscription = register(aggregator);
                Box::new(MovingAverageCrossover::new(
                    id,
                    self.instrument,
                    subscription,
                    window,
                    size,
                ))
            }
            StrategyKind::Quote {
                half_spread,
                size,
                reprice,
                max_inventory,
            } => Box::new(Quoter::new(
                id,
                self.instrument,
                half_spread,
                size,
                reprice,
                max_inventory,
            )),
        }
    }

    /// The same strategy with one setting replaced.
    ///
    /// For a sweep, which varies a parameter across runs. The setting is named
    /// exactly as the config file names it, and a name this kind does not take
    /// is **refused** — silently ignoring it would report a grid of runs that
    /// were all secretly identical, which is worse than no grid at all.
    pub fn with(&self, key: &str, value: &str) -> Result<StrategyConfig, ConfigError> {
        let scaled = || {
            types::parse_scaled(value)
                .map_err(|e| ConfigError::whole(format!("{key} {value:?}: {e}")))
        };
        let count = || {
            value.parse::<u32>().map_err(|_| {
                ConfigError::whole(format!("{key} wants a whole number, got {value:?}"))
            })
        };

        let kind = match (self.kind, key) {
            (StrategyKind::Crossover { size, bars, .. }, "window") => StrategyKind::Crossover {
                window: count()? as usize,
                size,
                bars,
            },
            (StrategyKind::Crossover { window, bars, .. }, "size") => StrategyKind::Crossover {
                window,
                size: Qty::from_scaled(scaled()?),
                bars,
            },
            (StrategyKind::Crossover { window, size, .. }, "bars") => StrategyKind::Crossover {
                window,
                size,
                bars: BarSpec::Tick {
                    threshold: count()?,
                },
            },
            (
                StrategyKind::Quote {
                    size,
                    reprice,
                    max_inventory,
                    ..
                },
                "half-spread",
            ) => StrategyKind::Quote {
                half_spread: Px::from_scaled(scaled()?),
                size,
                reprice,
                max_inventory,
            },
            (
                StrategyKind::Quote {
                    half_spread,
                    reprice,
                    max_inventory,
                    ..
                },
                "size",
            ) => StrategyKind::Quote {
                half_spread,
                size: Qty::from_scaled(scaled()?),
                reprice,
                max_inventory,
            },
            (
                StrategyKind::Quote {
                    half_spread,
                    size,
                    max_inventory,
                    ..
                },
                "reprice",
            ) => StrategyKind::Quote {
                half_spread,
                size,
                reprice: Px::from_scaled(scaled()?),
                max_inventory,
            },
            (
                StrategyKind::Quote {
                    half_spread,
                    size,
                    reprice,
                    ..
                },
                "max-inventory",
            ) => StrategyKind::Quote {
                half_spread,
                size,
                reprice,
                max_inventory: Qty::from_scaled(scaled()?),
            },
            (kind, other) => {
                return Err(ConfigError::whole(format!(
                    "{other:?} is not a setting {} takes",
                    match kind {
                        StrategyKind::Crossover { .. } => "crossover",
                        StrategyKind::Quote { .. } => "quote",
                    }
                )));
            }
        };
        Ok(StrategyConfig {
            instrument: self.instrument,
            symbol: self.symbol.clone(),
            kind,
        })
    }

    /// A one-line description, for a session banner.
    pub fn summary(&self) -> String {
        match self.kind {
            StrategyKind::Crossover { window, size, .. } => {
                format!("crossover {} window {window} size {size}", self.symbol)
            }
            StrategyKind::Quote {
                half_spread, size, ..
            } => format!(
                "quote {} half-spread {half_spread} size {size}",
                self.symbol
            ),
        }
    }
}

/// Everything a session needs to start.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Every instrument, in id order.
    pub instruments: Vec<InstrumentConfig>,
    /// Bounds per instrument.
    pub limits: LimitBook,
    /// Strategies to run, one per declaration.
    pub strategies: Vec<StrategyConfig>,
}

impl SessionConfig {
    /// Reads a config from a file.
    pub fn load(path: impl AsRef<Path>) -> Result<SessionConfig, ConfigError> {
        let text = std::fs::read_to_string(path.as_ref())
            .map_err(|e| ConfigError::whole(format!("cannot read the config: {e}")))?;
        SessionConfig::parse(&text)
    }

    /// The instruments, in the order the engine wants them.
    pub fn instrument_list(&self) -> Vec<Instrument> {
        self.instruments.iter().map(|i| i.instrument).collect()
    }

    /// Looks an instrument up by the symbol a feed uses.
    pub fn by_symbol(&self, symbol: &str) -> Option<&InstrumentConfig> {
        self.instruments.iter().find(|i| i.symbol == symbol)
    }

    /// Reads a config from text.
    pub fn parse(text: &str) -> Result<SessionConfig, ConfigError> {
        let mut instruments: Vec<InstrumentConfig> = Vec::new();
        let mut pending_limits: Vec<(u64, String, Limits)> = Vec::new();
        let mut strategies: Vec<(u64, String, StrategyKind)> = Vec::new();

        for (index, raw) in text.lines().enumerate() {
            let line = index as u64 + 1;
            let content = raw.split('#').next().unwrap_or("").trim();
            if content.is_empty() {
                continue;
            }
            let mut fields = content.split_whitespace();
            let directive = fields.next().expect("non-empty");

            match directive {
                "instrument" => {
                    let symbol = named(&mut fields, line, "a symbol")?;
                    if instruments.iter().any(|i| i.symbol == symbol) {
                        return Err(ConfigError::at(line, format!("{symbol} is declared twice")));
                    }
                    let pairs = Pairs::read(fields, line)?;
                    pairs.require(&["tick", "lot", "min"], &[], line)?;
                    let tick = pairs.px("tick", line)?;
                    let lot = pairs.qty("lot", line)?;
                    let min = pairs.qty("min", line)?;
                    let id = InstrumentId::new(instruments.len() as u32);
                    let instrument = Instrument::new(id, tick, lot, min).map_err(|e| {
                        ConfigError::at(line, format!("{symbol} has unusable conventions: {e}"))
                    })?;
                    instruments.push(InstrumentConfig {
                        symbol,
                        id,
                        instrument,
                    });
                }

                "limits" => {
                    let symbol = named(&mut fields, line, "a symbol")?;
                    let pairs = Pairs::read(fields, line)?;
                    pairs.require(
                        &[
                            "max-position",
                            "max-exposure",
                            "max-order-notional",
                            "max-orders",
                            "rate-window",
                            "max-quote-age",
                        ],
                        &[],
                        line,
                    )?;
                    let limits = Limits {
                        max_position: pairs.qty("max-position", line)?,
                        max_exposure: pairs.money("max-exposure", line)?,
                        max_order_notional: pairs.money("max-order-notional", line)?,
                        max_orders_in_window: pairs.count("max-orders", line)?,
                        rate_window: pairs.span("rate-window", line)?,
                        max_quote_age: pairs.span("max-quote-age", line)?,
                    };
                    limits.validate().map_err(|e| {
                        ConfigError::at(line, format!("{symbol} has unusable limits: {e:?}"))
                    })?;
                    pending_limits.push((line, symbol, limits));
                }

                "strategy" => {
                    let name = named(&mut fields, line, "a strategy name")?;
                    let symbol = named(&mut fields, line, "a symbol")?;
                    let pairs = Pairs::read(fields, line)?;
                    let kind = match name.as_str() {
                        "crossover" => {
                            pairs.require(&["window", "size"], &["bars"], line)?;
                            let window = pairs.count("window", line)? as usize;
                            if window == 0 {
                                return Err(ConfigError::at(
                                    line,
                                    "window must be at least one bar",
                                ));
                            }
                            let size = pairs.qty("size", line)?;
                            if size.to_scaled() <= 0 {
                                return Err(ConfigError::at(line, "size must be positive"));
                            }
                            let bars = match pairs.get("bars") {
                                None => BarSpec::Tick { threshold: 1 },
                                Some(_) => BarSpec::Tick {
                                    threshold: pairs.count("bars", line)?,
                                },
                            };
                            StrategyKind::Crossover { window, size, bars }
                        }
                        "quote" => {
                            pairs.require(
                                &["half-spread", "size", "max-inventory"],
                                &["reprice"],
                                line,
                            )?;
                            let half_spread = pairs.px("half-spread", line)?;
                            if half_spread.to_scaled() <= 0 {
                                return Err(ConfigError::at(
                                    line,
                                    "half-spread must be positive, or the quotes cross",
                                ));
                            }
                            let size = pairs.qty("size", line)?;
                            if size.to_scaled() <= 0 {
                                return Err(ConfigError::at(line, "size must be positive"));
                            }
                            // Defaults to the half-spread: a quote is worth
                            // moving once the market has drifted as far as the
                            // edge it was trying to earn.
                            let reprice = match pairs.get("reprice") {
                                None => half_spread,
                                Some(_) => pairs.px("reprice", line)?,
                            };
                            if reprice.to_scaled() <= 0 {
                                return Err(ConfigError::at(
                                    line,
                                    "reprice must be positive, or every tick costs a round trip",
                                ));
                            }
                            let max_inventory = pairs.qty("max-inventory", line)?;
                            if max_inventory.to_scaled() < 0 {
                                return Err(ConfigError::at(
                                    line,
                                    "max-inventory cannot be negative",
                                ));
                            }
                            StrategyKind::Quote {
                                half_spread,
                                size,
                                reprice,
                                max_inventory,
                            }
                        }
                        other => {
                            return Err(ConfigError::at(
                                line,
                                format!(
                                    "{other:?} is not a strategy this build knows; \
                                     try crossover or quote"
                                ),
                            ));
                        }
                    };
                    strategies.push((line, symbol, kind));
                }

                other => {
                    return Err(ConfigError::at(
                        line,
                        format!(
                            "{other:?} is not a directive; expected instrument, limits, or strategy"
                        ),
                    ));
                }
            }
        }

        if instruments.is_empty() {
            return Err(ConfigError::whole("the config declares no instruments"));
        }

        let find = |symbol: &str, line: u64| -> Result<InstrumentId, ConfigError> {
            instruments
                .iter()
                .find(|i| i.symbol == symbol)
                .map(|i| i.id)
                .ok_or_else(|| {
                    ConfigError::at(line, format!("{symbol} has no instrument declaration"))
                })
        };

        let mut book = LimitBook::with_instruments(instruments.len());
        let mut configured = vec![false; instruments.len()];
        for (line, symbol, limits) in &pending_limits {
            let id = find(symbol, *line)?;
            if configured[id.index()] {
                return Err(ConfigError::at(
                    *line,
                    format!("{symbol} has limits declared twice"),
                ));
            }
            book.set(id, *limits)
                .map_err(|e| ConfigError::at(*line, format!("{symbol}: {e:?}")))?;
            configured[id.index()] = true;
        }

        // An instrument with no limits cannot be traded. Saying so now beats
        // discovering it one refused order at a time.
        if let Some(missing) = configured.iter().position(|done| !done) {
            return Err(ConfigError::whole(format!(
                "{} has no limits line, so it could never be traded",
                instruments[missing].symbol
            )));
        }

        let mut resolved = Vec::new();
        for (line, symbol, kind) in strategies {
            let instrument = find(&symbol, line)?;
            resolved.push(StrategyConfig {
                instrument,
                symbol,
                kind,
            });
        }

        Ok(SessionConfig {
            instruments,
            limits: book,
            strategies: resolved,
        })
    }
}

fn named<'a>(
    fields: &mut impl Iterator<Item = &'a str>,
    line: u64,
    what: &str,
) -> Result<String, ConfigError> {
    fields
        .next()
        .map(str::to_string)
        .ok_or_else(|| ConfigError::at(line, format!("expected {what}")))
}

/// The `key value` pairs after a directive's subject.
struct Pairs<'a> {
    entries: Vec<(&'a str, &'a str)>,
}

impl<'a> Pairs<'a> {
    fn read(
        mut fields: impl Iterator<Item = &'a str>,
        line: u64,
    ) -> Result<Pairs<'a>, ConfigError> {
        let mut entries = Vec::new();
        while let Some(key) = fields.next() {
            let value = fields
                .next()
                .ok_or_else(|| ConfigError::at(line, format!("{key:?} has no value after it")))?;
            if entries.iter().any(|(k, _)| *k == key) {
                return Err(ConfigError::at(line, format!("{key:?} is given twice")));
            }
            entries.push((key, value));
        }
        Ok(Pairs { entries })
    }

    fn get(&self, key: &str) -> Option<&'a str> {
        self.entries
            .iter()
            .find(|(k, _)| *k == key)
            .map(|(_, v)| *v)
    }

    /// Every expected key is present, and nothing unexpected is.
    ///
    /// Unknown keys are refused rather than ignored: a misspelled limit that is
    /// silently dropped is a limit that is not enforced.
    /// Checks the settings on a line against what the directive accepts.
    ///
    /// `optional` is spelled out rather than exempted in here, because a
    /// hardcoded exemption is a per-directive rule hiding in a shared helper —
    /// it was `bars`, and the second one would have been `reprice`.
    fn require(&self, required: &[&str], optional: &[&str], line: u64) -> Result<(), ConfigError> {
        // Unknown keys first. A misspelled setting is the common mistake, and
        // naming the thing that was actually written beats telling someone
        // what is missing and leaving them to spot the typo themselves.
        for (key, _) in &self.entries {
            if !required.contains(key) && !optional.contains(key) {
                let mut known: Vec<&str> = required.to_vec();
                known.extend_from_slice(optional);
                return Err(ConfigError::at(
                    line,
                    format!("{key:?} is not a setting here; expected one of {known:?}"),
                ));
            }
        }
        for key in required {
            if self.get(key).is_none() {
                return Err(ConfigError::at(line, format!("missing {key:?}")));
            }
        }
        Ok(())
    }

    fn decimal(&self, key: &str, line: u64) -> Result<i64, ConfigError> {
        let text = self
            .get(key)
            .ok_or_else(|| ConfigError::at(line, format!("missing {key:?}")))?;
        types::parse_scaled(text).map_err(|e| ConfigError::at(line, format!("{key} {text:?}: {e}")))
    }

    fn px(&self, key: &str, line: u64) -> Result<Px, ConfigError> {
        Ok(Px::from_scaled(self.decimal(key, line)?))
    }

    fn qty(&self, key: &str, line: u64) -> Result<Qty, ConfigError> {
        Ok(Qty::from_scaled(self.decimal(key, line)?))
    }

    fn money(&self, key: &str, line: u64) -> Result<Notional, ConfigError> {
        Ok(Notional::from_scaled(self.decimal(key, line)? as i128))
    }

    fn count(&self, key: &str, line: u64) -> Result<u32, ConfigError> {
        let text = self
            .get(key)
            .ok_or_else(|| ConfigError::at(line, format!("missing {key:?}")))?;
        text.parse()
            .map_err(|_| ConfigError::at(line, format!("{key} {text:?} is not a whole number")))
    }

    /// A duration, written `30s` or `500ms`.
    fn span(&self, key: &str, line: u64) -> Result<ExchangeSpan, ConfigError> {
        let text = self
            .get(key)
            .ok_or_else(|| ConfigError::at(line, format!("missing {key:?}")))?;
        let (number, per_unit) = if let Some(rest) = text.strip_suffix("ms") {
            (rest, 1_000_000i128)
        } else if let Some(rest) = text.strip_suffix('s') {
            (rest, SCALE as i128)
        } else {
            return Err(ConfigError::at(
                line,
                format!("{key} {text:?} needs a unit: 30s or 500ms"),
            ));
        };
        let scaled = types::parse_scaled(number)
            .map_err(|e| ConfigError::at(line, format!("{key} {text:?}: {e}")))?;
        if scaled <= 0 {
            return Err(ConfigError::at(line, format!("{key} must be positive")));
        }
        // `parse_scaled` gives units of 1e-9, so a value in seconds is already
        // nanoseconds; milliseconds need scaling down by a thousand.
        let nanos = if per_unit == SCALE as i128 {
            scaled as i128
        } else {
            scaled as i128 / 1_000
        };
        Ok(ExchangeSpan::from_nanos(nanos))
    }
}
