# ultra_trade Architecture

Design source of truth. Boundaries and ownership live here; behavior contracts
live in `specs/`; status lives in GitHub issues.

> **Status: PROPOSED.** The crate split and seams below are a starting proposal,
> not a ratified decision. Confirm or change them in the first architecture
> issue, then delete this banner. Do not build past a boundary you have not
> agreed to.

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

## Proposed crates

| Crate | Owns | Must not know about |
|---|---|---|
| `types` | ids, `Px`/`Qty` fixed-point, typed timestamps, instrument, side, errors | everything else |
| `event` | typed event records, sequencing, the durable log, replay cursor | venues, strategies |
| `marketdata` | feed normalization, book building, bar/tick aggregation | orders, positions |
| `risk` | pre-trade checks, limits, kill switch, exposure accounting | strategies, feeds |
| `oms` | order state machine, venue adapter trait, fill handling, reconciliation | strategies |
| `strategy` | the strategy trait, signal → intent logic | sockets, clocks, files |
| `engine` | the loop that wires the above — a **library**, not a binary | venue/feed specifics, clients |
| `backtest` | replay driver, fill/latency/fee models, result reporting | strategy internals |
| `simkit` | deterministic scripted feeds, simulated venue, fixtures | production adapters |
| `client` | operator UI: reads the log, appends command events | engine internals, venues, strategies |
| `adapters/*` | one crate per real venue or feed | each other |
| `bin/*` | thin binaries that bind adapters and run `engine` | domain logic of any kind |

Dependency direction is strictly downward in that table (Constitution I).

**`engine` is a library.** It takes its adapters and runs; it owns no `main`. A
thin binary under `bin/` binds a feed, a venue, and a clock and calls it. This is
what makes Constitution III structural rather than a rule people must remember:
backtest and live are two *callers* of one library, not two modes of one program,
so there is no place for an `if backtest` to live.

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

## Open decisions

Each of these should become an issue, then an ADR under `docs/adr/` or a spec
decision record before the code that depends on it lands.

- [ ] Runtime model: single-threaded event loop with pinned core, vs. async, vs. thread-per-stage with SPSC queues. Drives the latency budget shape.
- [ ] Event log durability: in-memory ring + async persist, vs. synchronous append. Drives the crash-recovery contract.
- [ ] Fixed-point representation: scaled `i64` vs. a decimal crate. Drives every arithmetic site.
- [ ] Order ID scheme and idempotency contract on reconnect.
- [ ] Position reconciliation policy: how divergence between internal and venue state halts trading and how it resumes.
- [ ] Which venue/feed adapter lands first, and what its integration smoke proves.
- [ ] Latency budget targets (p50/p99/p99.9) for the tick-to-trade path.
- [ ] **Reduce-only semantics**: exactly which gate checks a reduce-only order bypasses, and which still bind. *Forced by: the first risk-gate spec (Phase 2, D2.2).*
- [ ] **Resume preconditions per halt reason**: resuming out of a reconciliation halt must be impossible until reconciled; "explicit and logged" is not sufficient. *Forced by: the halt/resume spec (Phase 2, D2.3).*
- [ ] **Engine state machine**: which of the four commands is legal in which state, including the degenerate cells (flatten while halted, kill while flattening, resume while a flatten is in flight). *Forced by: the command-event spec (Phase 2). This is a states × operations matrix and the spec template already demands one.*
- [ ] **Dropped command channel**: does the engine keep trading or halt when it loses the client? A dead-man switch is the conservative read of fail-closed and is also annoying on a network hiccup. *Forced by: the client transport spec (Phase 2).*

## Non-goals for v1

State them here as they are decided, so agents stop proposing them:

- **Manual order entry from the client.** The control surface is four commands. Placing or cancelling arbitrary orders from a UI is a trading terminal — it needs authentication, its own risk path, and an order-ticket lifecycle, and it is a separate project.
- **An in-process UI.** Rejected: it shares an address space and scheduler with the hot path (see the crate table).
- **A client that queries engine state directly.** Rejected: it would create a second interface to keep in sync and a surface replay cannot see (seam 5).
