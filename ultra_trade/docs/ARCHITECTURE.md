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
```

The dashed edges (feed adapter, venue adapter) are the ONLY things that differ
between backtest, paper, and live (Constitution III).

## Proposed crates

| Crate | Owns | Must not know about |
|---|---|---|
| `types` | ids, `Px`/`Qty` fixed-point, typed timestamps, instrument, side, errors | everything else |
| `event` | typed event records, sequencing, the durable log, replay cursor | venues, strategies |
| `marketdata` | feed normalization, book building, bar/tick aggregation | orders, positions |
| `risk` | pre-trade checks, limits, kill switch, exposure accounting | strategies, feeds |
| `oms` | order state machine, venue adapter trait, fill handling, reconciliation | strategies |
| `strategy` | the strategy trait, signal → intent logic | sockets, clocks, files |
| `engine` | the loop that wires the above; the only place with a `main` shape | venue/feed specifics |
| `backtest` | replay driver, fill/latency/fee models, result reporting | strategy internals |
| `simkit` | deterministic scripted feeds, simulated venue, fixtures | production adapters |
| `adapters/*` | one crate per real venue or feed | each other |

Dependency direction is strictly downward in that table (Constitution I).

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

## Non-goals for v1

State them here as they are decided, so agents stop proposing them:

- (fill in — e.g. multi-venue smart order routing, options greeks, a GUI)
