# Phase 0 Research: Value Types And Source-Typed Timestamps

**Feature**: `specs/006-value-types` · **Issue**: #6 · **Date**: 2026-08-12

Everything the spec left to implementation. The representation decisions are
already closed by [ADR #4](../../docs/adr/4-fixed-point-representation.md) and
[ADR #6](../../docs/adr/6-value-type-api-contracts.md) and are not reopened here.

---

## R-001 — Toolchain and edition

**Decision**: Rust 1.93.0 stable, edition 2024, pinned by a `rust-toolchain.toml`
at the workspace root with the `rustfmt` and `clippy` components.

**Rationale**: 1.93.0 is what is installed (`rustc 1.93.0 (254b59607 2026-01-19)`).
The gate is `cargo fmt --check` and `cargo clippy -- -D warnings`, and both are
version-sensitive — an unpinned toolchain means the gate's verdict depends on
whose machine ran it, which makes a merge-blocking check unreproducible. Pinning
costs one manual bump per upgrade and makes that bump a reviewable event.

**Alternatives considered**: no pin (rejected: the fmt/clippy verdict drifts per
machine); an MSRV floor without a pin (rejected: solves compilation
compatibility, not gate reproducibility, and this workspace has no external
consumers to be compatible with).

---

## R-002 — `#![no_std]` for the `types` crate

**Decision**: the crate is `#![no_std]` and `#![forbid(unsafe_code)]`. Tests
bring in `std` via `#[cfg(test)] extern crate std;` and via the separate
integration-test targets.

**Rationale**: this converts three spec invariants from tested to structural.
Without `alloc`, `String` and `Box` cannot appear, so INV-009's "no heap data on
an error path" is enforced by the compiler rather than by review. Without `std`,
`SystemTime::now()` and `Instant::now()` do not exist, so FR-022 cannot be
violated by accident. The crate needs nothing from `std` — every operation is
integer arithmetic.

It does **not** remove floats: `f64` lives in `core`, so INV-002 still needs its
compile-fail test.

**Alternatives considered**: plain `std` with a lint (rejected: a lint is a rule
someone can allow-attribute past, and the whole point of this crate is that the
compiler holds the line); `no_std` plus `alloc` (rejected: `alloc` reintroduces
`String` for no benefit here).

**Reviewer will challenge this.** It is not required by the spec. The counter is
that it is a subtraction, not an addition — it removes capability the crate has
no use for, and each removal closes a named invariant.

---

## R-003 — Property testing: `proptest`

**Decision**: `proptest` as a dev-dependency, with explicit strategies over the
full backing range plus hand-listed boundary cases.

**Rationale**: SC-003 requires both extremes, zero, and the single-scale-unit
step to be *generated* cases rather than assumed ones. `proptest`'s integer
strategies sample boundaries deliberately, its shrinking reports a minimal
counterexample (which matters when a failure is a specific `i64` near a range
end), and failures persist to a regression file so a flake becomes a permanent
case.

Boundary values are additionally asserted directly rather than left to sampling —
a generator that *usually* hits `i64::MIN` is not evidence.

**Alternatives considered**: `quickcheck` (rejected: weaker shrinking, no
regression persistence); hand-written table tests only (rejected: the spec asks
for round-trip and exactness across the *full* range, which is a property, not a
table); `kani`/exhaustive proof (rejected as disproportionate for this issue —
worth revisiting if the arithmetic ever grows a branch that tests cannot cover).

---

## R-004 — Asserting non-compilation: `trybuild`

**Decision**: `trybuild` as a dev-dependency. One `.rs` case per illegal
operation under `crates/types/tests/compile-fail/`, each with a checked-in
`.stderr` expectation, driven by a single `tests/compile_fail.rs` harness.

**Rationale**: the spec requires nine compile-fail cases and the issue says a
commented-out line is not a test. `trybuild` is the conventional mechanism and
asserts on the error text, not just the failure — so a case cannot start passing
for the wrong reason (e.g. a typo making it fail to resolve a name rather than
fail the intended type check).

**Cost to accept**: `.stderr` files are toolchain-sensitive and will need
regeneration (`TRYBUILD=overwrite`) on a compiler bump. R-001's pin makes that a
deliberate event rather than a surprise.

**Alternatives considered**: `compiletest_rs` (rejected: rustc-internal
oriented, heavier); a build script that shells out to `rustc` (rejected:
reinvents `trybuild` with worse diagnostics); documenting the illegal operations
without testing them (rejected by the issue and by Principle IV).

---

## R-005 — Clock kinds as a sealed type parameter

**Decision**: one generic `Timestamp<K>` and one generic `Span<K>`, where `K` is
a zero-sized marker (`Exchange`, `Receive`, `Monotonic`) implementing a **sealed**
`ClockKind` trait. The three timestamp names in the spec are type aliases.

**Rationale**: ADR #6 requires spans to carry their kind. With three concrete
timestamp structs, spans would need three concrete structs too, and relating a
timestamp type to its span type would need an associated-type trait anyway — the
same type-level machinery, spelled six times. Sealing the trait means no
downstream crate can invent a fourth clock kind, which keeps "three clocks, named
in `CONTEXT.md`" true.

**Implementation notes that will otherwise cost a debugging session**:

- The phantom field is `PhantomData<fn() -> K>`, not `PhantomData<K>`. The
  function-pointer form is `Send + Sync` regardless of `K` and keeps the type
  covariant; the bare form leaks `K`'s auto-traits into the timestamp.
- `#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]` on a type with a
  `PhantomData<K>` field generates `K: Clone`-style bounds that the marker types
  do not satisfy usefully. These impls are hand-written, unbounded on `K`.
- `Ord` must be derived-or-written over `nanos` only. Hand-writing it is also
  what keeps `Timestamp<Exchange>` non-comparable to `Timestamp<Receive>`: the
  impl is `impl<K: ClockKind> Ord for Timestamp<K>`, which only ever compares two
  timestamps of the *same* `K`.

**Alternatives considered**: three concrete timestamp structs plus three concrete
span structs (rejected: six hand-written copies of the same arithmetic, and the
compile-fail guarantees have to be re-proven per pair); an unsealed `ClockKind`
(rejected: a fourth kind could appear without a `CONTEXT.md` entry, which is the
noun-drift the constitution's Principle IX exists to stop).

---

## R-006 — Where the checked construction boundary actually is

**Decision**: `from_scaled(i64) -> Self` is **total** — every `i64` is a valid
scale-unit count, so there is nothing to check. The checked construction boundary
is `try_from_units`, which multiplies whole units by `SCALE` and can overflow:
`Px::try_from_units(i64)`, `Qty::try_from_units(i64)`, and
`Notional::try_from_units(i128)`.

**Rationale**: the spec asks for a checked constructor whose error arm a test
reaches (TEST-011). Designing the API showed that the scale-unit constructor
cannot fail, so the spec's original wording ("a scale-unit count outside the
backing range") described an unreachable case. The whole-units constructor is
where an out-of-range value can genuinely be introduced — `Px::try_from_units(10_000_000_000)`
is $10bn, past the `Px` ceiling — and it is the readable constructor at call
sites anyway.

`Notional::try_from_units` takes `i128` rather than `i64` for the same reason: an
`i64` count of whole units times `SCALE` is at most 9.2e27 and always fits, so an
`i64` parameter would leave the error arm unreachable for that type.

**Spec corrected** in this phase: S5 acceptance 1 and the first Failure Modes row
now say "whole units". Also corrected: the timestamp window (≈1678–2262) is not a
checked construction error — an instant outside it has no `i64` representation,
so there is no construction path to check. That window is only reachable from
inside, by adding a span, and that operation is checked.

**Alternatives considered**: a fallible `from_scaled` returning `Result` for
uniformity (rejected: an error arm that cannot be reached is exactly the
"unexploded invariant" Principle IV names); dropping the checked constructor and
losing TEST-011 (rejected: it deletes a real boundary rather than the phantom
one).

---

## R-007 — Proving zero allocation

**Decision**: a dedicated integration-test target `tests/no_alloc.rs` installs a
`#[global_allocator]` that counts allocations in an `AtomicUsize`, then exercises
the full operation surface and asserts the count did not move.

**Rationale**: `#[global_allocator]` is per-binary, and each Rust integration
test file is its own binary — so the counting allocator applies to exactly this
target and nothing else. That isolation matters because `proptest` allocates
freely; putting the allocation assertion in the same binary as a property test
would measure the framework.

The allocator is armed after setup so that test-harness startup allocations are
not counted. This is the one place `unsafe` appears, and it is in a test target,
not in the library, so `#![forbid(unsafe_code)]` on the lib still holds.

**Alternatives considered**: the `assert_no_alloc` crate (rejected: a third
dev-dependency for ~30 lines, and it needs the same global-allocator trick
underneath); inspecting assembly or `cargo-asm` (rejected: not a test, and
brittle across optimization levels); trusting that integer arithmetic does not
allocate (rejected: true today, silently false the first time someone adds a
`format!` to an error path — which is the regression the test exists to catch).

---

## R-008 — Proving the dependency direction (TEST-023)

**Decision**: the test embeds the crate manifest with `include_str!("../Cargo.toml")`
and asserts the `[dependencies]` table is empty.

**Rationale**: it needs no toml parser, no dev-dependency, and no process spawn;
`include_str!` resolves at compile time, so the test performs no I/O and cannot
be affected by the working directory. Asserting *zero* normal dependencies is
strictly stronger than asserting "no workspace crate", and it is the truth for
this crate — `proptest` and `trybuild` are dev-dependencies and do not appear in
that table.

**Cost to accept**: it is a text assertion, so it would not notice a dependency
introduced through a workspace-inherited table. That is a real gap and the right
place to close it is a `cargo tree`/`cargo deny` check in CI, which is D0.4
(→ #1). Noted in the test's own comment so the next reader does not over-trust it.

**Alternatives considered**: spawning `cargo metadata` from the test (rejected:
a subprocess that can contend on the build lock, and it makes an offline
guarantee depend on cargo's cache state); a `build.rs` check (rejected: build
scripts run before the thing they check and turn a test failure into a build
failure, which is harder to read).

---

## R-009 — Error type

**Decision**:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueError { OutOfRange, Overflow, InvalidStep }
```

with `core::fmt::Display` and `core::error::Error` implemented over `&'static
str` messages. No `String`, no `Box<dyn Error>`, no format arguments.

**Rationale**: three variants because the spec's Failure Modes name three
distinct conditions, and each needs a test that reaches it (Principle IV).
`Display` over static strings allocates nothing and is what makes an error
readable at the operator boundary; `no_std` guarantees it cannot quietly grow a
`String`. The spec's "no formatting on the error path" is a hot-path rule — the
hot path must not *call* `Display`, not that it may not exist.

**Alternatives considered**: `#[non_exhaustive]` (rejected: speculative — it
forces `_ =>` arms downstream today to buy flexibility no one has asked for);
one variant carrying context (rejected: context means data, data on an error
path means either a lifetime or an allocation); `thiserror` (rejected: a
proc-macro dependency for three unit variants).

---

## R-010 — Rounding algorithm, shared by tick rounding and the notional product

**Decision**: one internal helper over `i128`, used by `Px::round_to_tick`,
`Qty::round_to_lot`, and `Px::notional`.

```text
round_to_step(value, step > 0, dir) -> i128
    q     = value.div_euclid(step)      // floor, since step > 0
    r     = value.rem_euclid(step)      // 0 <= r < step
    if r == 0 { return value }          // idempotence (INV-005)
    lower = q * step
    upper = lower + step
    Down          -> lower
    Up            -> upper
    TowardZero    -> if value > 0 { lower } else { upper }
    AwayFromZero  -> if value > 0 { upper } else { lower }
```

**Rationale**: `div_euclid`/`rem_euclid` give floor semantics for a positive
step with no sign special-casing, which is where hand-rolled rounding usually
goes wrong on negatives. Working in `i128` and range-checking on the way back to
`i64` means `lower + step` cannot overflow for `Px`/`Qty` inputs, so the only
checked case is the final narrowing.

Verified against the spec's vectors: value `-10.004`, tick `0.01` gives
`q = -1001`, `lower = -10.01`, `upper = -10.00`, so `Down → -10.01`,
`TowardZero → -10.00`, `AwayFromZero → -10.01` — the table in the spec.

For the notional product the same helper runs at `step = SCALE`: the raw product
is in `1e-18` units, rounding to a multiple of `1e9` and dividing gives the
`1e-9` result.

**INV-004's proof, concretely**: `|px| ≤ 2^63` and `|qty| ≤ 2^63`, so the product
is at most `2^126 ≈ 8.507e37`; `upper` adds at most `1e9` to that; `i128::MAX =
2^127 - 1 ≈ 1.701e38`. The margin is a factor of two, so the product is total and
`Px::notional` returns a bare `Notional` rather than a `Result`.

**Alternatives considered**: `checked_div`/`checked_rem` with sign branches
(rejected: four sign cases, and negative-value rounding is the exact thing the
spec's vectors exist to pin); doing the work in `i64` for `Px`/`Qty` (rejected:
`lower + step` can overflow near the ceiling, adding a checked op inside a helper
that already has one).

---

## R-011 — Release-profile overflow behavior

**Decision**: leave the release profile at Rust's default — interior arithmetic
panics on overflow in debug and wraps in release, exactly as ADR #4 specifies.
Do not set `overflow-checks = true` for release in this issue.

**Rationale**: ADR #4 decided this and the issue says the overflow policy is not
open. Recording it here because the plan is where someone would otherwise "just
turn on the safe option" and silently reverse a decision — and because the spec's
Failure Modes row for release-mode wrapping is the honest statement of what that
costs.

**What would revisit it**: the live binary is a different question from the
`types` crate. Whether `bin/live` builds with `overflow-checks = true` — trading
a small, measurable cost for fail-closed arithmetic in production — belongs with
the binary and its latency budget (D4.2 → #1), not here.

---

## R-012 — What "offline" means for the gate

**Decision**: `proptest` and `trybuild` are fetched once from crates.io; after
that, the full gate runs with `--offline`. No test opens a socket, reads a
credential, or touches a venue.

**Rationale**: Principle VIII requires `cargo test --workspace` to be safe on any
machine, offline, with no credentials — that is about what the *tests do*, not
about whether a dependency was ever downloaded. Worth stating explicitly so
SC-005's "offline" is not read as "no `cargo fetch` ever ran" and quietly failed
on a clean checkout.

**Alternatives considered**: vendoring dev-dependencies into the repo (rejected:
checked-in third-party source for two test-only crates, with its own update
burden — revisit if a build-reproducibility requirement ever lands);
zero dev-dependencies with hand-rolled property and compile-fail harnesses
(rejected: reimplementing shrinking and `.stderr` comparison is more code to get
wrong than the thing being tested).

---

## Open questions

None. No `NEEDS CLARIFICATION` remains in the Technical Context.
