use core::cmp::Ordering;
use core::fmt;
use core::hash::{Hash, Hasher};
use core::marker::PhantomData;
use core::ops::{Add, Neg, Sub};

use crate::error::ValueError;

mod sealed {
    pub trait Sealed {}
}

/// One of the three clocks in this system.
///
/// Sealed: no fourth kind can be added downstream, so "three clocks, each named
/// in `CONTEXT.md`" stays true.
pub trait ClockKind: sealed::Sealed {}

/// The venue's clock — the only one valid for ordering market events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Exchange;

/// The local instant at which this process observed an event. Valid for
/// latency measurement, never for market-state logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Receive;

/// The injected local monotonic clock, for elapsed-time and timeout logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Monotonic;

impl sealed::Sealed for Exchange {}
impl sealed::Sealed for Receive {}
impl sealed::Sealed for Monotonic {}

impl ClockKind for Exchange {}
impl ClockKind for Receive {}
impl ClockKind for Monotonic {}

/// An instant on one clock, in nanoseconds.
///
/// Two timestamps of different kinds share no operation: they cannot be added,
/// subtracted, compared, or converted (Constitution VII). Nothing here reads a
/// clock — a timestamp arrives as a value on an event.
///
/// The `i64` nanosecond count spans roughly 1678–2262. An instant outside that
/// has no representation, so there is no construction path to check; the window
/// is only reachable from inside, by [`Timestamp::checked_add`].
pub struct Timestamp<K: ClockKind> {
    nanos: i64,
    kind: PhantomData<fn() -> K>,
}

/// The elapsed nanoseconds between two timestamps of one clock kind, carrying
/// that kind.
///
/// A receive-derived span cannot touch an exchange timestamp (ADR #6).
/// Relating two clocks is reserved for a named skew conversion, which does not
/// exist yet — Constitution VII requires skew to be modelled, not assumed.
///
/// Backed by `i128` so that subtracting two timestamps is total: the widest
/// difference two `i64` nanosecond values can have is ~1.845e19, which an `i64`
/// cannot hold.
pub struct Span<K: ClockKind> {
    nanos: i128,
    kind: PhantomData<fn() -> K>,
}

/// The timestamp a venue assigned to an event.
pub type ExchangeTime = Timestamp<Exchange>;
/// The local instant at which this process observed an event.
pub type ReceiveTime = Timestamp<Receive>;
/// A reading of the injected local monotonic clock.
pub type MonotonicTime = Timestamp<Monotonic>;
/// Elapsed exchange time.
pub type ExchangeSpan = Span<Exchange>;
/// Elapsed receive time.
pub type ReceiveSpan = Span<Receive>;
/// Elapsed monotonic time — what a tick-to-trade measurement is made of.
pub type MonotonicSpan = Span<Monotonic>;

impl<K: ClockKind> Timestamp<K> {
    /// The earliest representable instant on this clock.
    pub const MIN: Self = Self::from_nanos(i64::MIN);
    /// The latest representable instant on this clock.
    pub const MAX: Self = Self::from_nanos(i64::MAX);

    /// Builds a timestamp from a nanosecond count.
    ///
    /// For [`Exchange`] and [`Receive`] the origin is the Unix epoch; for
    /// [`Monotonic`] it is an opaque process-local origin whose absolute value
    /// is meaningless.
    #[inline]
    #[must_use]
    pub const fn from_nanos(nanos: i64) -> Self {
        Self {
            nanos,
            kind: PhantomData,
        }
    }

    /// This timestamp's nanosecond count.
    #[inline]
    #[must_use]
    pub const fn to_nanos(self) -> i64 {
        self.nanos
    }

    /// Moves this timestamp forward by a span of its own clock kind.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the result leaves the representable
    /// window.
    pub fn checked_add(self, span: Span<K>) -> Result<Self, ValueError> {
        // Widened first: a span is `i128` and can be far wider than the window.
        let moved = match (self.nanos as i128).checked_add(span.to_nanos()) {
            Some(moved) => moved,
            None => return Err(ValueError::Overflow),
        };
        match i64::try_from(moved) {
            Ok(nanos) => Ok(Self::from_nanos(nanos)),
            Err(_) => Err(ValueError::Overflow),
        }
    }

    /// Moves this timestamp backward by a span of its own clock kind.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the result leaves the representable
    /// window.
    pub fn checked_sub(self, span: Span<K>) -> Result<Self, ValueError> {
        let moved = match (self.nanos as i128).checked_sub(span.to_nanos()) {
            Some(moved) => moved,
            None => return Err(ValueError::Overflow),
        };
        match i64::try_from(moved) {
            Ok(nanos) => Ok(Self::from_nanos(nanos)),
            Err(_) => Err(ValueError::Overflow),
        }
    }
}

impl<K: ClockKind> Span<K> {
    /// No elapsed time.
    pub const ZERO: Self = Self::from_nanos(0);

    /// Builds a span from a nanosecond count, with the clock kind named at the
    /// call site — `MonotonicSpan::from_nanos(5_000_000_000)`.
    #[inline]
    #[must_use]
    pub const fn from_nanos(nanos: i128) -> Self {
        Self {
            nanos,
            kind: PhantomData,
        }
    }

    /// This span's nanosecond count.
    #[inline]
    #[must_use]
    pub const fn to_nanos(self) -> i128 {
        self.nanos
    }

