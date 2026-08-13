---

description: "Task list for issue #6 — fixed-point value types and source-typed timestamps"
---

# Tasks: Fixed-Point Value Types And Source-Typed Timestamps

**Input**: Design documents from `/specs/006-value-types/`

**Prerequisites**: [plan.md](plan.md), [spec.md](spec.md), [research.md](research.md),
[data-model.md](data-model.md), [contracts/](contracts/)

**Tests**: MANDATORY (Constitution Principle IV). Every test in the spec's
Required Tests is written and observed FAILING in Phase 2, before any behavior
exists. Every `ValueError` variant has a task that proves a test reaches it.

**Organization**: grouped by the spec's contract scenarios S1–S6, which map to
US1–US6 below.

## Format: `[ID] [P?] [Story] Description`

- **[P]**: can run in parallel — different files, no dependency on incomplete work
- **[Story]**: US1–US6, mapping to spec scenarios S1–S6
- Paths are relative to the repository root (`ultra_trade/`)

## Story map

| Story | Spec scenario | Priority | Proves |
|---|---|---|---|
| US1 | S1 — exact values across the whole range | P1 | TEST-001, 002 (+ compile-fail 006, 007, 009) |
| US2 | S2 — a price plus a quantity does not build | P1 | TEST-003, 004, 005 (+ compile-fail 008) |
| US3 | S3 — two clock kinds never meet | P1 | TEST-015, 016 (+ compile-fail 017–020) |
| US4 | S4 — rounding states its direction | P2 | TEST-013, 014 (+ compile-fail 010) |
| US5 | S5 — no quiet out-of-range value | P2 | TEST-011, 012 |
| US6 | S6 — `types` is the bottom of the dependency order | P3 | TEST-022, 023 |

## PR mapping

Phases land in the three PRs from [plan.md](plan.md):

- **PR 1** — Phases 1–2. Workspace, skeleton, and the entire test suite, all RED.
- **PR 2** — Phases 3, 4, 6, 7 (US1, US2, US4, US5). The money half.
- **PR 3** — Phases 5, 8 (US3, US6). The clock half.

PR 2 and PR 3 touch disjoint modules and may land in either order. Phase 9
finishes whichever lands last.

---

## Phase 1: Setup (Shared Infrastructure)

**Purpose**: the workspace and a skeleton whose signatures are complete and whose
bodies are not. Complete signatures matter: a compile-fail test must fail because
the operation is rejected, not because the method does not exist yet.

- [X] T001 Create the workspace manifest at `Cargo.toml` with `members = ["crates/types"]`, `resolver = "3"`, and a `[workspace.package]` block carrying edition 2024
- [X] T002 [P] Pin the toolchain in `rust-toolchain.toml` — channel `1.93.0`, components `rustfmt` and `clippy` (research.md R-001)
- [X] T003 [P] Create `crates/types/Cargo.toml` with an **empty** `[dependencies]` table and `[dev-dependencies]` `proptest` and `trybuild` (R-003, R-004)
- [X] T004 Create `crates/types/src/lib.rs` with `#![no_std]`, `#![forbid(unsafe_code)]`, `pub const SCALE: i64 = 1_000_000_000;`, module declarations, and the public re-exports listed in `contracts/public-api.md` (R-002)
- [X] T005 [P] Declare `ValueError` with its three variants plus `Display` and `core::error::Error` over `&'static str` in `crates/types/src/error.rs` (R-009)
- [X] T006 [P] Declare the `RoundDir` enum (`Up`, `Down`, `TowardZero`, `AwayFromZero`, no `Default`) in `crates/types/src/rounding.rs`
- [X] T007 [P] Stub `Px`, `Qty`, `Notional` in `crates/types/src/money.rs` — private backing fields, every signature from `contracts/public-api.md` with `unimplemented!()` bodies, and the hand-written `Clone`/`Copy`/`PartialEq`/`Eq`/`PartialOrd`/`Ord`/`Hash`/`Debug` impls
  - NOTE: money uses `#[derive]` for these impls, not hand-written ones. The hand-written form is only needed where a `PhantomData<K>` field would generate spurious `K: Clone` bounds — that is `time.rs` (T008), not `money.rs`. Intent (the impls exist in the skeleton so compile-fail cases fail for the right reason) is met.
