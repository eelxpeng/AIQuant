# Roadmap seed (one-time)

**This file is scaffolding, not a source of truth.** Paste its contents into a
new GitHub issue titled `Master roadmap`, note the issue number, then update
`AGENTS.md` / the constitution to cite that number and **delete this file**.

Status must never live in a checked-in document — it goes stale the moment work
starts. From then on, the roadmap is the issue and its phase hubs; a
workstream's status is its issue's open/closed state.

---

## Master roadmap

Deliverables are dependency-ordered. Each is measurable: it names a condition
someone can test, not an area of work.

### Phase 0 — Foundations

Gate: nothing above this phase starts until replay is proven.

- **D0.1** `types` exposes fixed-point `Px`/`Qty` and source-typed timestamps; a property test shows no precision loss across the supported range.
- **D0.2** A typed, append-only event log with a replay cursor; a 10k-event fixture replays byte-identically.
- **D0.3** `simkit` provides a scripted feed and a deterministic simulated venue; `cargo test --workspace` passes offline with no credentials.
- **D0.4** CI runs fmt, clippy (`-D warnings`), and the workspace test suite on every PR.

### Phase 1 — Market data

- **D1.1** A feed normalizes into typed events with exchange time preserved; out-of-order and duplicate events have specified, tested behavior.
- **D1.2** Book building reproduces a recorded session's top-of-book exactly on replay.
- **D1.3** Staleness detection: a feed gap beyond its spec'd bound marks data stale and is observable downstream.

### Phase 2 — Execution and risk

Gate: no strategy work lands before the risk gate exists.

- **D2.1** Order state machine reaches a terminal state under every specified failure mode, proven by a test per mode.
- **D2.2** The risk gate is the only path from intent to venue; a test proves no bypass path compiles.
- **D2.3** Kill switch halts new orders from every system state, including mid-order and during reconnect.
- **D2.4** Reconciliation halts trading on any internal-vs-venue position divergence.

### Phase 3 — Strategy and backtest

- **D3.1** The strategy trait exposes no clock, socket, or filesystem; a compile-level test enforces it.
- **D3.2** The backtest driver runs the same engine and strategy code as live, with only adapters swapped; no `if backtest` branch exists in engine or strategy crates.
- **D3.3** A named fill model with written assumptions; its parameters are spec'd, not constants in a harness.
- **D3.4** A look-ahead guard: a test proves a strategy cannot observe data timestamped after the event being processed.

### Phase 4 — Live readiness

- **D4.1** One real venue adapter passes an opt-in, gated integration smoke.
- **D4.2** Tick-to-trade p99 measured and within its declared budget, enforced in CI.
- **D4.3** Paper mode runs unattended against a live feed for a full session with zero unhandled events.
- **D4.4** Crash-recovery: the process is killed at each specified window and resumes to a consistent position.

### Phase 5 — Production

- **D5.1** Operator runbook: start, halt, resume, and reconcile procedures, each exercised at least once.
- **D5.2** Observability: latency, fills, position, and limit utilization visible live.
- **D5.3** Live gating: an explicit, documented checklist gates the first live capital.

---

## Phase hubs

Create one hub issue per phase once its work can begin. Do not pre-stage issues
for work that cannot start yet — keep future plans as a section in this master
issue.

A hub holds only: the phase title, its deliverables, the child-workstream list,
and an explicit gate line if the phase is gated. Use plain bullets (`- #N title`),
never checkboxes — a manual tick goes stale, whereas an issue's open/closed state
cannot.

Boundaries and principles live in `AGENTS.md`, `docs/`, and the constitution —
never in a hub body. Do not hand-write narrative status anywhere.