    /// Adds two spans of the same clock kind at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the sum leaves the range.
    pub const fn checked_add(self, rhs: Span<K>) -> Result<Span<K>, ValueError> {
        match self.nanos.checked_add(rhs.nanos) {
            Some(nanos) => Ok(Self::from_nanos(nanos)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Subtracts two spans of the same clock kind at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::Overflow`] when the difference leaves the range.
    pub const fn checked_sub(self, rhs: Span<K>) -> Result<Span<K>, ValueError> {
        match self.nanos.checked_sub(rhs.nanos) {
            Some(nanos) => Ok(Self::from_nanos(nanos)),
            None => Err(ValueError::Overflow),
        }
    }

    /// Negates a span at a boundary.
    ///
    /// # Errors
    ///
    /// [`ValueError::OutOfRange`] on the most negative span, which has no
    /// positive counterpart.
    pub const fn checked_neg(self) -> Result<Span<K>, ValueError> {
        match self.nanos.checked_neg() {
            Some(nanos) => Ok(Self::from_nanos(nanos)),
            None => Err(ValueError::OutOfRange),
        }
    }
}

/// Total — this is the one subtraction that cannot overflow, and INV-013 is
/// why: the widest difference two `i64` nanosecond counts can have is
/// `2^64 - 1 ≈ 1.845e19`, which an `i64` cannot hold and an `i128` holds with
/// room to spare. That is what buys the missing `Result` here, and it is why
/// [`Span`] is `i128` rather than `i64`.
impl<K: ClockKind> Sub for Timestamp<K> {
    type Output = Span<K>;

    #[inline]
    fn sub(self, rhs: Self) -> Span<K> {
        Span::from_nanos((self.nanos as i128) - (rhs.nanos as i128))
    }
}

// Interior span arithmetic: debug-assert overflow, release wrap, as ADR #4
// specifies for every interior operation. `checked_*` is the boundary form.

impl<K: ClockKind> Add for Span<K> {
    type Output = Span<K>;

    #[inline]
    fn add(self, rhs: Span<K>) -> Span<K> {
        Span::from_nanos(self.nanos + rhs.nanos)
    }
}

impl<K: ClockKind> Sub for Span<K> {
    type Output = Span<K>;

    #[inline]
    fn sub(self, rhs: Span<K>) -> Span<K> {
        Span::from_nanos(self.nanos - rhs.nanos)
    }
}

impl<K: ClockKind> Neg for Span<K> {
    type Output = Span<K>;

    #[inline]
    fn neg(self) -> Span<K> {
        Span::from_nanos(-self.nanos)
    }
}

// These impls are hand-written and unbounded on `K` on purpose. A derive would
// generate `K: Clone`-style bounds that the zero-sized markers do not usefully
// satisfy. Each one compares or hashes the nanosecond count only, and because
// the impl is over a single `K`, it can never relate two clock kinds.

impl<K: ClockKind> Clone for Timestamp<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: ClockKind> Copy for Timestamp<K> {}

impl<K: ClockKind> PartialEq for Timestamp<K> {
    fn eq(&self, other: &Self) -> bool {
        self.nanos == other.nanos
    }
}

impl<K: ClockKind> Eq for Timestamp<K> {}

impl<K: ClockKind> PartialOrd for Timestamp<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: ClockKind> Ord for Timestamp<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nanos.cmp(&other.nanos)
    }
}

impl<K: ClockKind> Hash for Timestamp<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.nanos.hash(state);
    }
}

impl<K: ClockKind> fmt::Debug for Timestamp<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Timestamp<{}>({})",
            core::any::type_name::<K>(),
            self.nanos
        )
    }
}

impl<K: ClockKind> Clone for Span<K> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<K: ClockKind> Copy for Span<K> {}

impl<K: ClockKind> PartialEq for Span<K> {
    fn eq(&self, other: &Self) -> bool {
        self.nanos == other.nanos
    }
}

impl<K: ClockKind> Eq for Span<K> {}

impl<K: ClockKind> PartialOrd for Span<K> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<K: ClockKind> Ord for Span<K> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nanos.cmp(&other.nanos)
    }
}

impl<K: ClockKind> Hash for Span<K> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.nanos.hash(state);
    }
}

impl<K: ClockKind> fmt::Debug for Span<K> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Span<{}>({}ns)", core::any::type_name::<K>(), self.nanos)
    }
}

/// How far behind the venue's clock an event arrived, in nanoseconds.
///
/// **The only place the exchange clock and the receive clock are compared.**
/// Everywhere else that does not compile, because a market-state decision made
/// across two clocks reads the market as of a moment it was never in. Latency
/// measurement is the one use `CONTEXT.md` sanctions for receive time, and it
/// gets a named function so the exception is countable rather than a
/// subtraction somebody talks themselves into at a call site.
///
/// Positive is the normal direction: the venue stamped it, then it reached us.
/// **Negative means the local clock is behind the venue's**, which is clock
/// skew and is reported rather than clamped — it is the one condition that
/// makes every other lag figure in the same run untrustworthy.
///
/// Returns nanoseconds rather than a [`Span`], because a `Span` carries the
/// clock kind it was measured on and this quantity belongs to neither.
///
/// This is a measurement, never an input to a decision. Nothing on the order
/// path may call it.
#[inline]
pub fn feed_lag_nanos(exchange: ExchangeTime, receive: ReceiveTime) -> i64 {
    receive.to_nanos().saturating_sub(exchange.to_nanos())
}
