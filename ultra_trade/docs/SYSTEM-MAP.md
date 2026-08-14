# How the system fits together

Orientation for someone reading this codebase for the first time. It says **what
exists and how the pieces meet**; `ARCHITECTURE.md` says **why the boundaries
are where they are**, and the constitution says what may never change.

Everything below describes code that is on `main` and runs.

---

## 1. Three programs, one engine

There is one trading engine. What makes a run a backtest, a paper session, or a
live session is **which two things get plugged into it at startup** — a feed and
a venue. Nothing below that binding can tell the difference, which is the whole
point (Constitution III).

```mermaid
graph LR
    subgraph binaries["the three binaries"]
        R["bin/record"]
        B["bin/backtest"]
        P["bin/paper"]
    end
    R -->|synthetic feed + simulated venue| E["engine<br/>(one library)"]
    B -->|recorded feed + simulated venue| E
    P -->|<b>live</b> feed + simulated venue| E
    E -.->|swap one binding| L["bin/live<br/><i>not built yet</i>"]
```

| Program | Feed | Venue | What it is for |
|---|---|---|---|
| `record` | synthetic | simulated | produce a recording to work with |
| `backtest` | a recording | simulated | run a strategy over past market data |
| `paper` | **live** | simulated | trade a real market with no money at risk |
| *`live`* | live | **real** | not built — it is `paper` with the venue swapped |

A fourth binary, `journal`, binds nothing: it reads a recording back and says
what happened (§6).

All three take the same **session config** — what to trade, under what limits,
with which strategies (`examples/kraken.conf`):

```bash
cargo run -p record   -- examples/kraken.conf session.log 2000
cargo run -p backtest -- session.log examples/kraken.conf
cargo run -p paper    -- examples/kraken.conf /tmp/md live.log
cargo run -p journal  -- session.log --fills
```

Give `backtest` the config a recording was made under and it reproduces the
session; give it a different one to ask what another policy would have done over
the same market.

---

## 2. Where market data actually comes from

**The Rust program never opens a network socket.** Not in any binary. This is
the question people ask first, so here is the whole path:

```mermaid
graph LR
    K["Kraken<br/>public REST API"] -->|HTTPS| BR["tools/kraken-bridge.py<br/><i>outside the workspace</i>"]
    BR -->|"one text line per event"| F["FIFO or file"]
    F --> LF["crates/adapters/live<br/>LiveFeed"]
    LF -->|"Inbound events"| EN["engine"]
    STDIN["operator typing<br/>halt / resume / kill / flatten"] --> LF
```

The bridge speaks HTTPS and JSON; the trading system speaks one line per event:

```text
Q <symbol> <exchange_nanos> <bid_px> <bid_qty> <ask_px> <ask_qty>
T <symbol> <exchange_nanos> <px> <qty> <B|S>
```

**Why the seam is there.** Every venue's wire format is different, so a
venue-specific half is unavoidable. Putting it outside the workspace means a TLS
and websocket stack never enters a real-money binary as a side effect of wanting
market data — the trading system still has **zero third-party dependencies**. A
bridge for a different venue is a new script, not a new crate.

**What it costs, measured.** Polling REST rather than reading a stream puts the
data path about **6 seconds behind the market** (median, with a 3-second poll;
max 37s observed). Fine for paper. Disqualifying for any latency work, which is
worth knowing before anyone tries to measure a latency budget.

---

## 3. What happens to a single market event

This is the hot path — market data in, order out. It runs on one thread, and it
does not allocate, lock, or make a syscall.

```mermaid
sequenceDiagram
    participant Feed as feed adapter
    participant Eng as engine
    participant Log as event log
    participant MD as marketdata
    participant St as strategy
    participant Rk as risk gate
    participant Ven as venue adapter

    Feed->>Eng: Inbound::Market (a quote)
    Eng->>Log: append — recorded BEFORE anything reads it
    Eng->>MD: update the book
    MD-->>Eng: accepted, or refused as out of order
    Eng->>St: here is the quote, and the time of this event
    St-->>Eng: Intent (I want to buy 1)
    Eng->>Rk: check(intent, position, book, limits)
    alt approved
        Rk-->>Eng: Approved (rounded to tick and lot)
        Eng->>Ven: submit
        Eng->>Log: OrderSubmitted, caused_by this record
    else refused
        Rk-->>Eng: Rejected(reason)
        Eng->>Log: IntentRejected, caused_by this record
        Eng->>St: IntentRefused — so it knows nothing is working
    end
    Ven-->>Eng: Accepted / Filled (as new Inbound events)
```