- [X] T008 [P] Stub the clock layer in `crates/types/src/time.rs` — sealed `ClockKind`, the three markers, `Timestamp<K>` and `Span<K>` with `PhantomData<fn() -> K>`, the six type aliases, every signature unimplemented, and the trait impls hand-written unbounded on `K` (R-005)
- [X] T009 Confirm the skeleton compiles and is clean: `cargo build -p types`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`

**Checkpoint**: the public surface exists and compiles; no behavior does.

---

## Phase 2: RED Tests (Blocking Prerequisites)

**Purpose**: every test in the spec's Required Tests, written and observed
failing.

**⚠️ CRITICAL**: no task in Phase 3 or later may begin until T020 records which
tests fail and why (Constitution IV).

- [X] T010 [P] Write TEST-001 and TEST-002 in `crates/types/tests/roundtrip.rs` — proptest over each type's full backing range, with `MIN`, `MAX`, `0`, and `±1` scale unit asserted directly rather than left to sampling (SC-003)
- [X] T011 [P] Write TEST-003, TEST-004, TEST-005 in `crates/types/tests/notional.rs` — exactness when representable and direction-independent, the spec's notional product vectors including negatives, and totality over all `i64` pairs including both minima
- [X] T012 [P] Write TEST-011, TEST-012, TEST-013, TEST-014 in `crates/types/tests/boundaries.rs` — each reaching a named `ValueError` variant, plus the spec's tick rounding vectors and idempotence
- [X] T013 [P] Write TEST-015 and TEST-016 in `crates/types/tests/time.rs` — same-kind ordering, subtraction, checked span application, and `MAX - MIN` totality per kind
- [X] T014 [P] Write TEST-021 in `crates/types/tests/no_alloc.rs` with its own counting `#[global_allocator]`, armed after harness startup (R-007)
- [X] T015 [P] Write TEST-022 and TEST-023 in `crates/types/tests/structure.rs` — size pins, and the `include_str!("../Cargo.toml")` assertion that `[dependencies]` is empty (R-008)
- [X] T016 [P] Write the trybuild harness in `crates/types/tests/compile_fail.rs`
- [X] T017 [P] Write the five money cases in `crates/types/tests/compile-fail/` — `px_plus_qty.rs`, `px_compared_to_qty.rs`, `px_times_qty_operator.rs`, `float_conversion_either_direction.rs`, `round_without_direction.rs` (contracts/illegal-operations.md)
- [X] T018 [P] Write the four clock cases in `crates/types/tests/compile-fail/` — `exchange_minus_receive.rs`, `exchange_compared_to_monotonic.rs`, `monotonic_converted_to_exchange.rs`, `span_kinds_mixed.rs`
- [X] T019 Generate the nine `.stderr` files with `TRYBUILD=overwrite cargo test -p types --test compile_fail`, then read each one and confirm it is the intended type error — not an unresolved name (R-004)
- [X] T020 Run `cargo test -p types` and record, per test, whether it failed from an unimplemented stub or from a build rejection; this table is PR 1's evidence (quickstart.md)

**Checkpoint**: every test RED for a reason someone has read. Implementation may begin.

---

## Phase 3: User Story 1 - Exact values across the whole range (Priority: P1) 🎯 MVP

**Goal**: every value is an exact multiple of one scale unit, round trips
exactly, and adds like an integer.

**Independent Test**: `cargo test -p types --test roundtrip` passes over each
type's full backing range.

- [X] T021 [US1] Implement `from_scaled`, `to_scaled`, and the `ZERO`/`MIN`/`MAX` constants for `Px`, `Qty`, and `Notional` in `crates/types/src/money.rs`
- [X] T022 [US1] Implement the interior `Add`, `Sub`, and `Neg` operators for all three types in `crates/types/src/money.rs`, relying on Rust's built-in debug-overflow panic and release wrap (FR-011, ADR #4 — do **not** make these checked)
- [X] T023 [US1] Confirm TEST-001 and TEST-002 are green: `cargo test -p types --test roundtrip`
- [X] T024 [US1] Confirm TEST-006, TEST-007, and TEST-009 now fail for the intended reason, with `Px + Px` and `Px < Px` working as their positive control: `cargo test -p types --test compile_fail`

**Checkpoint**: money values are exact and provably so.

---

