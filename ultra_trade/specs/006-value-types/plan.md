# Implementation Plan: Fixed-Point Value Types And Source-Typed Timestamps

**Branch**: `006-value-types` | **Date**: 2026-08-12 | **Spec**: [spec.md](spec.md)

**Input**: Feature specification from `/specs/006-value-types/spec.md`

## Summary

Stand up the workspace and its first crate, `types`, holding `Px`, `Qty`, and
`Notional` at ADR #4's widths and scale, plus `ExchangeTime`, `ReceiveTime`, and
`MonotonicTime` as three clock kinds that cannot meet. The contract is proven by
property tests over each type's full backing range and by nine tests that assert
non-compilation.

The approach, from Phase 0 research: a `no_std`, zero-dependency,
`forbid(unsafe_code)` library where the compiler carries as much of the contract
as it can. Clock kinds are a sealed type parameter so `Timestamp<K>` and
`Span<K>` are written once; spans are `i128` so timestamp subtraction is total;
one rounding helper over `i128` serves tick rounding, lot rounding, and the
notional product. Two dev-dependencies (`proptest`, `trybuild`) and nothing else.

## Technical Context

**Language/Version**: Rust 1.93.0 stable, edition 2024, pinned by
`rust-toolchain.toml` with `rustfmt` and `clippy` (R-001)

**Primary Dependencies**: none. The library is `#![no_std]` with an empty
`[dependencies]` table — asserted by TEST-023. Dev-only: `proptest` (R-003),
`trybuild` (R-004)

**Storage**: N/A — no persistence, no serialization, no `serde` (D0.2 owns the
durable format)

**Testing**: `cargo test`; `proptest` for range properties; `trybuild` with
checked-in `.stderr` for non-compilation; a counting `#[global_allocator]` in its
own test target for the zero-allocation proof (R-007)

**Target Platform**: platform-independent. `no_std` and integer-only, so no
target-specific behavior; developed on x86_64-unknown-linux-gnu

**Project Type**: Rust library crate, the first in a new Cargo workspace

