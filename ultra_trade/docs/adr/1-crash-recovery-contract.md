# Crash-recovery contract

- **Issue**: #1 (needs its own issue — see "Naming")
- **Status**: proposed
- **Date**: 2026-08-13

## Naming

Filed under the roadmap hub, like the other two ADRs, because D4.4 has no
issue of its own yet. Rename it when one exists. Agents do not open workstream
issues.

Written before the code it decides, which is what `docs/adr/README.md` asks.

## Context

D4.4 asks that "the process is killed at each specified window and resumes to a
consistent position". Two things now make that answerable that were not before:
the recorded-log format (ADR, recorded log format) fixed what survives a crash,
and the background writer fixed what a crash costs — a process crash loses only
what is still in the ring, a machine crash also loses what the operating system
had not flushed.

What is left is the part that decides whether a restart is safe: **what the
system believes about its position when it comes back, and what it is allowed
to do about it.**

The expensive case is narrow and specific. A fill happens, the venue records it,
and the process dies before that fill reaches the log. On restart the log says
flat and the venue says long. Every other window is a variation on knowing less
than the venue does; this is the one where acting on what we know loses money.

## Decisions

### D-1 · Recovery never modifies the damaged file

A session is a **chain of segments**. The file the crash damaged is left exactly
as the crash left it, and recovery opens a new one whose first record continues
the sequence:

```
session-42.0.log   seq 0 … 8_193      torn tail, never written to again
session-42.1.log   seq 8_194 …        recovery starts here
```

The reader already truncates a damaged tail *on read* and reports what it
dropped, so nothing on disk has to be repaired to make the session readable.

This costs no format change. A segment's first record carries its own sequence
number, and the header already carries the session id, so a chain is ordered by
`(session_id, first seq)` without a new field. `LogWriter` needs to be able to
start at a sequence other than zero; that is an addition, not a version bump.

**Why not truncate in place.** It overwrites the only evidence of what happened,
before anyone has looked at it, and a second crash *during* the truncate leaves a
file that is neither the original nor the repair.

### D-2 · A resumed session starts halted, and only an operator resumes it

Restart enters `Halted`. Reconciliation agreeing is **necessary but not
sufficient**: even when the books match, the session stays halted until a human
explicitly resumes it.

A crash is an incident. Something should look at it before the system trades
again, and Constitution V already says exiting a halt is an explicit, logged
decision and never automatic.

Reduce-only orders still pass the gate while halted, so an operator can flatten
without resuming — which is exactly the position they are most likely to want.

**The cost, stated plainly**: no unattended restart. A process that dies at
03:00 stays halted until someone resumes it. That is the intended behaviour and
it is a real operational cost, not an oversight.

### D-3 · Restart cancels everything the **venue** reports, not everything the log knows about

On restart, before anything else:

1. Ask the venue for its open orders.
2. Cancel every one of them and wait for confirmation.
3. Ask the venue for its positions.
4. Compare against what the log implies.

**Step 1 is driven by the venue's list, not by the log's.** That is the whole
point, and it is the part that is easy to get wrong. The orders most likely to
matter after a crash are exactly the ones the log does *not* know about — sent
in the moments before the process died, with their records still in the ring.
Cancelling "every order in our order store" would leave those live at the venue
with nothing managing them.

Queue position is lost. That is the trade: a known state beats a good queue
position after a crash.

**Why not adopt them.** Matching venue orders to logged ones needs a venue-side
identifier that survives a reconnect, which is the reconnect-idempotency half of
D-6 and is still unanswered. Adopting orders on a guess is worse than cancelling
them.

**Why not refuse to start.** It leaves live orders at the venue with nothing
managing them, which is the state you least want to sit in.

### D-4 · A divergence is never resolved by trusting either side

If the venue's position and the log's disagree, the session stays halted and
**resume is refused entirely** — not merely discouraged. The system does not
adopt the venue's number, and does not assume its own is right.

This is Constitution V, and it means a crash in the dangerous window
(D-5, window 3) **requires a human**. There is no automatic path out. That is
deliberate, and it is the strongest argument for keeping the ring small and the
flush frequent: the window's size is a design choice, and it is the only lever
that reduces how often a person is needed.

Recording an operator's adjustment as a command event — so replay reproduces it
— is the obvious follow-on and is **not** decided here. Today the answer is that
trading stops.

## D-5 · The kill-window matrix

`specs/README.md` requires this for anything with durable state. Every row is a
moment the process can die.