## Phase 4: User Story 2 - A price plus a quantity does not build (Priority: P1)

**Goal**: the notional product is the one operation relating two value types, and
it names its rounding direction.

**Independent Test**: `cargo test -p types --test notional` passes, and
`px * qty` does not build.

**Depends on**: US1 (`from_scaled`/`to_scaled` must exist for the vectors).

- [X] T025 [US2] Implement the shared `round_to_step(value: i128, step: i128, dir: RoundDir) -> i128` helper in `crates/types/src/rounding.rs` using `div_euclid`/`rem_euclid`, with `#[cfg(test)]` unit tests for the negative-value and exact-multiple cases (R-010)
- [X] T026 [US2] Implement `Px::notional(self, qty: Qty, dir: RoundDir) -> Notional` in `crates/types/src/money.rs` — `i128` intermediate, `round_to_step` at `step = SCALE`, and a bare `Notional` return because the product cannot overflow (INV-004)
- [X] T027 [US2] Add a code comment at `Px::notional` recording INV-004's bound — max product `2^126 ≈ 8.507e37` against `i128::MAX ≈ 1.701e38` — so the missing `Result` reads as proven rather than forgotten
- [X] T028 [US2] Confirm TEST-003, TEST-004, TEST-005 are green and TEST-008 fails for the intended reason: `cargo test -p types --test notional --test compile_fail`

**Checkpoint**: the only cross-type operation works, states its direction, and the operator form is rejected.

---

## Phase 5: User Story 3 - Two clock kinds never meet (Priority: P1)

**Goal**: three clock kinds, ordered and subtractable only against themselves,
with spans that carry their kind.

**Independent Test**: `cargo test -p types --test time` passes and the four clock
compile-fail cases reject.

**Depends on**: nothing in US1/US2 — this phase touches only `time.rs` and can
land before or after the money phases.

- [X] T029 [US3] Implement `Timestamp<K>`'s `from_nanos`, `to_nanos`, `MIN`, and `MAX` in `crates/types/src/time.rs`
  - NOTE: `from_nanos`/`to_nanos`/`MIN`/`MAX` were already real in the Phase 1 skeleton — `MIN` and `MAX` are `const` and a `const` must have a value. Recorded in red-evidence.md as one reason four `time` tests passed before implementation.
- [X] T030 [US3] Implement `Span<K>`'s `from_nanos`, `to_nanos`, `ZERO`, interior `Add`/`Sub`/`Neg`, and `checked_add`/`checked_sub`/`checked_neg` in `crates/types/src/time.rs`
- [X] T031 [US3] Implement `Sub<Timestamp<K>> for Timestamp<K>` yielding `Span<K>` in `crates/types/src/time.rs`, with a comment recording INV-013 — the widest `i64` difference is ~1.845e19 ns, which is why the span is `i128` and the operator is total
- [X] T032 [US3] Implement `Timestamp::<K>::checked_add(Span<K>)` and `checked_sub(Span<K>)` in `crates/types/src/time.rs`, returning `ValueError::Overflow` when the result leaves the `i64` nanosecond window
- [X] T033 [US3] Confirm TEST-015 and TEST-016 are green: `cargo test -p types --test time`
- [X] T034 [US3] Confirm TEST-017, TEST-018, TEST-019, TEST-020 fail for the intended reason, with same-kind ordering and subtraction as their positive control: `cargo test -p types --test compile_fail`

**Checkpoint**: clock skew is not a bug that can be written.

---

## Phase 6: User Story 4 - Rounding states its direction (Priority: P2)

**Goal**: every value crossing a venue boundary is rounded in a direction the
call site named, and a non-positive step is rejected.

**Independent Test**: `cargo test -p types --test boundaries` covers the tick
vector table, and `px.round_to_tick(tick)` does not build.

**Depends on**: US2 (reuses `round_to_step`).

- [X] T035 [US4] Implement `Px::round_to_tick(self, tick: Px, dir: RoundDir)` in `crates/types/src/money.rs` — `InvalidStep` when `tick <= 0`, `Overflow` when the rounded value leaves `i64`, computed in `i128` and narrowed on the way out
- [X] T036 [US4] Implement `Qty::round_to_lot(self, lot: Qty, dir: RoundDir)` in `crates/types/src/money.rs` with the same contract
- [X] T037 [US4] Confirm TEST-013 and TEST-014 are green, including idempotence on exact multiples and the ceiling-overflow arm: `cargo test -p types --test boundaries`
- [X] T038 [US4] Confirm TEST-010 fails for the intended reason — arity, against a working two-argument call: `cargo test -p types --test compile_fail`

