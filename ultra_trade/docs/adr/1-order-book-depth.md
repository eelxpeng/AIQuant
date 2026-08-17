# Order-book depth

- **Issue**: #1 (needs its own issue — see the other ADRs' "Naming" note)
- **Status**: accepted
- **Date**: 2026-08-17

## Context

Every market event this system has ever recorded carries **top of book only**:
one bid, one ask, and the size at each. That was enough to build everything up
to rung 8, and it puts a hard ceiling on how honest a backtest can be.

`FillModel::TouchDisplayed` fills a marketable order up to the size showing at
the touch and no further, because there is no further — the recording does not
say what is behind the touch. So an order for more than the displayed size
either fills partially and gives up, or, under `TouchUnlimited`, fills the lot
at the touch price as though the book were infinite there. Both are wrong in
the same direction for the same reason: **a strategy that trades size cannot be
evaluated against data that does not know what size costs.**

Depth fixes the data. It does not fix impact — see "What this still does not
buy".

## What the venue actually sends

Measured against Kraken's `book` channel at depth 10, over 30 seconds of BTC/USD:

| | |
|---|---|
| Opening snapshot | 20 levels (10 each side) |
| Updates | 683 messages |
| Level changes | 942, so **~1.4 levels per update** |
| Rate | ~32 level changes per second |

The important fact is the last row. Depth is a **delta** feed: an update
usually moves one price level, not the whole book. A design that re-sent the
book on every change would be paying for a snapshot to communicate one number.

## Decisions

### D-1 · One record per level, and an explicit end of update

A record is 80 bytes with a 56-byte payload (ADR, recorded log format). Ten
levels a side is 320 bytes of prices and quantities, so a book update cannot be
one record and the fixed size is not negotiable — record *n* lives at
`header + n × 80`, which is what makes seeking arithmetic and a torn tail
detectable from a file's length.

So a book update is a run of records:

```
BookLevel  { instrument, side, px, qty }     one per changed level
BookLevel  { instrument, side, px, qty }
BookApplied{ instrument }                     the update is complete
```

`qty` of zero removes the level, which is how the venue expresses it too.

This is the shape the crash-recovery ADR already chose for venue snapshots — "a
snapshot is one record per open order followed by a completion marker, which
the fixed-size format handles naturally". Two features using one idiom beats
two idioms.

**Cost, stated**: about eight times the log volume of a top-of-book feed —
~55 records a second against ~7. At 80 bytes that is 4.4 KB/s, or 16 MB an
hour. Acceptable, and worth checking again if anyone subscribes to depth 100.

**Why not widen the record.** It breaks every recording, both properties the
fixed size buys, and the arithmetic in `Segments`. The format is the wrong
thing to bend for one feature.

**Why not a bounded array of levels per record.** Three levels fit in 56 bytes,
so an update of four needs a continuation flag, and now there are two framing
schemes — the array and the continuation — where D-1 has one.

### D-2 · A partial book is never visible to a strategy

The engine accumulates `BookLevel` records and applies them to the book **only**
when `BookApplied` arrives. Nothing is dispatched in between.

This is the whole reason the completion marker exists. Applying levels one at a
time would let a strategy see a book that the venue never published: mid-update
the best bid can sit above the best ask, and a crossed book is a state every
consumer downstream has a right to assume it will not be handed. The risk gate
ages quotes and would refuse against it; a quoter would compute a midpoint from
it and post around a price that never existed.

The buffer is a fixed-size array sized at startup. An update with more levels
than it holds is a **refusal**, not a truncation: half a book applied is worse
than no book, and the count is reachable so a session can say it happened.

### D-3 · Top of book stays a separate event

`MarketKind::Quote` does not go away, and a depth feed does not replace it.

A venue that publishes only a top-of-book stream — or a bridge that cannot get
depth out — must keep working, and every recording made before this ADR must
keep replaying. The book derives its top from depth when it has depth, and from
`Quote` when that is all it gets, and a session may receive both.

**Consequence**: `Books` has two ways to learn its top of book. That is a real
cost and it is accepted, because the alternative is that adding depth breaks
every existing recording and every venue without a depth feed.

### D-4 · Format version 2

New record kinds mean a version-1 reader cannot read a version-2 log. Saying so
in the header is clearer than letting it fail per record on an unknown
discriminant, which is the reasoning the crash-recovery ADR used for the same
situation.

**Version-1 logs stay readable.** The reader already refuses only a version
*greater* than it knows, so v1 files continue to work unchanged — and there is
a test that says so, because "old recordings still read" is exactly the claim
that rots silently.

### D-5 · Walking the book is a fill model, and it is opt-in

`FillModel::WalkBook` consumes levels from the touch outwards until the order is
filled or the book runs out. The existing models stay, because a recording with
no depth cannot use the new one and must still be backtestable.

A model that walks a book it does not have would have to invent liquidity. It
refuses instead: with no depth beyond the touch, `WalkBook` fills what the touch
shows and stops, which is `TouchDisplayed`. The difference is only ever visible
where the data supports it.

## What this still does not buy

Stated plainly, because depth invites the belief that a backtest is now
realistic.

- **Market impact.** The recorded book is what the market showed *without* our
  order in it. Walking it assumes the levels would have sat still while we ate
  them, and they would not have.
- **Queue position.** A resting order still assumes the front of the queue.
  Depth says how much is at a price, not how much is ahead of us at it.
- **Hidden liquidity.** Displayed size is not all the size, in either
  direction.

Walking a recorded book is strictly less wrong than assuming the touch is
infinite. It is not right.

## Alternatives rejected

| Alternative | Why it lost |
|---|---|
| Widen the record beyond 80 bytes | Breaks every recording, seek-by-arithmetic, and torn-tail detection, to serve one feature. |
| Several levels packed per record | Needs a continuation flag for updates that overflow it, so two framing schemes instead of one. |
| Re-send the whole book each update | Pays a 20-level snapshot to communicate the 1.4 levels that actually changed. |
| Model depth instead of recording it (assume a shape past the touch) | Invents liquidity nobody observed and puts it in the same numbers as measured data, where nothing downstream can tell them apart. |
| Apply levels as they arrive, no completion marker | Hands strategies a crossed book that the venue never published (D-2). |
| Replace `Quote` with depth | Breaks every existing recording and every venue whose bridge cannot get depth (D-3). |

## Consequences

**What this constrains.** The event alphabet grows two kinds and the format goes
to version 2. The engine gains a buffer on the market path, bounded and sized at
startup, and a rule that nothing downstream sees a partial book. `Books` grows a
second way to learn its top.

**What it costs.** Roughly eight times the log volume on a depth feed. One more
branch on the hot path. Two fill models that agree except where depth exists,
which is a thing a reader of results has to know about.

**What it does not settle.** Market impact, queue position, and hidden
liquidity remain unmodelled, and a depth-aware backtest must not be presented as
though they were solved.