| Window | Log holds | Venue holds | On restart | Ends in |
|---|---|---|---|---|
| **1. Before the first fill** | maybe some `OrderSubmitted`; possibly not the last ones | possibly live orders, no fills | cancel by venue list, reconcile: both flat | `Halted`, books agree |
| **2. Between fills** | fills up to *N*, flushed | position reflecting fills up to *N* | cancel by venue list, reconcile: agree | `Halted`, books agree |
| **3. After a fill, before it reached the log** | fills up to *N* | position reflecting *N + 1* | cancel by venue list, reconcile: **disagree** | `Halted`, **resume refused**, human required |
| **4. During reconciliation** | whatever it held; reconciliation writes no state | unchanged | reconciliation is a comparison, so nothing is half-done — repeat it | as windows 1–3 |
| **5. During recovery itself** | previous segments, plus a partial new one | orders possibly already cancelled | cancel again — cancelling an already-cancelled order is a no-op the order machine already handles | as windows 1–3, plus another segment |
| **6. Before any record was written** | header only, or nothing | nothing | an empty log reads clean; a zero-byte file is not a log and is refused | `Halted`, books agree |

Window 3 is the only one that cannot resolve itself, and windows 4 and 5 are
safe precisely because reconciliation and cancellation are both idempotent.

Window 5 has a visible cost: a crash loop produces a segment per attempt. They
are small and they are evidence, so they accumulate rather than being cleaned up
automatically.

## D-6 · The states × operations matrix

The four commands against the states a recovering session passes through.
Degenerate cells included, because those are the ones that get missed.

| | **Halt** | **Resume** | **Kill** | **Flatten** |
|---|---|---|---|---|
| **Recovering** (cancelling, awaiting the venue) | no-op, already halted | **refused** — recovery is not finished | allowed; terminal | **refused** — the position is not known yet |
| **Halted**, books agree | no-op | **allowed** → `Running` | allowed; terminal | allowed (reduce-only) |
| **Halted**, books disagree | no-op | **refused** — D-4 | allowed; terminal | allowed (reduce-only) |
| **Killed** | no-op | refused | no-op | refused |

Two cells deserve their reasons:

- **Flatten while recovering** is refused because flattening means "drive the
  position to zero", and the system does not yet know what the position is.
  Acting on a stale number would trade in the wrong direction.
- **Kill while recovering** is allowed, and must be. The kill switch works from
  every state (Constitution V), and a restart that is going badly is exactly
  when an operator reaches for it.

## What this requires that does not exist yet

Stated so the implementation is not a surprise:

- **`VenueAdapter` must be able to be asked for a snapshot** — its open orders
  and its positions. Today a venue only pushes reports; nothing can request one.
- **Two new record kinds**, so an open-order report and the end of a snapshot are
  in the log. A list does not fit a fixed 56-byte payload, so a snapshot is one
  record per open order followed by a completion marker — which the fixed-size
  format handles naturally.
- **A format version bump to 2.** New record kinds mean a version-1 reader
  cannot read a version-2 log, and saying so in the header is clearer than
  letting it fail per record on an unknown kind. Version-1 logs stay readable.
- **`LogWriter` starting at a non-zero sequence**, for D-1's segments.
- **A `Recovering` engine state**, distinct from `Halted`, so the matrix above
  is expressible and the refusals in it are testable.

## Alternatives rejected

| Alternative | Why it lost |
|---|---|
| Truncate the damaged file and continue (D-1) | Overwrites the only evidence before anyone looks at it, and a crash during the repair leaves a file that is neither. |
| Resume automatically once the books agree (D-2) | A crash-looping process would trade its way through every restart unattended, and Constitution V says leaving a halt is never automatic. |
| Cancel the orders the log knows about (D-3) | The orders that matter most are the ones the log does *not* know about — sent just before the crash, records lost with the ring. |
| Adopt the venue's open orders (D-3) | Needs a venue-side id that survives reconnect, which D-6 of the log-format ADR leaves unanswered. Adopting on a guess is worse than cancelling. |
| Refuse to start while orders are open (D-3) | Leaves live orders at the venue with nothing managing them. |
| Adopt the venue's position on divergence (D-4) | Constitution V: a divergence never resolves itself by trusting one side. |
| Reconstruct position from the venue alone and ignore the log | Throws away the only record of *why* the position is what it is, and makes the log stop being the audit trail. |

## Consequences

**What this constrains.** A restart is now a defined sequence with a defined
end state, and every path through it ends halted. Nothing may trade on a
recovered session without a human. The venue adapter grows a request side, and
the event alphabet grows two kinds and a version.

**What it costs.** No unattended restart (D-2). Lost queue position on every
restart (D-3). A human on every crash that lands in window 3 (D-4). Segments
accumulate, one per recovery attempt (D-1).

**What it does not settle.** How an operator resolves a divergence — an
adjustment command that replay reproduces is the obvious shape and is not
decided here. Nor is the reconnect-idempotency question that made D-3 choose
cancelling over adopting.

**What would make us revisit.** A venue-side order id that survives reconnect,
which would make adopting orders possible and make D-3 worth re-arguing. Or an
operational need for unattended restart, which would make D-2 worth
re-arguing — with the crash-loop failure mode priced in.