**Checkpoint**: no rounding happens that a call site did not ask for.

---

## Phase 7: User Story 5 - No quiet out-of-range value (Priority: P2)

**Goal**: an out-of-range value cannot be introduced, and every boundary
operation says so with a `Result`.

**Independent Test**: `cargo test -p types --test boundaries` reaches
`OutOfRange` and `Overflow` from construction, aggregation, and negation.

**Depends on**: US1. The clock half of S5 (`Timestamp::checked_add`) landed in
US3 for module cohesion — stated here so the scenario's coverage is traceable
rather than assumed.

- [X] T039 [US5] Implement `Px::try_from_units(i64)`, `Qty::try_from_units(i64)`, and `Notional::try_from_units(i128)` in `crates/types/src/money.rs`, returning `OutOfRange` when the scaled form leaves the backing range (R-006 — note the `i128` parameter on `Notional`, without which the arm is unreachable)
- [X] T040 [US5] Implement `checked_add`, `checked_sub`, and `checked_neg` for all three value types in `crates/types/src/money.rs`, with `checked_neg` on the minimum returning `OutOfRange` rather than wrapping
- [X] T041 [US5] Confirm TEST-011 and TEST-012 are green: `cargo test -p types --test boundaries`
- [X] T042 [US5] Confirm every `ValueError` variant is reached by at least one test — `OutOfRange` (TEST-011, 012), `Overflow` (TEST-012, 014, 015), `InvalidStep` (TEST-013) — and record any variant with no test hit as a blocker, not a detail (Principle IV)

**Checkpoint**: fail-closed arithmetic, with each error arm exercised.

---

## Phase 8: User Story 6 - `types` is the bottom of the dependency order (Priority: P3)

**Goal**: the structural guards that keep Principle I and ADR #4's cache
argument true.

**Independent Test**: `cargo test -p types --test structure`.

- [X] T043 [US6] Confirm `crates/types/Cargo.toml` still has an empty `[dependencies]` table, and add a comment in `crates/types/tests/structure.rs` recording R-008's gap — a text assertion cannot see a workspace-inherited dependency, and closing that is D0.4 (→ #1)
- [X] T044 [US6] Confirm TEST-023 is green: `cargo test -p types --test structure`
- [X] T045 [US6] Confirm TEST-022 is green — `Px` and `Qty` are 8 bytes, `Notional` is 16 — which is the precondition for ADR #4's rejection of "`i128` for everything"

**Checkpoint**: the boundaries are enforced by a test rather than by memory.

---

## Phase 9: Polish & Cross-Cutting Concerns

- [X] T046 [P] Add rustdoc to every public item in `crates/types/src/`, each stating its unit and, where relevant, its rounding or overflow contract; define each hard term once (Principle IX)
  - NOTE: enforced from the first commit by `#![deny(missing_docs)]` in `lib.rs` rather than retrofitted. `RUSTDOCFLAGS="-D warnings" cargo doc -p types --no-deps` is clean, so no intra-doc link is broken.