Four things in that picture are load-bearing:

1. **The input is logged before anything reads it.** If the log refuses the
   write, the engine halts — a decision that is not in the log is not
   replayable.
2. **Every decision names the record that caused it.** "Why does this order
   exist" is a lookup, not an investigation.
3. **There is exactly one arrow into the venue, and it goes through the gate.**
   No second path exists; a compile-level test proves the venue cannot be
   reached from outside the engine.
4. **The venue answers by producing new inputs**, not by returning a value. That
   is what puts fills in the log and therefore into replay.

---

## 4. The crates, and who depends on whom

Read from the manifests, not from memory. Dependencies point strictly downward.

```mermaid
graph TD
    types["<b>types</b><br/>Px, Qty, clocks, ids<br/><i>no dependencies at all</i>"]
    event["<b>event</b><br/>the alphabet, the log,<br/>the on-disk format"]
    md["<b>marketdata</b><br/>book + bars"]
    oms["<b>oms</b><br/>orders, positions,<br/>venue trait"]
    risk["<b>risk</b><br/>the one gate"]
    strat["<b>strategy</b><br/>the trait"]
    eng["<b>engine</b><br/>the loop"]

    types --> event
    event --> md
    event --> oms
    event --> risk
    md --> strat
    oms --> strat
    md --> eng
    risk --> eng
    oms --> eng
    strat --> eng

    eng --> hist["adapters/historical"]
    eng --> live["adapters/live"]
    oms --> sim["adapters/sim"]
    oms --> rep["report"]

    risk --> cfg["<b>config</b><br/>the session file"]
    md --> cfg
    cfg -.->|"read at startup only"| BINS["the three binaries"]
```

`config` points *into* the binaries, not into the engine. The engine is handed
instruments and limits as values; it has never read a file and cannot tell that
one exists. That is what keeps a test able to build a session in three lines.

| Crate | Owns | Notably does **not** know about |
|---|---|---|
| `types` | `Px`, `Qty`, `Notional`, typed clocks, ids, `Instrument` | everything — it has zero dependencies |
| `event` | the event alphabet, the log, the 80-byte record format, the background writer | venues, strategies |
| `marketdata` | top of book, bar aggregation | orders, positions |
| `risk` | limits, the kill switch, the single gate | strategies, feeds |
| `oms` | order state machine, FIFO position accounting, the venue trait | strategies |
| `strategy` | the trait, and two strategies that use opposite halves of the order lifecycle | clocks, sockets, files |
| `engine` | the loop that wires it all — a **library**, no `main` | which venue or feed is bound |
| `adapters/sim` | simulated venue and fill model | feeds |
| `adapters/historical` | a recording, replayed as a feed | orders |
| `adapters/live` | live feed, line protocol, **the one real clock** | venues, orders |
| `report` | log → PnL, drawdown, refusals | engine internals |
| `config` | the session file — instruments, limits, strategies | the engine, the log, any I/O beyond reading one file |
| `recovery` | bringing a crashed session back, and refusing to when it does not add up | feeds, clocks, which venue is bound |
| `simkit` | scripted feeds and fixtures — test scaffolding only | production adapters |

**22,800 lines, 425 tests, zero third-party dependencies** in the trading path.
`proptest` and `trybuild` are dev-only.

---

## 5. Two things the type system enforces that you cannot see in a diagram

**A price is not a quantity.** `px + qty` does not compile. Neither does
`px * qty` — the product is `px.notional(qty, RoundDir::Down)`, which forces the
caller to say which way it rounds. There is no default rounding anywhere.

