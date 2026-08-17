# Status

What is built, what is not, and where the work is up to.

`README.md` says what this is. `docs/SYSTEM-MAP.md` says how the pieces fit
together. **This file says how far along it is** — the first thing to read
before picking up work, and the thing to update when landing any.

Today: **27,700 lines, 490 tests, 15 crates, 5 binaries, zero third-party
dependencies** in the trading path — and a UI that adds none.

---

## The ladder

Each rung is something the system can do that it could not do before. A rung
ticks when it works end to end from a command line, not when its parts compile.

- [x] **1 · A deterministic engine.** Market event in, order out, through a risk
      gate, recorded. Same input, byte-identical output.
- [x] **2 · A session survives leaving the process.** A recording is a file with
      a stable format, and it reads back.
- [x] **3 · Backtest.** Run a strategy over a recording and get numbers out.
- [x] **4 · Paper trading.** A live market, a real clock, an operator who can
      halt it, a simulated venue so nothing can lose money.
- [x] **5 · A session survives a crash.** Restart, rebuild what was held, come
      back halted, wait for a human.
- [x] **6 · Trustworthy results.** A complete profit-and-loss: realized plus the
      open position marked to market, with the rule and timestamp stated.
- [x] **7 · Research throughput.** A grid of configurations over one recording,
      ranked, measured out of sample.
- [x] **8 · A real venue, read-only.** Streaming market data with depth and
      checksums, still trading against the simulator.
- [ ] **9 · Believable fills.** The simulator's remaining
      optimism named and bounded, so a backtest result can be argued for rather
      than merely produced.
- [x] **10 · Somewhere to look.** A web viewer and a separate operator console:
      watch a live session, read a recording, and press the four buttons.
- [ ] **11 · Sustained operation.** ← **you are here.** Rung 8 run for days rather than minutes,
      with the failures that only appear at that timescale.
- [ ] **12 · Live.** Real orders, real money.

Rungs 1–8 and 10 took PRs #7 to #35. Rung 12 is deliberately far away and nothing
below should be read as saying otherwise.

---

## What needs building

Ordered within each theme by what I would pick up first. Every item is meant to
be startable from what is written here.

### 1 · Believable fills — rung 9

The simulator is a model, and the honest question is not "is it right" but
"which way is it wrong, and by how much". Each of these is a known lie with an
unknown size.

