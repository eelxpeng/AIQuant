# Contract: the `types` public API

**Feature**: `specs/006-value-types` · **Issue**: #6 · **Date**: 2026-08-12

This crate's contract with the rest of the system is its public surface — there
is no wire protocol, no CLI, and no endpoint. Signatures below are normative:
anything not listed does not exist, and the companion
[illegal-operations.md](illegal-operations.md) lists what must actively fail to
compile.

```rust
#![no_std]
#![forbid(unsafe_code)]
```

---

## Constants and errors

```rust
/// Scale units per whole unit. The single source of truth (ADR #4).
pub const SCALE: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueError {
    /// The value cannot be represented at all.
    OutOfRange,
    /// The operation's result left the representable range.
    Overflow,
    /// A tick or lot step was zero or negative.
    InvalidStep,
}

impl core::fmt::Display for ValueError;   // &'static str messages, no allocation
impl core::error::Error for ValueError;
```

## Rounding direction

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoundDir {
    Up,           // toward +∞
    Down,         // toward −∞
    TowardZero,
    AwayFromZero,
}
```

No `Default` impl, by design (FR-013).

---

## `Px`

```rust
pub struct Px(/* private i64 */);

impl Px {
    pub const ZERO: Px;
    pub const MIN: Px;
    pub const MAX: Px;

    // Representation window (FR-003). Total: every i64 is a valid scale-unit count.
    pub const fn from_scaled(scaled: i64) -> Px;
    pub const fn to_scaled(self) -> i64;

    // Checked construction boundary (R-006). Errors OutOfRange past ±9.22e9 units.
    pub fn try_from_units(units: i64) -> Result<Px, ValueError>;

    // Boundary arithmetic (FR-010).
    pub fn checked_add(self, rhs: Px) -> Result<Px, ValueError>;
    pub fn checked_sub(self, rhs: Px) -> Result<Px, ValueError>;
    pub fn checked_neg(self) -> Result<Px, ValueError>;

    // Venue boundary. `tick` must be > 0 or InvalidStep (FR-013, FR-015).
    pub fn round_to_tick(self, tick: Px, dir: RoundDir) -> Result<Px, ValueError>;

    // The one operation relating two value types (INV-003). Total (INV-004).
    pub fn notional(self, qty: Qty, dir: RoundDir) -> Notional;
}

// Interior arithmetic: debug-assert overflow, release wrap (FR-011, ADR #4).
impl core::ops::Add for Px { type Output = Px; }
impl core::ops::Sub for Px { type Output = Px; }
impl core::ops::Neg for Px { type Output = Px; }

impl Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug for Px;
```

## `Qty`

Identical to `Px`, with two differences:

```rust
pub fn round_to_lot(self, lot: Qty, dir: RoundDir) -> Result<Qty, ValueError>;
// and no `notional` — the product is spelled `px.notional(qty, dir)`, once.
```

## `Notional`

```rust
pub struct Notional(/* private i128 */);

impl Notional {
    pub const ZERO: Notional;
    pub const MIN: Notional;
    pub const MAX: Notional;

    pub const fn from_scaled(scaled: i128) -> Notional;
    pub const fn to_scaled(self) -> i128;

    // i128 units, not i64: an i64 unit count always fits, which would leave the
    // error arm unreachable (R-006).
    pub fn try_from_units(units: i128) -> Result<Notional, ValueError>;

    pub fn checked_add(self, rhs: Notional) -> Result<Notional, ValueError>;
    pub fn checked_sub(self, rhs: Notional) -> Result<Notional, ValueError>;
    pub fn checked_neg(self) -> Result<Notional, ValueError>;
}

impl core::ops::Add / Sub / Neg for Notional;
impl Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug for Notional;
```

No rounding on `Notional`: it never crosses a venue boundary, so it has no tick
or lot to round to.

---

## Clocks

```rust
mod private { pub trait Sealed {} }

pub trait ClockKind: private::Sealed {}

pub struct Exchange;
pub struct Receive;
pub struct Monotonic;

impl ClockKind for Exchange {}
impl ClockKind for Receive {}
impl ClockKind for Monotonic {}
```

Sealed: no fourth clock kind can be added downstream (R-005).

## `Timestamp<K>` and `Span<K>`

```rust
pub struct Timestamp<K: ClockKind>(/* private i64 nanos, PhantomData<fn() -> K> */);
pub struct Span<K: ClockKind>(/* private i128 nanos, PhantomData<fn() -> K> */);

pub type ExchangeTime   = Timestamp<Exchange>;
pub type ReceiveTime    = Timestamp<Receive>;
pub type MonotonicTime  = Timestamp<Monotonic>;
pub type ExchangeSpan   = Span<Exchange>;
pub type ReceiveSpan    = Span<Receive>;
pub type MonotonicSpan  = Span<Monotonic>;

impl<K: ClockKind> Timestamp<K> {
    pub const MIN: Self;
    pub const MAX: Self;

    pub const fn from_nanos(nanos: i64) -> Self;
    pub const fn to_nanos(self) -> i64;

    // Checked: this direction can leave the ≈1678–2262 window (FR-020).
    pub fn checked_add(self, span: Span<K>) -> Result<Self, ValueError>;
    pub fn checked_sub(self, span: Span<K>) -> Result<Self, ValueError>;
}

// Total — never overflows (INV-013). The only subtraction that yields a Span.
impl<K: ClockKind> core::ops::Sub<Timestamp<K>> for Timestamp<K> {
    type Output = Span<K>;
}

impl<K: ClockKind> Span<K> {
    pub const ZERO: Self;

    // The kind is named at the call site (FR-021):
    //   MonotonicSpan::from_nanos(5_000_000_000)
    pub const fn from_nanos(nanos: i128) -> Self;
    pub const fn to_nanos(self) -> i128;

    pub fn checked_add(self, rhs: Span<K>) -> Result<Span<K>, ValueError>;
    pub fn checked_sub(self, rhs: Span<K>) -> Result<Span<K>, ValueError>;
    pub fn checked_neg(self) -> Result<Span<K>, ValueError>;
}

impl<K: ClockKind> core::ops::Add / Sub / Neg for Span<K>;

// Hand-written, unbounded on K (R-005): a derive would demand `K: Clone` etc.
impl<K: ClockKind> Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug
    for Timestamp<K> and Span<K>;
```

`Timestamp::checked_add`/`checked_sub` take a **`Span<K>`**; the `Sub` operator
takes a **`Timestamp<K>`** and yields a span. The asymmetry is deliberate — those
are two different operations and the parameter type distinguishes them — and it
is why the checked forms are named rather than operators.

---

## Notes binding on the implementation

- **No ambient clock.** No constructor reads one; `no_std` means
  `SystemTime::now()` and `Instant::now()` are not in scope at all (R-002,
  FR-022).
- **`Debug` is implemented** on every public type because `proptest` requires it
  (R-003). It writes through the formatter and allocates nothing, but it is
  formatting and must not be called on the hot path (Principle VI).
- **`SCALE` is exported** as ADR #4's single source of truth. It is not a
  switchability mechanism, and there is no `type Backing = i64` alias — ADR #4
  rejected one.
- **Anything not listed does not exist**: no `Display` on value types, no
  `FromStr`, no `serde`, no decimal parsing, no division, no scalar
  multiplication, no `Nearest` rounding, no cross-clock conversion. Each is a
  deferral in the spec with a tracking link.