**Three clocks that never mix.** Exchange time, receive time and monotonic time
are different types. Subtracting one from another does not compile. That is why
there is exactly **one** cross-clock conversion in the whole system, in
`adapters/live::project_exchange_time`, with a written note on how it is wrong
and what it must not be used for. The types made it impossible to do by
accident, so doing it on purpose had to be deliberate.

---

## 6. The recording, and why everything is one file format

A session records **every input and every decision** to one file. That single
artifact serves three jobs:

```mermaid
graph LR
    S["a session<br/>(paper or backtest)"] --> LOG["session.log<br/>80-byte records"]
    LOG -->|"inputs only, fresh engine"| BT["backtest<br/><i>trade it again</i>"]
    LOG -->|"every input, silent venue"| RP["replay<br/><i>reproduce it exactly</i>"]
    LOG -->|"walk the records"| RPT["report<br/><i>PnL, drawdown, refusals</i>"]
```

The difference between the first two is the thing most likely to trip you up, so
the code makes you name it: `Replaying::MarketDataOnly` is a **backtest**, and
`Replaying::EveryInput` is a **replay**. Feeding a recording's venue reports back
in *and* binding a simulated venue would deliver every fill twice.

`bin/journal` is the tool that reads one:

```bash
journal session.log            # decisions, and what the venue said back
journal session.log --all      # every record, market data included
journal session.log --fills    # each fill with the running position and profit
journal session.log --curve    # the same as CSV, for plotting
```

It re-derives the books through `oms`'s accounting rather than reimplementing
it, so its numbers cannot drift from the session's. It is also the only check
that a recording is decodable by something *other than the code that wrote it*
— which is what makes "the log is the audit trail" a fact rather than an
intention. A recording whose tail a crash tore off still reads, and says so.

**A session is a chain of files, not one file.** `session.log` names the
session; its records live in `session.0.log`. If a crash tears the tail off
that file, the damaged file is **never written to again** — recovery opens
`session.1.log`, whose first record carries the next sequence number, and the
chain reads back as one gap-free session.

```text
session.0.log   seq 0 … 8_193      torn tail, left exactly as the crash left it
session.1.log   seq 8_194 …        the session continues here
```

Numbering the *first* segment is what makes this safe. If the first segment
were the bare `session.log`, a tool that forgot to look for `session.1.log`
would read part of a session and report it as all of it. Instead the bare path
holds nothing, so that tool fails loudly. Sequence numbers are the engine's
only ordering key, so a chain that restarts at zero or skips a number is not
one session — and `Segments::read_all` refuses it rather than concatenating.

**Why 80 bytes, fixed.** Record *n* sits at `header + n × 80`, so seeking to a
sequence number is arithmetic, and a file that ends mid-record is detectable from
its length alone. Every record carries a CRC and a format version. The header
carries the instrument table, so a recording is interpretable years later without
the config that produced it — and a backtest **refuses** a recording whose
instruments disagree with its own.

---

## 5b. Two strategies, and why the second one is not more arithmetic

`crossover` takes liquidity with market orders. `quote` rests a bid and an ask
around the midpoint and pulls them when the market drifts.

The second one exists to be a different **shape**. A market order is live and
gone inside one event; a resting order has to be named, watched, and cancelled.
Until `quote` existed, everything under that second half — resting orders at
the venue, limit prices through the risk gate, `CancelSubmitted` in the log,
the order machine's `PendingCancel` — was built and had **no production
caller**.

Two seams had to be finished for it to be possible at all:

| Missing | Why it mattered |
|---|---|
| A strategy could not learn its order's id | An order has no id until the gate approves and the venue accepts, so `ctx.order()` cannot return one. Without `StrategyEvent::OrderLive` a strategy could place a resting order and never manage it. |
| A strategy could not cancel | Only the operator's flatten-and-kill sweep could. A quote you cannot pull is a quote that fills on every adverse move. |

A strategy's cancel is recorded separately from the operator's sweep, because
"who pulled this quote" is exactly the sort of question the log exists to
answer. And a cancel is **not** put to the risk gate: pulling an order only
reduces exposure, and a gate that could refuse one is a gate that can trap a
strategy in a position.

---

## 6b. Coming back from a crash

Point `paper` at a session that already exists and it does not refuse and does
not start fresh — it **continues** that session.

