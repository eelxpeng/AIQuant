# Core engine decisions: runtime, log, order ids, and fail-closed behaviour

- **Issue**: #1 (needs its own issue — see "Process note")
- **Status**: proposed
- **Date**: 2026-08-12

## Process note, first

`docs/adr/README.md` says a decision ADR merges **before** the implementation
that depends on it, because writing it afterwards turns a decision record into a
description. This one was written afterwards. The implementation of `event`,
`marketdata`, `risk`, `oms`, `strategy`, `engine`, `adapters/sim`, `simkit`,
`report`, and `bin/backtest` landed in a single pass at the user's explicit
request to skip the spec-first flow.

That inversion is recorded rather than hidden. Every decision below answers an
item `docs/ARCHITECTURE.md` lists as open, so each one is a live question that a
human still has to rule on. **Nothing here is ratified.** Read it as "this is
what the code currently does and why", not as "this is settled".

## Context

Phase 0's `types` crate landed under the full spec flow (#6). The rest of the
vertical slice — event log through backtest binary — needed a dozen decisions
that `docs/ARCHITECTURE.md` had deliberately deferred to "the first thing that
forces them". Building the slice forced them all at once.

What breaks if these are wrong: replay stops being reproducible (D-1, D-3),
an operator gets trapped in a position during the incident the controls were
written for (D-4, D-5), or money is counted with a silent rounding error
(D-8).

## Decisions

### D-1 · Runtime model: a single-threaded synchronous loop

One thread, one `Engine::on_inbound` call per input, no async, no spawning, no
shared state. Venue reports produced while handling an event are processed in
the same call, bounded by a cascade limit.

*Answers*: "Runtime model" (open decision, forced by the first engine loop).

### D-2 · The event alphabet splits into inputs and decisions

`Inbound` is what happens *to* the system; `Outbound` is what the system
*decided*. Replay feeds the inbound records of a session back through a fresh
engine and compares the outbound ones.

Every outbound record names `caused_by`: the sequence number of the inbound
record being processed when the decision was taken.

### D-3 · Sequence numbers order the session, and every record is versioned

The log assigns a monotonic gap-free `Seq`. It is the engine's only ordering key
— not exchange time, which ties and goes backwards across venues, and not
receive time, which is a different clock. Every record carries `FORMAT_VERSION`
from the first release.