- [X] T047 Confirm TEST-021 is green across the full operation surface: `cargo test -p types --test no_alloc`
- [X] T048 Run the full gate from the repository root and capture the output verbatim: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test --workspace`
- [X] T049 Walk `specs/006-value-types/quickstart.md` end to end on a clean checkout and correct anything that does not match reality
- [X] T050 Write the PR bodies citing spec `specs/006-value-types/`, the scenario IDs, the TEST IDs turned green, the task IDs completed, a "Design deltas" section versus issue #6, and an "Out of scope" list with tracking links (`specs/README.md`, Constitution gates)
- [X] T051 State explicitly in the PR body what was not run: no benchmarks and no latency budget (D4.2 → #1), no CI (D0.4 → #1), and whether any operation allocates (expected: none)

---

## Dependencies & Execution Order

### Phase dependencies

- **Phase 1 (Setup)**: no dependencies.
- **Phase 2 (RED tests)**: needs Phase 1's complete signatures. **Blocks every implementation phase.**
- **Phase 3 (US1)**: after T020.
- **Phase 4 (US2)**: after US1 — the vectors need `from_scaled`/`to_scaled`.
- **Phase 5 (US3)**: after T020. Independent of the money phases; `time.rs` shares no file with them.
- **Phase 6 (US4)**: after US2 — reuses `round_to_step`.
- **Phase 7 (US5)**: after US1.
- **Phase 8 (US6)**: after US1 and US3 (TEST-022 pins sizes for both halves).
- **Phase 9 (Polish)**: after everything.

### The one cross-story dependency worth knowing

`RoundDir` and `round_to_step` serve both the notional product (US2, P1) and tick
and lot rounding (US4, P2). The helper is implemented in US2 because that is the
higher-priority consumer, so the dependency runs from lower priority to higher
and never blocks the MVP.

### Parallel opportunities

- Phase 1: T002, T003, and T005–T008 are different files and run together after T001.
- Phase 2: T010–T018 are nine different files and all run together. This is the widest parallel window in the feature.
- Phases 3–8: US3 (`time.rs`) runs fully parallel with the money phases (`money.rs`, `rounding.rs`). That is the PR 2 / PR 3 split.
- Within the money phases, tasks are sequential: they edit the same `money.rs`.

---

## Parallel Example: Phase 2

```bash
# Nine independent files — write them together:
Task: "TEST-001, 002 in crates/types/tests/roundtrip.rs"
Task: "TEST-003, 004, 005 in crates/types/tests/notional.rs"
Task: "TEST-011..014 in crates/types/tests/boundaries.rs"
Task: "TEST-015, 016 in crates/types/tests/time.rs"
Task: "TEST-021 in crates/types/tests/no_alloc.rs"
Task: "TEST-022, 023 in crates/types/tests/structure.rs"
Task: "trybuild harness in crates/types/tests/compile_fail.rs"
Task: "five money cases in crates/types/tests/compile-fail/"
Task: "four clock cases in crates/types/tests/compile-fail/"

# Then serially: T019 (.stderr review), T020 (record the RED table).
```

## Parallel Example: two developers after Phase 2

```bash
# Developer A — PR 2, the money half:
Phases 3, 4, 6, 7  (money.rs, rounding.rs)

# Developer B — PR 3, the clock half:
Phase 5           (time.rs)

# Neither touches the other's file. Phase 8 needs both; Phase 9 closes.
```

---

## Implementation Strategy

### MVP

Phases 1 → 2 → 3. That is a workspace, a full RED suite, and `Px`/`Qty`/
`Notional` that round trip and add exactly. It is provable on its own and it is
what everything above `types` is waiting for.

### Incremental delivery

1. **PR 1** — Phases 1–2. Everything RED, with the reasons recorded. Reviewable as a test-design review, separately from any implementation.
2. **PR 2** — Phases 3, 4, 6, 7. The money half green.
3. **PR 3** — Phases 5, 8. The clock half green.
4. Phase 9 lands with whichever of PR 2 / PR 3 is second.

### Stop and ask

- If implementing ADR #4's table shows it is wrong — that is an ADR amendment, not a mid-task judgment call (issue #6).
- If a needed noun is missing from `CONTEXT.md` and its meaning is not obvious.
- Before widening any limit, loosening a tolerance, or making a test pass by changing the test. None of those is an available move here.

---

## Notes

- Do not tick a task unless every noun and verb in it is implemented. A partial task stays open with a NOTE naming the landed subset.
- Every "confirm green" task names the exact command; its output belongs in the PR body (SC-001, SC-005).
- No task adds `serde`, decimal parsing, division, a `Nearest` rounding mode, CI, or a benchmark. Each is a deferral in the spec with a tracking link.
- `cargo fetch` is needed once for `proptest` and `trybuild`; after that the whole gate runs `--offline` (R-012).

---

## Landed

PR [#7](https://github.com/eelxpeng/AIQuant/pull/7) — all three planned PRs merged as one
branch (`feat/6-value-types`) with two commits: the spec and design docs, then
the crate. The RED state is recorded in [red-evidence.md](red-evidence.md)
rather than as its own commit, because the skeleton was never staged as one —
an after-the-fact RED commit would be a commit the gate was never run against.
