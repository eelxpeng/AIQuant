# Phase 1 Data Model: Value Types And Source-Typed Timestamps

**Feature**: `specs/006-value-types` · **Issue**: #6 · **Date**: 2026-08-12

Every entity here is an immutable value. There is no state, no lifecycle, and no
transition table — a `Px` is not a thing that changes, it is a number that is
either exactly representable or does not exist. The "validation rules" column is
therefore about what may be *constructed*, not about what may be *mutated*.

---

## Value types

| Entity | Backing | Scale | Size | Range |
|---|---|---|---|---|
| `Px` | `i64` (private) | `1e-9` | 8 bytes | ±9_223_372_036.854775807 |
| `Qty` | `i64` (private) | `1e-9` | 8 bytes | ±9_223_372_036.854775807 |
| `Notional` | `i128` (private) | `1e-9` | 16 bytes | ±1.70141183460469231731687303715884105727e29 |

`i64::MIN` is one scale unit further negative than `MAX` is positive and has no
positive counterpart, which is why negation is a checked operation.

**Shared rules** (INV-001, INV-002, INV-010):

- The backing integer is private and reachable only through `from_scaled` /
  `to_scaled` (FR-003).
- Every value is an exact integer multiple of one scale unit. There is no
  representable value between two scale units, so "loses precision" is not a
  state a value can be in — an operation either lands on a scale unit or names
  the direction it rounds toward.
- `PartialEq`/`Eq`/`PartialOrd`/`Ord` compare backing integers. No tolerance
  parameter exists anywhere.
- No `From`/`Into`/`TryFrom` between a value type and `f32` or `f64`, in either
  direction.

**Relationships**: exactly one, `Px × Qty → Notional` via `Px::notional`, which
takes a rounding direction (ADR #6). There is no other operation relating two
value types, and no `Mul` operator between them.

---

## Rounding

| Entity | Shape | Notes |
|---|---|---|
| `RoundDir` | unit enum: `Up`, `Down`, `TowardZero`, `AwayFromZero` | `Copy`; no default; no `Nearest` variant (deferred → #1) |

`Up` is toward +∞ and `Down` toward −∞, so on negative values `Down` and
`TowardZero` differ. A value already on an exact multiple of the step is returned
unchanged for every direction (INV-005).

The step for `Px::round_to_tick` is a `Px` and for `Qty::round_to_lot` is a
`Qty` — a tick size is a price and a lot size is a quantity, so no `Instrument`
type is needed here. A step of zero or a negative step is `ValueError::InvalidStep`.

---

## Clock kinds, timestamps, and spans

| Entity | Shape | Notes |
|---|---|---|
| `ClockKind` | sealed marker trait | Cannot be implemented downstream — three kinds, permanently |
| `Exchange`, `Receive`, `Monotonic` | zero-sized marker types | The `CONTEXT.md` clocks, one type each |
| `Timestamp<K: ClockKind>` | `i64` nanoseconds (private) + `PhantomData<fn() -> K>` | 8 bytes; window ≈1678–2262 |
| `Span<K: ClockKind>` | `i128` nanoseconds (private) + `PhantomData<fn() -> K>` | 16 bytes; total under timestamp subtraction (INV-013) |

**Aliases** (the names the spec and `CONTEXT.md` use):

| Alias | Expands to | Origin of the count |
|---|---|---|
| `ExchangeTime` | `Timestamp<Exchange>` | Unix epoch |
| `ReceiveTime` | `Timestamp<Receive>` | Unix epoch |
| `MonotonicTime` | `Timestamp<Monotonic>` | opaque process-local origin; absolute value is meaningless |
| `ExchangeSpan` | `Span<Exchange>` | — |
| `ReceiveSpan` | `Span<Receive>` | — |
| `MonotonicSpan` | `Span<Monotonic>` | — |

**Rules** (INV-007, INV-013):

- Ordering and equality are defined by `impl<K: ClockKind> Ord for Timestamp<K>`,
  which by construction can only ever compare two timestamps carrying the same
  `K`. Cross-kind comparison is not "false", it is not expressible.
- `Timestamp<K> - Timestamp<K> → Span<K>` is total: the widest difference two
  `i64` nanosecond values can have is ~1.845e19, and a span holds `i128`.
- `Timestamp<K> + Span<K>` is checked — that direction can leave the `i64`
  window — and is spelled `checked_add`.
- A `Span<Receive>` and a `Span<Exchange>` share no operation. Relating two
  clock kinds is reserved for a named skew conversion that does not exist yet
  (ADR #6, deferred → #1).
- Nothing here reads a clock. A timestamp arrives as a value from the caller.

---

## Errors

| Variant | Raised by | Meaning |
|---|---|---|
| `ValueError::OutOfRange` | `try_from_units`, `checked_neg` | the value cannot be represented at all |
| `ValueError::Overflow` | `checked_add`/`checked_sub`, rounding near a bound, `Timestamp::checked_add` | the operation's result left the representable range |
| `ValueError::InvalidStep` | `round_to_tick`, `round_to_lot` | the tick or lot step was zero or negative |

`Copy`, no payload, no heap data (INV-009). Each variant has a test that reaches
it: TEST-011 and TEST-012 (`OutOfRange`), TEST-012, TEST-014 and TEST-015
(`Overflow`), TEST-013 (`InvalidStep`).

---

## Constants

| Constant | Value | Purpose |
|---|---|---|
| `SCALE` | `1_000_000_000` | scale units per whole unit — the single source of truth (ADR #4) |
| `Px::ZERO`, `Qty::ZERO`, `Notional::ZERO` | `0` | additive identity; the empty-aggregation result |
| `Px::MIN`/`MAX`, `Qty::MIN`/`MAX`, `Notional::MIN`/`MAX` | backing bounds | boundary cases the property tests assert directly |
| `Span::<K>::ZERO` | `0` ns | — |

There is no `type Backing = i64` alias. ADR #4 considered and rejected it; adding
one silently reverses that decision.
