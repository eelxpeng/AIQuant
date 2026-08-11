# ultra_trade Constitution

The durable rules for the real-time quantitative trading system. This document
supersedes habit, convenience, and any sibling project's practice. Where this
document and `docs/` disagree on design, `docs/` is updated to match or this
document is amended explicitly.

Every principle below exists because getting it wrong loses money, silently.

## Core Principles

### I. Layered Dependency Direction

Dependencies point one way: `types` → `event` → `marketdata` / `risk` / `oms` →
`strategy` → `engine`. A lower layer MUST NOT know about a higher one. Strategy
code MUST NOT reach a venue, a socket, a clock, or a file directly — it receives
events and emits intents.

A crate that needs something from above is a design error, not a case for a
back-reference. Fix the boundary or move the type down into `types`.

### II. Deterministic Replay (NON-NEGOTIABLE)

The same input event sequence MUST produce byte-identical output: the same
orders, in the same order, with the same reasons. Every source of nondeterminism
is injected, never ambient — clocks, random number generators, iteration order,
thread scheduling, and IDs.

`SystemTime::now()`, `Instant::now()`, `rand::thread_rng()`, and `HashMap`
iteration order are forbidden on any path that influences an order. A replay
test that reproduces a recorded session is the acceptance evidence.

Without this, no bug is reproducible and no backtest is trustworthy.

### III. Backtest/Live Parity (NON-NEGOTIABLE)

There is ONE strategy and engine code path. Backtest, simulation, paper, and
live differ ONLY in which adapters are bound at the edge — the feed source and
the venue sink. A strategy MUST NOT be able to observe which mode it is running
in.

Consequences that bind every PR:

- No `if backtest { ... }` branch in engine or strategy code, ever.
- Any information a strategy reads MUST have been available at that event's
  timestamp in live trading. Look-ahead is a correctness bug, not a modelling
  choice.
- A fill model, latency model, or fee model used in backtest MUST be a named,
  spec'd component with its assumptions written down — not a constant buried in
  a test harness.

This is the single most expensive class of bug in this domain: a strategy that
is profitable only because the backtest was lying.

### IV. Spec-First, Test-First (NON-NEGOTIABLE)

Every behavior has a normative spec before implementation, and implementation
starts from failing contract tests. Specs include invariants, event examples,
failure modes, replay behavior, and required tests. Tests are written → they
fail → then implementation follows.

A behavior without a spec and a failing test does not get built.

Every fail-closed branch and every new error arm MUST have at least one test
that actually reaches it. An error string with zero test hits is an unexploded
invariant, not a defense.

### V. Fail-Closed Risk (NON-NEGOTIABLE)

Every order reaches the venue through the risk gate. There is no bypass, no
"internal" path, no test-only shortcut that also exists in the shipped binary.

- On any ambiguity — unknown instrument, stale market data, unreconciled
  position, unreachable limit config — the system rejects the order. Never
  "assume flat", never "assume the last known value".
- The kill switch MUST work from every state, including mid-order, during
  reconnect, and while a limit config is reloading. It is proven by a test per
  state, not by inspection.
- Position and exposure limits are checked against the system's own accounting,
  and that accounting is reconciled against the venue. A divergence halts
  trading; it never resolves itself by trusting one side.

### VI. Latency Budgets Are Contracts

Every hot-path component declares a latency budget in its spec, expressed as a
percentile (p50/p99/p99.9), not an average. A PR that changes a hot-path
component cites a benchmark run against that budget.

On the hot path there is no allocation, no lock, no syscall, no formatting
logger, and no unbounded loop. Backpressure behavior is specified — what is
dropped, queued, or blocked when a consumer falls behind — never emergent.

A regression past a declared budget blocks merge exactly like a failing test.

### VII. Exact Numerics And Explicit Clocks

Money and quantity are fixed-point types. Floating point MUST NOT appear in an
order, position, or accounting path. Price comparisons are exact; a tolerance
requires a spec justification.

Every value crossing a venue boundary is explicitly rounded to that venue's tick
or lot size, with the direction stated at the call site.

Timestamps are typed by source — exchange time, receive time, local monotonic —
and MUST NOT be subtracted or compared across kinds. Clock skew is a modelled
condition, not an assumption.

### VIII. Simulated Venues From Day 1

A deterministic simulated venue and scripted market-data feed exist before any
real adapter. Unit and scenario tests use them exclusively: no network, no
filesystem, no real venue, no wall clock.

Real adapters are proven by opt-in, explicitly gated integration smokes whose
evidence is cited in the PR. A default `cargo test --workspace` MUST be safe to
run on any machine, offline, with no credentials present.

### IX. Plain-Language Communication

Specs, docs, commits, PR bodies, and code comments lead with the point in short
plain words, and define a hard term once. Code comments say *why* in one or two
lines. A term used in a spec MUST appear in `CONTEXT.md` with its `_Avoid_:`
line.

## Quality Gates That Block Merge

- Owning spec exists with invariants, event examples, failure modes, and
  required tests (Principle IV).
- Contract tests were written and observed failing before implementation began.
- Replay determinism test passes for any change on the engine path (Principle
  II).
- Backtest/live parity holds: no mode branch in engine or strategy code, and no
  new look-ahead surface (Principle III).
- Every new order path is covered by a risk-gate test, and every new fail-closed
  branch has a test that reaches it (Principles IV, V).
- Hot-path changes cite a benchmark against the declared latency budget, or the
  spec records why the budget is not applicable (Principle VI).
- No float, no ambient clock, no ambient RNG introduced on an order path
  (Principles II, VII).
- No real network, credentials, venue, or wall clock in default tests
  (Principle VIII).
- A test whose result depends on timing, concurrency, or a race cites a clean
  pass streak under load, not a single green run.
- PR body carries a "Design deltas" section versus the intake issue ("none" is
  acceptable and must be stated).
- Deferrals link a tracking issue on the same line. A `TODO` without an issue
  number is rejected.

## Governance

Roadmap, phases, and execution status are owned by GitHub issues in
`eelxpeng/AIQuant` (master roadmap #1 and its phase hubs), never by a
checked-in status document.
Design source of truth is `docs/`. Behavior contracts are the specs under
`specs/`.

Amendments require a written rationale, a version bump, and a propagation pass
across the Spec Kit templates and any affected specs. MAJOR for a
backward-incompatible principle removal or redefinition, MINOR for a new
principle or materially expanded section, PATCH for clarifications.

All plans and implementations are reviewed against these principles. Complexity
that violates a principle MUST be justified in writing or removed.

**Version**: 1.0.0 | **Ratified**: 2026-08-10 | **Last Amended**: 2026-08-10
