# Contract: operations that must not compile

**Feature**: `specs/006-value-types` · **Issue**: #6 · **Date**: 2026-08-12

The negative half of the [public API contract](public-api.md). Each row is one
`trybuild` case under `crates/types/tests/compile-fail/`, with a checked-in
`.stderr` so a case cannot start passing for the wrong reason — a name typo that
fails to resolve is not the same failure as a type check that rejects the
operation (R-004).

Nine cases. Every one is RED-by-construction before implementation: with stub
types that compile but are unimplemented, the illegal operations already fail,
so slice 1's evidence is that each fails **for the stated reason**.

## Value types

| Test | Case file | Illegal code | Guards |
|---|---|---|---|
| TEST-006 | `px_plus_qty.rs` | `px + qty`, `qty + px` | INV-003 — no `Add` across value types |
| TEST-007 | `px_compared_to_qty.rs` | `px < qty`, `px == qty` | INV-003 — no `PartialOrd`/`PartialEq` across value types |
| TEST-008 | `px_times_qty_operator.rs` | `px * qty`, `qty * px` | FR-006 — a bare operator cannot name a rounding direction |
| TEST-009 | `float_conversion_either_direction.rs` | `Px::from(1.5_f64)`, `let _: f64 = px.into()`, `px as f64` | INV-002 — no float conversion in either direction |
| TEST-010 | `round_without_direction.rs` | `px.round_to_tick(tick)`, `qty.round_to_lot(lot)` | INV-005 — no default direction, no single-argument entry point |

## Clocks

| Test | Case file | Illegal code | Guards |
|---|---|---|---|
| TEST-017 | `exchange_minus_receive.rs` | `exchange_time - receive_time` | INV-007 — the skew subtraction, the whole point of typed clocks |
| TEST-018 | `exchange_compared_to_monotonic.rs` | `exchange_time < monotonic_time`, `exchange_time == monotonic_time` | INV-007 — comparison, which subtraction does not cover |
| TEST-019 | `monotonic_converted_to_exchange.rs` | `ExchangeTime::from(monotonic_time)`, `let _: ExchangeTime = monotonic_time.into()` | INV-007 — conversion, which arithmetic does not cover |
| TEST-020 | `span_kinds_mixed.rs` | `exchange_time.checked_add(receive_span)`, `receive_span + exchange_span`, `receive_span < monotonic_span` | FR-020 — a span carries its kind and cannot cross |

## Why each case needs a positive control

A compile-fail test passes if the code fails to build for *any* reason,
including the operation not existing at all. Each guard is therefore paired with
a test that proves the same-type or same-kind operation **does** work:

| Compile-fail case | Positive control |
|---|---|
| TEST-006, TEST-007 | TEST-002 — `Px + Px` and `Px < Px` work and are exact |
| TEST-008 | TEST-003, TEST-004 — `px.notional(qty, dir)` builds and yields a `Notional` |
| TEST-009 | TEST-001 — `from_scaled`/`to_scaled` is the only conversion, and it round trips |
| TEST-010 | TEST-014 — the two-argument rounding call works across the direction table |
| TEST-017, TEST-018, TEST-019 | TEST-015 — same-kind ordering and subtraction work |
| TEST-020 | TEST-015, TEST-016 — `Timestamp<K> + Span<K>` works and same-kind subtraction is total |

## What this set does not cover

- **Raw-integer misuse downstream.** `to_scaled()` hands out an `i64`, and
  nothing stops a caller doing arithmetic on it. That is greppable in review, not
  compile-enforceable, and it is the accepted cost recorded in ADR #6.
- **A fourth clock kind.** `ClockKind` is sealed, so this is unreachable rather
  than tested.
- **Floats reached indirectly.** `px.to_scaled() as f64` compiles, because that
  is a cast on an `i64`. INV-002 is about the value type's own surface.