**Performance Goals**: no numeric budget yet — tick-to-trade is set by D4.2
(→ #1). Structural targets that bind now: zero allocation in every operation,
`Px`/`Qty` 8 bytes, `Notional` 16, `Span` 16, `i128` intermediate on every
notional product

**Constraints**: no allocation, no `std`, no float in the public surface, no
ambient clock, no `unsafe` in the library, no formatting on the hot path

**Scale/Scope**: one crate, five source modules, 23 required tests (9 of them
compile-fail), ~three PRs

## Constitution Check

*GATE: Must pass before Phase 0 research. Re-check after Phase 1 design.*

| Principle | Gate for this feature | Pre-research | Post-design |
|---|---|---|---|
| I. Layered dependency direction | `types` is the bottom; zero workspace dependencies | PASS | PASS — empty `[dependencies]`, asserted by TEST-023 (R-008) |
| II. Deterministic replay | No ambient clock, RNG, or iteration-order dependence; every operation pure | PASS | PASS — `no_std` removes `SystemTime`/`Instant` from scope entirely (R-002) |
| III. Backtest/live parity | No mode branch, no look-ahead surface | PASS (no modes exist here) | PASS |
| IV. Spec-first, test-first | Spec merged first; RED tests land before behavior; every error arm reached by a test | PASS | PASS — slice 1 is RED-only; each `ValueError` variant is mapped to a test in data-model.md |
| V. Fail-closed risk | Risk surface: none. The applicable part is that ambiguity errors rather than assumes | PASS | PASS — `InvalidStep` on a non-positive step, `OutOfRange`/`Overflow` at every boundary |
| VI. Latency budgets | Hot path yes, no budget yet — spec records why (D4.2) and names the binding constraints | PASS | PASS — sizes pinned by TEST-022, zero-allocation by TEST-021 |
| VII. Exact numerics, explicit clocks | This feature *is* the principle | PASS | PASS — no float conversion, exact comparison, three clock kinds, explicit rounding |
| VIII. Simulated venues / offline tests | Default `cargo test` safe offline with no credentials | PASS | PASS — with the honest caveat that dev-dependencies need one `cargo fetch` (R-012) |
| IX. Plain language | Every noun in `CONTEXT.md` with its `_Avoid_` line | PASS | PASS — `Scale Unit`, `Rounding Direction`, and `Span` added |

**No violations.** Two choices a reviewer will reasonably challenge are argued
in research.md rather than hidden: `#![no_std]` (R-002) and the sealed clock-kind
type parameter (R-005). Neither breaches a principle; both are recorded so the
challenge has something to read.

### Design deltas found during planning

Designing the API surface showed three places where the spec described something
unimplementable. The spec has been corrected; recorded here so the change is
visible rather than silent:

1. **`from_scaled` cannot fail.** Every `i64` is a valid scale-unit count, so the
   spec's "scale-unit count outside the backing range" named an unreachable error
   arm. The checked construction boundary is `try_from_units` (whole units ×
   `SCALE`, which can overflow). S5 acceptance 1 and the first Failure Modes row
   now say "whole units" (R-006).
2. **`Notional::try_from_units` takes `i128`, not `i64`.** An `i64` count of whole
   units always fits, which would again leave the arm unreachable (R-006).
3. **The ≈1678–2262 timestamp window is not a checked construction error.** An
   instant outside it has no `i64` representation, so there is nothing to check.
   The window is only reachable from inside, by adding a span, and that operation
   is checked. The Failure Modes row and the matching edge case were corrected.

## Project Structure

### Documentation (this feature)

```text
specs/006-value-types/
├── plan.md                        # This file
├── spec.md                        # The contract
├── research.md                    # Phase 0 — R-001…R-012
├── data-model.md                  # Phase 1 — entities, ranges, error mapping
├── quickstart.md                  # Phase 1 — how to run and read the gate
├── contracts/
│   ├── public-api.md              # Every signature that exists
│   └── illegal-operations.md      # The nine that must not compile
├── checklists/requirements.md
└── tasks.md                       # /speckit-tasks output — not created here
```

### Source Code (repository root)

```text
ultra_trade/
├── Cargo.toml                     # workspace: members = ["crates/types"]
├── rust-toolchain.toml            # pinned 1.93.0 + rustfmt + clippy
└── crates/types/
    ├── Cargo.toml                 # [dependencies] empty; dev: proptest, trybuild
    ├── src/
    │   ├── lib.rs                 # #![no_std] #![forbid(unsafe_code)]; SCALE; re-exports
    │   ├── error.rs               # ValueError
    │   ├── rounding.rs            # RoundDir + round_to_step over i128 (R-010)
    │   ├── money.rs               # Px, Qty, Notional
    │   └── time.rs                # ClockKind (sealed), Timestamp<K>, Span<K>
    └── tests/
        ├── roundtrip.rs           # TEST-001, 002
        ├── notional.rs            # TEST-003, 004, 005
        ├── boundaries.rs          # TEST-011, 012, 013, 014
        ├── time.rs                # TEST-015, 016
        ├── no_alloc.rs            # TEST-021 — own #[global_allocator]
        ├── structure.rs           # TEST-022, 023
        ├── compile_fail.rs        # trybuild harness
        └── compile-fail/          # 9 cases + checked-in .stderr
```

**Structure Decision**: a Cargo workspace with a single member, `crates/types`,
matching the crate table in `docs/ARCHITECTURE.md`. Modules split by what they
own rather than by layer — there are no layers inside a value crate. Tests are
integration targets (`tests/`) rather than `#[cfg(test)]` modules, because they
exercise the *public* surface, which is what the contract is about; the one
exception would be internal helper unit tests, which `round_to_step` may want.

`tests/no_alloc.rs` must stay its own target: `#[global_allocator]` is
per-binary, and `proptest` allocates freely, so mixing them would measure the
framework instead of the crate (R-007).

## PR sequence

Follows the intake issue's plan and `specs/README.md`'s preferred sequence.

| PR | Contents | Green when |
|---|---|---|
| 1 | Workspace, `rust-toolchain.toml`, `crates/types` skeleton with unimplemented stubs, and the **entire** test suite | Nothing. Every test is RED, and the PR body states which reason each failed for (see quickstart.md) |
| 2 | `Px`, `Qty`, `Notional`: constructors, checked and interior arithmetic, rounding, the notional product | TEST-001…014 (including the five money compile-fail cases and their positive controls), 021…023 |
| 3 | `ClockKind`, `Timestamp<K>`, `Span<K>` and their compile-time separation | TEST-015…020 |

PRs 2 and 3 touch disjoint modules and can land in either order.

## Complexity Tracking

> Fill ONLY if Constitution Check has violations that must be justified

No violations. Table intentionally empty.
