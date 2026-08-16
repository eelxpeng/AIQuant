# Status

What is built, what is not, and where the work is up to.

`README.md` says what this is. `docs/SYSTEM-MAP.md` says how the pieces fit
together. **This file says how far along it is** — it is the first thing to read
before picking up work, and the thing to update when landing any.

Today: **25,400 lines, 450 tests, 15 crates, 5 binaries, zero third-party
dependencies** in the trading path.

---

## The ladder

Each rung is a thing the system can do that it could not do before. A rung is
ticked only when it works end to end from a command line, not when its parts
exist.

- [x] **1 · A deterministic engine.** Market event in, order out, through a risk
      gate, recorded to a log. Same input, byte-identical output.
- [x] **2 · A session survives leaving the process.** A recording is a file with
      a stable format, and it reads back.
- [x] **3 · Backtest.** Run a strategy over a recording and get numbers out.
- [x] **4 · Paper trading.** A live market, a real clock, an operator who can
      halt it, and a simulated venue so nothing can lose money.
- [x] **5 · A session survives a crash.** Restart, rebuild what was held, come
      back halted, and wait for a human.
- [x] **6 · Trustworthy results.** A session's headline is now a complete
      profit-and-loss: realized plus the open position marked to market, with
      the rule and the timestamp stated, and a refusal rather than a zero when
      a position cannot be valued.
- [x] **7 · Research throughput.** Run a grid of configurations over one
      recording in one command, ranked, and measured out of sample so the
      in-sample winner can be caught being luck.
- [ ] **8 · A real venue, read-only.** ← **you are here.** Real market data over a real connection,
      still trading against the simulator.
- [ ] **9 · Live.** Real orders, real money. Everything above has to be true
      first, and rung 8 has to have run for a long time without surprises.

Rungs 1–7 took PRs #7 to #29. Rung 9 is deliberately far away.

---

## Built

### The engine and its seams

- [x] Fixed-point `Px` / `Qty` / `Notional`; no float anywhere near an order (#7)
- [x] Source-typed clocks — exchange, receive, monotonic cannot be mixed (#7)
- [x] Event log, market data, risk gate, order state machine, strategy trait,
      engine loop (#8)
- [x] Market events that arrive out of order are refused, not applied (#10)
- [x] Compile-level proofs: a strategy cannot reach the venue, the book, or a
      later event (#12)
- [x] The kill switch works from every state (#13)
- [x] A 10,000-event replay reproduces byte-identically (#11)
- [x] Zero-allocation hot path, proved by a counting allocator under CI (#8)

### The recording

- [x] Fixed 80-byte records, CRC per record, explicit wire discriminants (#14, #15)
- [x] Reader and writer, with every damage mode reported rather than swallowed (#16)
- [x] A recording replays as a feed — backtest and replay are different readings (#17)
- [x] Recording happens off the hot path, on a background writer (#19)
- [x] A session is a **chain of segments**, so a crash never rewrites evidence (#24)

### Running it

- [x] `record` — make a recording from a synthetic market (#18)
- [x] `backtest` — run a strategy over a recording (#18)
- [x] `paper` — a live feed, a real clock, an operator console (#21)
- [x] `journal` — read a recording back: decisions, fills, PnL curve (#23)
- [x] One session config drives all of them; multi-instrument (#23)
- [x] `cargo bench -p engine` — the hot path, ~88 ns/event (#25)
- [x] `sweep` — a grid of configs over one recording, in and out of sample (#29)
- [x] `harness` — one definition of what a backtest is, shared by both (#29)

### Fault tolerance

- [x] Crash-recovery contract, written before the code (#20)
- [x] Restart replays its own recording to rebuild state (#25)
- [x] It comes back **halted**; only an operator resumes it (#25)
- [x] Replay and recording are compared per instrument; a disagreement refuses
      to start (#25)
- [x] Whatever was resting is cancelled on restart (#26)

### Strategies

- [x] `crossover` — takes liquidity with market orders (#8)
- [x] `quote` — rests a bid and an ask, and pulls them (#26)
- [x] A strategy learns its order's id, and can cancel it (#26)

### Verified against a real market

- [x] Kraken paper session, two instruments, backtest of its own recording
      reproduces it **per instrument** (#23)
- [x] A killed paper session resumed, traded on, and the two segments read back
      as one (#25)

---

## Not built

Ordered by what I would do next, not by size.

### Reporting

- [x] **Mark-to-market** — realized, unrealized, and a total, per instrument
      and per session (#28)
- [x] **The mark rule and its timestamp are reported**, because two rules give
      two answers over one log (#28)
- [x] **A position with no usable mark is named, not valued at zero** (#28)
- [x] **Drawdown is marked to market** — it was realized-only, which missed
      every fall a session rode out in an open position (#28)
- [ ] Sharpe, hit rate, average edge per fill — the numbers that make a quoter
      and a crossover comparable on more than one number. These belong with
      rung 7: they exist to rank runs against each other.

### Research throughput

- [x] `sweep` — a grid of variants over one recording, ranked by what each was
      worth, in one command (#29)
- [x] `--split` measures every variant out of sample and says when the ranking
      rearranged, which is what overfitting looks like (#29)
- [x] A sweep with no split nags about it (#29)
- [ ] **Rolling walk-forward** — many consecutive train/test windows rather
      than one split. One split is one experiment; a parameter that survives
      six consecutive windows is a different quality of evidence.
- [ ] Sharpe, hit rate, average edge per fill. The table ranks on total, which
      says nothing about how much risk bought it — `total` and `drawdown` side
      by side is the poor version of that.

### Towards a real venue — rungs 8 and 9

- [ ] `bin/live` and a real venue adapter. Nothing can lose money until this
      exists, which is by construction and not an accident.
- [ ] **D-3 against a real venue** — ask the venue for its open orders and
      cancel by *its* list. The current form cancels what the *reconstructed*
      simulated venue holds. A real venue has to be asked.
- [ ] **`EngineState::Recovering`** (contract D-6). It is wire-encoded in the
      log, so it costs a format version bump, and it only earns that when there
      is a real venue to reconcile against.
- [ ] Reconnect idempotency — a venue-side order id that survives a reconnect.
      Unanswered since the log-format ADR.

### Smaller, known, and deliberately left

- [ ] **`session_start` in the log header is always zero.** At header-write time
      no exchange clock has been observed, and a receive time in an
      exchange-time field is the mixing the type system exists to prevent.
      Deciding what the field should hold is an event-schema change.
- [ ] **Duplicate market events.** Out-of-order events are refused; duplicates
      need a venue sequence number no feed has given us.
- [ ] **No declared latency budget.** The hot path is now measured, so changes
      can be *compared*, but nothing says what the number is allowed to *be*.
      The constitution asks a hot-path change to cite a benchmark against a
      budget; there is no budget to cite against.
- [ ] **End-to-end latency cannot be measured at all.** The REST bridge is ~6s
      behind the market. That needs a streaming feed, which is rung 8 work.
- [ ] **No separate `client` process.** Operator commands come from stdin.

---

## Keeping this honest

- A rung ticks when the thing works from a command line, not when its parts
  compile.
- Land a change, update this file in the same PR, and cite the PR number.
- An item that has been "next" for a long time is either not next or not real.
  Say which.
- This file records **state**, not plans. Priority and scope splits are the
  human's, and the roadmap issue (#1) is still where those live.
