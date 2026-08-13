# Quickstart: validating the `types` crate

**Feature**: `specs/006-value-types` · **Issue**: #6 · **Date**: 2026-08-12

How to run the contract and read the result. This is a validation guide — the
implementation belongs in `tasks.md` and the PRs.

## Prerequisites

- The pinned toolchain installs itself from `rust-toolchain.toml` on first
  `cargo` invocation (Rust 1.93.0, edition 2024, with `rustfmt` and `clippy`).
- One network fetch for the two dev-dependencies:

  ```sh
  cargo fetch
  ```

  After this, everything below runs with `--offline`. No test opens a socket,
  reads a credential, or contacts a venue (Principle VIII).

## The gate

Run from the repository root (`ultra_trade/`):

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

Focused runs while working:

```sh
cargo test -p types                       # everything
cargo test -p types --test compile_fail   # the nine non-compilation cases
cargo test -p types --test no_alloc       # the counting-allocator target
```

Paste the exact commands and their output into the PR body (SC-001, SC-005).

## Reading a RED run

Slice 1 lands the workspace, the type stubs, and the whole test suite. The PR
must show **which** tests failed and **why**, because the halves fail for
different reasons and only some of them are meaningful. The observed run is in
[red-evidence.md](red-evidence.md): **46 failed, 7 passed** of 53 tests.

Seven passing at that stage is expected, not a gap. Each asserts a
*declaration* — the manifest's empty `[dependencies]`, the pinned struct widths,
`Timestamp::from_nanos` (which a `const MIN` forces to be real) — and
declarations are exactly what Phase 1 delivers.

| Test group | Expected RED reason |
|---|---|
| TEST-001…005, 011…016, 021…023 | panic from the unimplemented stub — the behavior does not exist yet |
| TEST-006…010, 017…020 (`trybuild`) | already fail to build. That is the *right* outcome, and it is why each `.stderr` is checked in: the case must fail with the intended type error, not with "no method named `round_to_tick`" |

A compile-fail case that passes because the method does not exist yet is not
evidence. Its positive control (see
[contracts/illegal-operations.md](contracts/illegal-operations.md)) is what
turns it into evidence, and those controls only go green in slices 2 and 3.

## Regenerating `.stderr` after a toolchain bump

```sh
TRYBUILD=overwrite cargo test -p types --test compile_fail
```

Review the diff — a changed error message is fine, a changed error *kind* means
a guard moved.

## What "done" looks like

Against the spec's Success Criteria:

| Criterion | How to see it |
|---|---|
| SC-001 | `cargo test -p types` green, with the RED output from slice 1 in the PR body |
| SC-002 | Nine `trybuild` cases pass; deleting any one guard from the implementation makes exactly that case fail. Verified by mutation: adding `impl Mul<Qty> for Px` fails only `px_times_qty_operator`, and adding a cross-clock `From` impl fails only `monotonic_converted_to_exchange` |
| SC-003 | Property tests run ≥1,000 cases each, and the boundary values (`MIN`, `MAX`, `0`, `±1` scale unit) are asserted directly, not left to sampling |
| SC-004 | `--test no_alloc` reports zero allocations; no `f32`/`f64` in the public surface |
| SC-005 | The three gate commands pass with `--offline` |
| SC-006 | Every path that can leave the range returns `Result`; the PR states which operations allocate (expected: none) |

## Sanity check by hand

The vectors in the spec's Event Examples are the fixtures. The two worth eyeballing:

```rust
// The product that does not fit the scale: 10.0001 × 0.000001 = 0.0000100001
let px  = Px::from_scaled(10_000_100_000);
let qty = Qty::from_scaled(1_000);
assert_eq!(px.notional(qty, RoundDir::TowardZero).to_scaled(), 10_000);
assert_eq!(px.notional(qty, RoundDir::AwayFromZero).to_scaled(), 10_001);

// The subtraction that must not compile — the two instants are 554_511 ns apart
// and the difference is meaningless.
let exchange = ExchangeTime::from_nanos(1_786_542_330_123_456_789);
let receive  = ReceiveTime::from_nanos(1_786_542_330_124_011_300);
// let _ = exchange - receive;   // <- TEST-017: this must not build
```

## Structure

```text
ultra_trade/
├── Cargo.toml              # workspace
├── rust-toolchain.toml     # pinned toolchain (R-001)
└── crates/types/
    ├── Cargo.toml          # [dependencies] is empty — asserted by TEST-023
    ├── src/
    │   ├── lib.rs          # #![no_std], #![forbid(unsafe_code)]
    │   ├── error.rs        # ValueError
    │   ├── rounding.rs     # RoundDir + the shared round_to_step helper
    │   ├── money.rs        # Px, Qty, Notional
    │   └── time.rs         # ClockKind, Timestamp<K>, Span<K>
    └── tests/
        ├── roundtrip.rs        # TEST-001, 002
        ├── notional.rs         # TEST-003, 004, 005
        ├── boundaries.rs       # TEST-011, 012, 013, 014
        ├── time.rs             # TEST-015, 016
        ├── no_alloc.rs         # TEST-021 (own global allocator)
        ├── structure.rs        # TEST-022, 023
        ├── compile_fail.rs     # trybuild harness
        └── compile-fail/       # the nine cases + .stderr
```