```mermaid
graph TD
    C["a session dies<br/>tail torn off session.0.log"] --> R["restart, same session root"]
    R --> RP["replay session.0.log<br/><i>market, commands, timers</i>"]
    RP --> V["the simulated venue<br/>regenerates its own reports"]
    V --> CMP{"does the replay agree<br/>with the recorded fills?"}
    CMP -->|yes| H["<b>Halted</b><br/>holding what it held"]
    CMP -->|no| X["<b>refuse to start</b><br/>neither side is trusted"]
    H -->|operator types resume| RUN["Running, into session.1.log"]
```

Four things in that picture are the whole design:

1. **State comes back by replay, not by installation.** The engine runs its own
   recording through the same path that produced the state the first time. A
   second way to reach a position is a second answer waiting to disagree.
2. **The venue's recorded reports are not replayed.** A simulated venue
   regenerates them from the same market and the same orders, which is what
   rebuilds *the venue* — its book, its resting orders — instead of leaving it
   empty while the engine believes it is trading.
3. **Whatever was resting is cancelled.** Reconstructing the venue brings
   those orders back live, and left alone they keep filling while the session
   is halted — the position moves and nobody decided that it should. The queue
   position is lost, which is the price of coming back to a state somebody
   chose (contract D-3).
4. **That makes reconciliation a real check.** The replay and the recording are
   two independent accounts: one derived by trading forward through the venue,
   one by walking the recorded fills. They are compared per instrument, and a
   disagreement stops the session rather than picking a side.
5. **It comes back halted.** A crash is an incident; leaving a halt is an
   explicit operator decision and never automatic (Constitution V).

Order ids continue rather than restart, because the replay drives the same
counter that issued them.

**What this does not cover: a real venue.** Its reports cannot be regenerated,
so recovery against one has to ask it what it holds and cancel by its list
(contract D-3). None of that is built, and the comparison above is sound
precisely because a *simulated* venue is deterministic.

---

## 7. Verified end to end

A live paper session against Kraken — **two instruments at once** — and then a
backtest of its own recording, under the same config file:

```text
                     live      backtest of its recording
  orders               19            19
  refused               0             0
  XBTUSD realized   -4.600        -4.705      position -0.500 both
  ETHUSD realized   -0.080        -0.290      position +5.000 both
  fees               0.000         0.315
```

−4.600 − 0.105 = −4.705 and −0.080 − 0.210 = −0.290, and those two fee figures
sum to the 0.315 reported. Same orders, same refusals, same final position on
each book; the entire difference is the fees the backtest charges and the paper
session did not. Parity is demonstrated against real market data, per
instrument, rather than asserted.

An earlier single-instrument session, before the config existed, showed the
same thing on one book: 80 orders each side, one `StaleMarketData` refusal each
side, −16.60 − 1.49 = −18.09.

---

## 8. What is not built

So the map is not mistaken for the territory:

| Missing | Consequence |
|---|---|
| `bin/live` and a real venue adapter | nothing can lose money yet, by construction |
| Crash recovery against a **real** venue | D-1, D-2, D-3 and a simulated-venue form of D-4 are built (§6b). D-3 cancels what the *reconstructed* venue holds; against a real one it has to ask the venue for its open orders instead, and no real venue exists to ask. A `Recovering` engine state (D-6) would also be a wire-format change, so it waits for that work. |
| `client` | operator commands come from stdin; there is no separate UI process |
| Latency budget | no budget is declared. `cargo bench -p engine` now measures the hot path (~88 ns/event, one instrument, one strategy) so changes can be compared, but nothing says what the number is *allowed* to be. End-to-end latency still cannot be measured at all: the REST bridge is ~6s behind the market. |
| Duplicate market events | out-of-order events are refused; duplicates need a venue sequence number no feed has given us yet |
| More strategies | two kinds exist, `crossover` and `quote`. They were chosen to be different *shapes* rather than different arithmetic; a third would be research, and research enters through a spec. |
| A session start time in the log header | the header has the field; both writers leave it zero, because at that moment no exchange clock has been observed and a receive time is not one. `journal` derives the span from the records instead. |