*Answers*: "Record format versioning" (raised by ADR #4).

### D-4 · Reduce-only bypasses size checks and binds on validity checks

> Reduce-only bypasses every check about **how much risk is being taken**, and
> binds on every check about **whether the order is well-formed or would harm
> the venue connection**.

Bypassed: halted state, unreconciled position, stale or absent market data,
position limit, exposure limit, single-order notional limit.
Binding: the kill switch, unknown instrument, missing limit config, non-positive
price or quantity, the venue's minimum quantity, the order rate limit.

The kill switch binds because "kill" means *this system is wrong, stop it* — and
if the system's own position accounting is what is wrong, letting it emit more
orders makes the incident worse. The rate limit binds because an unbounded order
rate is how a venue disconnects you, which traps the operator just as thoroughly
as a refused order.

A reduce-only order is additionally **clamped** to the position actually held,
so it cannot overshoot into a position on the other side.

*Answers*: "Reduce-only semantics" (deferred to the first risk-gate spec, D2.2).
**This is the decision most in need of a human ruling.**

### D-5 · Three engine states; kill is terminal; a reconciliation halt cannot be resumed

`Running` → `Halted` → `Killed`. Halted refuses risk-increasing orders and
permits reduce-only ones. Killed refuses everything and has no transition out.

`Resume` out of a halt is refused while any instrument's accounting disagrees
with the venue's. The refusal is recorded as a transition from `Halted` to
`Halted` so that it appears in the log rather than being an absence of a record.

*Answers*: "Engine state machine" and "Resume preconditions per halt reason"
(both deferred to Phase 2). The full states × operations matrix the spec
template demands has **not** been written; what exists is the subset the code
implements, covered by tests.

### D-6 · Order ids are a dense monotonic run from a configured base

`first_order_id` is configuration, and each order takes the next number. That
makes the order store a slice index rather than a hash, and makes replay
reproduce the same ids in the same order.

*Partially answers*: "Order ID scheme and idempotency on reconnect". **The
idempotency half is not addressed** — nothing here handles reconnecting to a
venue that already knows about orders from a previous connection.

### D-7 · A failed log write halts the engine; an unreachable venue refuses the order

If the log will not take a record, the engine halts: a decision that is not in
the log is not replayable, and continuing would produce a session the log cannot
explain. The halt itself is not recorded, for the same reason.

If the venue will not take an order, no order exists and the intent is recorded
as refused with `VenueUnreachable`. The alternative — a phantom order sitting
`Pending` forever — is worse.

### D-8 · Position accounting matches lots first-in-first-out, and never divides

Average-cost accounting needs a division to take the proportional share of a
cost basis when part of a position closes. Matching against FIFO lots makes
closed-lot profit `(exit − entry) × closed_qty`, which is exact.

Three numbers are kept: `cash` (signed money in and out, fees included),
`realized` (closed-lot profit less fees), and `total(mark) = cash + position ×
mark`, with `unrealized = total − realized`. Keeping `cash` rather than deriving
it makes that identity a *check* on the lot bookkeeping rather than a
restatement of it.

Rounding directions are stated once and are uniformly conservative: the signed
value of a fill rounds **up** before being subtracted from cash (so a buy costs a
scale unit more and a sale returns one less), and closed-lot profit rounds
**down**.

### D-9 · The timer source is bound at the edge

The engine records a timer request and hands it to the feed adapter. It never
fires one. A backtest fires timers off simulated time and a live session off a
real clock, and nothing downstream can tell which.

### D-10 · The venue trait carries an `observe_market` hook

A simulated venue needs a view of the book to fill against. The engine calls
`observe_market` unconditionally after every market event; a real adapter has
its own connection and uses the default no-op. No branch anywhere asks whether
the bound venue is simulated.

## Alternatives rejected

| Alternative | Why it lost |
|---|---|
| Async engine loop (D-1) | Future completion order is not reproducible, and Principle II is non-negotiable. |
| Thread-per-stage with SPSC queues (D-1) | Same reproducibility problem, plus it buys throughput this system does not need before it has a latency budget to measure against. |
| Exchange time as the ordering key (D-3) | Ties within a venue and goes backwards across venues. A key that is not total is not a key. |
| Reduce-only bypasses the kill switch (D-4) | The kill switch's whole job is to stop a system believed to be wrong. If its position accounting is the thing that is wrong, more orders make the incident worse. |
| Reduce-only refused when it exceeds the position (D-4) | Clamping is strictly more useful and cannot overshoot, so refusing would trap the operator for no gain. |
| Average-cost position accounting (D-8) | Needs a division on every closing fill, and therefore a rounding rule applied to money at the highest-frequency point in the system. |
| Engine fires its own timers (D-9) | The engine would have to synthesise inbound records, which replay would then synthesise a second time. |
| Log a phantom order when the venue is unreachable (D-7) | It would sit `Pending` forever and appear in `live()`, so a kill switch would try to cancel an order that never existed. |

## Consequences

**What this constrains.** The engine cannot become multi-threaded without
revisiting D-1 and re-proving replay. Bar boundaries, order ids, and timer
firing are all now defined relative to the event stream, so any change to how
events are ordered is a breaking change to replay.

**What it costs.** FIFO lots cost a bounded queue per instrument and an
allocation if it outgrows its reservation (`Position::lots_would_grow` reports
when). The single-threaded loop caps throughput at one core.

**What is still open.** Three items `docs/ARCHITECTURE.md` lists remain
genuinely unanswered:

- **Event log durability.** `MemoryLog` is the only implementation. Nothing is
  persisted, so there is no crash-recovery contract and no reconnect story. The
  record format carries its version, which is the piece that had to exist first.
- **Idempotency on reconnect** (the second half of D-6).
- **Latency budget targets.** Deliberately late — a target picked before you can
  measure is a guess — but nothing here has been benchmarked, so
  Principle VI's "cite a benchmark run" has not been satisfied by this work.

**What would make us revisit.** A measured latency budget that one core cannot
meet (D-1); a venue whose fee schedule is proportional rather than per-unit
(the sim venue's `Fees`); a position large enough that its FIFO lot queue
becomes a real cost (D-8).
