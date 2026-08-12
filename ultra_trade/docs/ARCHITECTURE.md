# ultra_trade Architecture

Design source of truth. Boundaries and ownership live here; behavior contracts
live in `specs/`; status lives in GitHub issues.

**Status: ratified** for v1 (#2, 2026-08-11). The crate table and the six seams
below are decisions, not proposals. Changing one is an architecture change: open
an issue, record the reason, and land it here before the code that depends on it.

## The one-sentence shape

Market data events flow in through a feed adapter, are normalized into a typed
event stream, drive strategy state, produce order intents, pass a risk gate, and
leave through a venue adapter — and the entire path is replayable from the event
log alone.

```
feed adapter ─┐                                    ┌─ venue adapter
              ▼                                    ▲
        [ marketdata ] → [ engine ] → [ risk ] → [ oms ]
                            ▲   │
                            │   ▼
                        [ strategy ]
                            │
                        [ event log ]  ← the replay boundary
                          ▲      │
              commands    │      │   observations
                          │      ▼
                     [ client ]  ← separate process; never touches engine state
```

The dashed edges (feed adapter, venue adapter) are the ONLY things that differ
between backtest, paper, and live (Constitution III).

The `client` reaches the engine only by appending a command event to the log, and
learns what happened only by reading the log. Both directions go through the same
boundary, so the UI can neither observe nor influence anything replay cannot see.

## Crates

| Crate | Owns | Must not know about |
|---|---|---|
| `types` | ids, `Px`/`Qty` fixed-point, typed timestamps, instrument, side, errors | everything else |
| `event` | typed event records, sequencing, the durable log, replay cursor | venues, strategies |
| `marketdata` | feed normalization, book building, aggregation machinery | orders, positions |
| `risk` | pre-trade checks, limits, kill switch, exposure accounting | strategies, feeds |
| `oms` | order state machine, venue adapter trait, fill handling, reconciliation | strategies |
| `strategy` | the strategy trait, signal → intent logic | clocks, sockets, files |
| `engine` | the loop that wires the above — a **library**, not a binary | venue/feed specifics, clients |
| `simkit` | scripted feeds, fixtures, test scaffolding **only** | production adapters |
| `client` | operator UI: reads the log, appends command events | engine internals, venues, strategies |
| `report` | log consumer → metrics (PnL, drawdown) for finished sessions | engine internals |
| `adapters/sim` | simulated venue + fill model — used by backtest AND tests | feeds, strategies |
| `adapters/historical` | recorded market data replayed as a feed adapter | orders, strategies |
| `adapters/<venue>` | one crate per real venue or feed | each other |
| `bin/*` | thin binaries binding adapters: live, paper, backtest | domain logic of any kind |

Dependency direction is strictly downward in that table (Constitution I).

**`engine` is a library.** It takes its adapters and runs; it owns no `main`. A
thin binary under `bin/` binds a feed, a venue, and a clock and calls it. This is
what makes Constitution III structural rather than a rule people must remember:
backtest and live are two *callers* of one library, not two modes of one program,
so there is no place for an `if backtest` to live.

**There is no `backtest` crate.** It decomposed into parts that already had
homes: the replay driver is a feed adapter (`adapters/historical`), the simulated
venue is a venue adapter (`adapters/sim`), reporting is a log consumer
(`report`), and the residue is `bin/backtest`. A `backtest` crate would be a
*place where backtest-only logic can accumulate* — it arrives as one convenience
helper, then a shortcut that is "fine because it's only backtest", and Principle
III rots inside a crate whose name makes the rot look legitimate. With no such
crate there is nowhere for it to go.

**`adapters/sim` is an adapter, not test scaffolding.** Every backtest runs
through it, so it carries a real workload and belongs beside the venue adapters
it mirrors. `simkit` is what remains: scripted feeds and fixtures you would never
trade on. The line is *an adapter is something you would run a real backtest
through; simkit is scaffolding.*

> **Known hazard.** Sharing `adapters/sim` between backtest and unit tests means
> someone tweaking it to make a test pass can silently move every backtest
> result. The mitigation is that the fill model is spec'd with its own tests and
> written-down assumptions (already required by the spec template). If that
> discipline slips, this is where it costs you.

**`report` stays separate from `client` for now.** Both are log consumers and
they may converge; premature split is cheap to fix, premature merge is not.

**`client` is a separate process**, not a crate linked into the trading binary. A
UI is made of allocation, locking, and rendering stalls — all of which the hot
path forbids. Out-of-process, a UI bug cannot reach the engine no matter how bad
it is. It also means the same client binary drives a live session and a recorded
one, because both are just a log.

## Load-bearing seams

These are the boundaries that must stay clean, because crossing them is how the
expensive bugs get in.

**1. The replay boundary (`event`).**
Everything that influences an order is an event in the log. If a decision
depended on something not in the log, replay is a lie. This is the boundary that
makes Constitution II testable.

**2. The mode boundary (adapter binding).**
Backtest / paper / live are chosen by which feed and venue adapters `engine`
binds at startup. Nothing downstream of that binding knows the mode. Any
`if backtest` below this line is a Constitution III violation.

**3. The risk gate (`risk`).**
Exactly one path from intent to venue, and it goes through the gate. There is no
second path — not for tests, not for manual override, not for "just this
strategy" (Constitution V).

**4. The clock seam (`types`).**
Time is injected. `engine` holds the clock and hands it down; nothing calls
ambient time (Constitution II, VII).

**`strategy` gets no clock at all.** The requirement splits in two, and neither
half needs one:

- *"What time is it now?"* → the strategy reads the **timestamp of the event it
  is currently processing**. That is not a clock, it is a property of the event.
  It is deterministic by construction, and it makes **look-ahead structurally
  impossible**: a strategy cannot observe a time later than the event it is
  handling. The most expensive bug class in this domain is eliminated by the
  shape of the interface rather than by review.
- *"Wake me later, even if nothing happens"* → the strategy requests a **timer
  event**. The request is an event and the firing is an event, so both replay.

That also keeps the mode difference where it belongs: timers fire off the real
clock in live and off simulated time in backtest, but that difference lives in
the event source bound at the edge (seam 2). Nothing downstream knows. A
heartbeat is the degenerate case of this mechanism.

So the strategy interface is narrower than it first looks: it sees events,
current-event-time, and subscribed aggregates — and reaches nothing.

**5. The command path (`client` → `event`).**
An operator action reaches the engine only as a **command event appended to the
log**. The client never calls into engine state, and the engine never reads the
client.

This is forced by Constitution II, not by taste. An operator halt at 10:32 is an
*input* to the session: if it is not in the log, replaying the same market data
produces a different result and the log no longer explains what happened. Making
commands events buys three things at once — replay reproduces operator actions,
the audit trail ("who halted, when, against what book") exists with no separate
logging path, and the client keeps a single one-directional write.

The v1 control surface is exactly four commands: **halt**, **resume**, **kill
switch**, **flatten**. Anything beyond these is a manual trading terminal, which
is a different product with a different risk profile.

Two constraints on that surface:

- **The kill switch MUST have a path that does not depend on the client being
  alive.** `CONTEXT.md` requires it to work from every state; a wedged UI, a dead
  socket, or a sleeping laptop is exactly the state you need it in. The client
  may *also* trigger it, but must never be the only way.
- **Flatten is the only command that creates orders**, so it is the only one that
  meets the risk gate — and there the fail-closed rule inverts. A gate that
  rejects a flatten because market data is stale traps the operator in the
  position they explicitly asked to exit, during the incident the rule was
  written for. Flatten therefore emits **reduce-only** orders, which the gate
  must treat differently from risk-increasing ones. The semantics are deferred
  below; the boundary is not.

**6. The aggregation boundary (`marketdata` → `strategy`).**
`marketdata` owns the aggregation **machinery** — the windowing, and above all
the rule for when a bar is complete. Bar *definitions* (1-minute, volume, dollar,
tick-imbalance) are **configuration**. A strategy subscribes to the aggregates it
wants; it never implements aggregation.

The split exists because a trade print is a *fact* while a bar is a *convention*.
Strategies legitimately disagree about the convention, so it must be
configurable — but bar construction carries one specific recurring bug, **using a
bar's close before the bar is complete**. That is look-ahead, it is silently
profitable in backtest, and it must not be re-implementable per strategy. One
tested implementation of "when is this bar done", unlimited configurations.

The invariant this seam exists to protect, to be pinned by the `marketdata`
spec: **an incomplete bar is never visible as a complete one.**

This also fits the research boundary: a research result saying "use 5-minute
dollar bars" crosses as configuration, which is what the root `README.md`
already requires of models.

## Open decisions

Each of these should become an issue, then an ADR under `docs/adr/` or a spec
decision record before the code that depends on it lands.

- [x] ~~**Fixed-point representation**~~ — decided: `Px`/`Qty` are `i64` @ `1e-9`, `Notional` is `i128` @ `1e-9`, as three distinct newtypes. See [`adr/4-fixed-point-representation.md`](adr/4-fixed-point-representation.md) (#4). Phase 0 is unblocked.
- [ ] **Runtime model**: single-threaded event loop with pinned core, vs. async, vs. thread-per-stage with SPSC queues. *Forced by: the first `engine` loop (D0.2 / Phase 1).*
- [ ] **Event log durability**: in-memory ring + async persist, vs. synchronous append. Drives the crash-recovery contract. *Forced by: the event-log spec (D0.2).*
- [ ] **Record format versioning**: every event log record must carry a format version from the first release, so a representation change is a supported migration rather than a break. *Forced by: the event-log spec (D0.2) — retrofitting after logs exist is the expensive order. Raised by ADR #4.*
- [ ] **Order ID scheme and idempotency on reconnect**. *Forced by: the OMS spec (Phase 2, D2.1).*
- [ ] **Position reconciliation policy**: how internal-vs-venue divergence halts trading, and how it resumes. *Forced by: the reconciliation spec (Phase 2, D2.4).*
- [ ] **Which venue/feed adapter lands first**, and what its integration smoke proves. *Forced by: Phase 4, D4.1.*
- [ ] **Latency budget targets** (p50/p99/p99.9) for tick-to-trade. *Forced by: Phase 4, D4.2 — deliberately late; a target picked before you can measure is a guess.*
- [ ] **Reduce-only semantics**: exactly which gate checks a reduce-only order bypasses, and which still bind. *Forced by: the first risk-gate spec (Phase 2, D2.2).*
- [ ] **Resume preconditions per halt reason**: resuming out of a reconciliation halt must be impossible until reconciled; "explicit and logged" is not sufficient. *Forced by: the halt/resume spec (Phase 2, D2.3).*
- [ ] **Engine state machine**: which of the four commands is legal in which state, including the degenerate cells (flatten while halted, kill while flattening, resume while a flatten is in flight). *Forced by: the command-event spec (Phase 2). This is a states × operations matrix and the spec template already demands one.*
- [ ] **Dropped command channel**: does the engine keep trading or halt when it loses the client? A dead-man switch is the conservative read of fail-closed and is also annoying on a network hiccup. *Forced by: the client transport spec (Phase 2).*
- [ ] **Timer granularity and cost**: how fine timer events may be before log volume becomes the constraint. *Forced by: the first timer-using strategy.*
- [ ] **Reduce-only semantics**: exactly which gate checks a reduce-only order bypasses, and which still bind. *Forced by: the first risk-gate spec (Phase 2, D2.2).*
- [ ] **Resume preconditions per halt reason**: resuming out of a reconciliation halt must be impossible until reconciled; "explicit and logged" is not sufficient. *Forced by: the halt/resume spec (Phase 2, D2.3).*
- [ ] **Engine state machine**: which of the four commands is legal in which state, including the degenerate cells (flatten while halted, kill while flattening, resume while a flatten is in flight). *Forced by: the command-event spec (Phase 2). This is a states × operations matrix and the spec template already demands one.*
- [ ] **Dropped command channel**: does the engine keep trading or halt when it loses the client? A dead-man switch is the conservative read of fail-closed and is also annoying on a network hiccup. *Forced by: the client transport spec (Phase 2).*

## Non-goals for v1

State them here as they are decided, so agents stop proposing them:

- **Manual order entry from the client.** The control surface is four commands. Placing or cancelling arbitrary orders from a UI is a trading terminal — it needs authentication, its own risk path, and an order-ticket lifecycle, and it is a separate project.
- **An in-process UI.** Rejected: it shares an address space and scheduler with the hot path (see the crate table).
- **A client that queries engine state directly.** Rejected: it would create a second interface to keep in sync and a surface replay cannot see (seam 5).
- **Parameter sweeps.** Not that sweeps are unwanted — a sweep-optimized backtest path would need to skip the event log to go fast enough, and that is a direct Principle III violation. Excluding them in v1 keeps one path. **If sweeps become necessary, that is an architecture change with a real cost, not an optimization**: it must be raised as an issue against this section, not slipped in as a fast path.
- **A `backtest` crate.** Rejected: nowhere for backtest-only logic to accumulate (see the crate table).
- **A clock in `strategy`.** Rejected: current-event-time plus timer events cover the requirement and make look-ahead structurally impossible (seam 4).
- **Per-strategy bar aggregation.** Rejected: it makes the incomplete-bar look-ahead bug re-implementable per strategy (seam 6).
