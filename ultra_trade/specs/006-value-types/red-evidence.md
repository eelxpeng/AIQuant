# RED evidence: PR 1 (T020)

**Feature**: `specs/006-value-types` · **Issue**: #6 · **Date**: 2026-08-12

Constitution IV requires tests written and observed failing before
implementation. This is that observation, taken against the skeleton from
Phase 1 — every signature present, every body `unimplemented!()`.

Command:

```sh
cargo test -p types --no-fail-fast
```

Result: **46 failed, 7 passed**, across 53 test functions.

## Failures, by reason

| Target | Failed | Reason |
|---|---|---|
| `roundtrip` (TEST-001, 002) | 16 | panic at `money.rs` — `from_scaled`/`to_scaled` and the interior operators are unimplemented |
| `notional` (TEST-003, 004, 005) | 7 | panic at `money.rs` — `Px::notional` is unimplemented |
| `boundaries` (TEST-011…014) | 13 | panic at `money.rs` — `try_from_units`, `checked_*`, and the rounding entry points are unimplemented |
| `time` (TEST-015, 016) | 9 | panic at `time.rs` — timestamp subtraction, `checked_add`/`checked_sub`, and span arithmetic are unimplemented |
| `no_alloc` (TEST-021) | 1 | panic while exercising the surface — reaches the unimplemented money operations |

Every failure is a stub panic. None is a compile error, a missing name, or a
wrong assertion: the tests address an API that exists and does nothing.

## Passing at the skeleton stage, and why that is correct

Seven tests pass before any behavior exists. Each asserts a *declaration*, not a
behavior, and declarations are what Phase 1 delivers:

| Test | Why it already passes |
|---|---|
| `illegal_operations_do_not_compile` (TEST-006…010, 017…020) | All nine cases reject. See the caveat below — this is the one result that is not yet evidence. |
| `types_has_no_workspace_dependency` (TEST-023) | The manifest's `[dependencies]` table is empty from the first commit |
| `value_type_sizes_are_pinned` (TEST-022) | The struct definitions carry the widths; no method body is involved |
| `a_timestamp_round_trips_its_nanosecond_count` | `Timestamp::from_nanos`/`to_nanos` are real in the skeleton — `MIN`/`MAX` are associated constants and a `const` must have a value |
| `timestamps_of_one_kind_are_ordered_by_their_count`, `timestamp_ordering_is_exact` | The hand-written `Ord` impl is in the skeleton on purpose (see below) |
| `a_span_can_be_built_directly_with_its_kind_named` | Same: `Span::ZERO` is a `const` and forces a real `from_nanos` |

## The caveat that matters

`illegal_operations_do_not_compile` passing here proves **nothing yet**. A
compile-fail case passes if the code fails to build for *any* reason, including
"the method does not exist".

That is why Phase 1 lands the *complete* signature set rather than an empty
skeleton, and why the `.stderr` files are checked in. Reviewing them (T019)
shows each case fails for the intended reason:

| Case | rustc error |
|---|---|
| `px_plus_qty` | `E0308` mismatched types |
| `px_compared_to_qty` | `E0308` mismatched types |
| `px_times_qty_operator` | `E0369` cannot multiply `Px` by `Qty` |
| `float_conversion_either_direction` | `E0308`, `E0277` `f64: From<Px>` unsatisfied, `E0605` non-primitive cast |
| `round_without_direction` | `E0061` this method takes 2 arguments but 1 was supplied |
| `exchange_minus_receive` | `E0308` expected `Timestamp<Exchange>`, found `Timestamp<Receive>` |
| `exchange_compared_to_monotonic` | `E0308` mismatched types |
| `monotonic_converted_to_exchange` | `E0308`, `E0277` `Timestamp<Exchange>: From<Timestamp<Monotonic>>` unsatisfied |
| `span_kinds_mixed` | `E0308` expected `Span<Exchange>`, found `Span<Receive>` |

The remaining half of the evidence is each case's positive control — TEST-002,
003, 004, 014, 015, 016 — which only goes green in PR 2 and PR 3. Until then the
compile-fail suite is necessary but not sufficient.