- [x] **Queue position** (#33). A resting order used to fill on the first print
      at its price; it now waits behind the size that was already there. On a
      quoter joining a level with four ahead of it: **200 fills → 66**, and the
      profit fell to a third. Off by default.
- [ ] **Cancellations do not move you up the queue.** Deliberately pessimistic:
      a level shrinking says somebody left, not whether they were ahead of us.
      A proportional model would be closer to reality and needs a defensible
      rule rather than a tuned constant.
- [ ] **Market impact.** The biggest remaining lie. The recorded book is what
      the market showed *without* our order in it; walking it assumes the levels
      would have sat still while we ate them. Even a crude size-proportional
      slippage term, clearly labelled a model rather than data, beats the
      current silence.
- [ ] **Hidden liquidity.** Displayed size is not all the size, in either
      direction. Unmodelled and unmeasured.
- [ ] **Latency between decision and arrival.** `SimVenue` takes a constant
      `latency` and every session passes zero, so a strategy reacting to a quote
      is assumed to be at the venue instantly.

### 2 · Somewhere to look — rung 10

Everything today is a terminal and a text file. This is the largest single gap
by user-visible surface, and it absorbs the `client` process outstanding since
the architecture doc was written.

- [x] **The contract, written first** (#34,
      `adr/1-operator-ui-contract.md`). What the UI may read, what it may
      write, and what a disconnect means per command.

- [x] **`journal --json` and `sweep --json`** (#35). Money is emitted as
      strings, never JSON numbers — a fixed-point price through a double is
      not the price any more.
- [x] **`paper --commands <path>`** (#35). A named pipe, so a separate process
      can steer a session; standard input belongs to whoever launched it. Also
      removes the old limitation that commands were disabled entirely when the
      market source was stdin.
- [x] **`tools/ui-view.py`** (#35) — the read-only viewer. Totals marked to
      market, equity curve, positions, refusals, feed lag, recent fills. It
      polls, so it is always slightly behind, and it says how far.
- [x] **`tools/ui-console.py`** (#35) — the same page plus the four buttons,
      a separate program you start deliberately. A command reports `pending`
      and is only done when it appears in the recording.
- [x] **Live or finished** is derived rather than asserted: the recording
      growing is the only evidence a session is running, and the page shows
      "no new records for Ns" when it stops.
- [ ] **Sweep results in the UI.** `sweep --json` exists and nothing renders it
      yet — the in-sample and out-of-sample columns side by side, sortable,
      with the rank change called out.
- [ ] **The book, rendered.** Depth is in the recording and the page does not
      show it. A ladder is the obvious thing a person wants when deciding
      whether a quote is well placed.
- [ ] **Authentication**, and therefore anything beyond one machine. Both
      servers bind to loopback and neither asks who you are.

**The rules, from the ADR.** The UI is not in the Rust workspace and never
decodes the log itself — it calls `journal`, because a second decoder of a
format that has bumped twice this month would be wrong quietly rather than
loudly. Viewing and controlling are **two programs**, so the thing left open on
a second monitor all day is not the thing that can flatten a book. A command is
**pending until it appears in the recording**, because a write returning tells
you the line was written and not that the engine acted. And the UI can never
change a risk limit: that is an edit to a config and a restart, which leaves a
reviewable artefact behind.

Flatten is the one command a UI must not auto-retry on a disconnect — it is the
only one that is not idempotent, and doubling it is worst in exactly the moment
somebody reaches for it.

### 3 · Research quality

Making it harder to fool yourself.

- [ ] **Rolling walk-forward.** `sweep --split` is one experiment; six
      consecutive train/test windows are a different quality of evidence.
      `harness::split_by_market_events` generalises to it.
- [ ] **Risk-adjusted ranking.** `sweep` sorts on total, which picks the most
      levered variant every time. Hit rate, average edge per fill and
      return-over-drawdown all come from data already in the log.
- [ ] **Per-round-trip results**, which hit rate needs. `oms` matches lots FIFO
      and does not record individual round trips; that is a change to position
      accounting and wants surfacing before it is made.

### 4 · Market data

- [ ] **A second venue.** Everything venue-specific is in `tools/`, which is the
      claim this would test. The checksum is Kraken's algorithm and a second
      venue needs its own.
- [ ] **Duplicate detection.** Out-of-order events are refused; duplicates need
      a venue sequence number no feed has given us yet.
- [ ] **Depth beyond the top levels.** The bridge subscribes to ten a side and
      `Books` holds sixteen. A strategy trading real size wants more, and the
      log volume grows with it.

### 5 · Going live — rung 12

- [ ] **Where a credential lives.** This repository has never held one. How a
      key is stored, how it reaches the process, and how it is kept out of a log
      and a core dump is undecided — a prerequisite, not a detail.
- [ ] **`bin/live` and a real venue adapter.** Nothing can lose money until this
      exists, which is by construction.
- [ ] **D-3 against a real venue** — ask it for its open orders and cancel by
      *its* list. The current form cancels what the reconstructed simulated
      venue holds.
- [ ] **`EngineState::Recovering`** (contract D-6), a wire-format change that
      only earns its cost alongside a real venue.
- [ ] **Reconnect idempotency** — a venue-side order id that survives a
      reconnect. Unanswered since the log-format ADR.

### 6 · Operations — rung 11

- [ ] **A declared latency budget.** The hot path (~88 ns/event) and the feed
      (~70 ms p50) are both measured; nothing says what either is *allowed* to
      be. The constitution asks a hot-path change to cite a benchmark against
      the budget, and there is no budget to cite against.
- [ ] **End-to-end latency.** The two halves are measured separately; nothing
      measures wire to order.
- [ ] **A multi-day paper session.** Everything run so far is seconds to
      minutes. Log volume, background-writer backpressure, reconnects, resyncs
      and recovery under real conditions are untested at the timescale that
      matters. This is what earns rung 12, and no feature substitutes for it.

### 7 · Small, known, and deliberately left

- [ ] **`session_start` in the log header is always zero.** At header-write time
      no exchange clock has been observed, and a receive time in an
      exchange-time field is the mixing the types forbid. Deciding what the
      field should hold is an event-schema change.
- [ ] **Two format bumps in two PRs** (2 for depth, 3 for the reset). Both were
      avoidable with more foresight. Nothing to fix; a note to do better.
- [ ] **`report` recomputes the book** to get a mark. Correct, and it means a
      report walks every market record twice over a long session.

---

## Built

Summarised; `docs/SYSTEM-MAP.md` has the detail.

| Area | What works | PRs |
|---|---|---|
| Types | fixed-point money, source-typed clocks, one sanctioned cross-clock measurement | #7, #30 |
| Engine | deterministic loop, risk gate, order machine, FIFO accounting, kill switch | #8, #13 |
| Recording | 80-byte records, CRC, background writer, segment chains, format 3 | #14–#19, #24, #31, #32 |
| Running it | `record`, `backtest`, `paper`, `journal`, `sweep` | #18, #21, #23, #29 |
| Recovery | replay-based, halted on return, per-instrument reconciliation, resting orders cancelled | #25, #26 |
| Strategies | crossover (takes), quoter (rests and cancels) | #8, #26 |
| Results | mark-to-market, marked drawdown, out-of-sample sweeps | #28, #29 |
| Market data | streaming websocket, depth, venue checksums, lag measurement | #30, #31, #32 |
| Fills | touch, walk-the-book, queue position | #31, #33 |

---

## Keeping this honest

- A rung ticks when the thing works from a command line, not when its parts
  compile.
- Land a change, update this file in the same PR, cite the PR number.
- An item that has been "next" for a long time is either not next or not real.
  Say which.
- This file records **state**. Priority and scope splits are the human's, and
  the roadmap issue (#1) is where those live.
