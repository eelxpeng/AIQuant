# ultra_trade Domain Language

This file records project terms that need exactly one shared meaning.

Every term used in a spec MUST appear here. The `_Avoid_:` line names the
concepts this term is **not** — that line is what stops two specs from quietly
redefining each other's nouns.

Adding a term is a normal part of the PR that first uses it.

## Instruments And Prices

**Instrument**:
The tradable contract at a specific venue, identified by an internal id that is
stable across sessions. It carries the tick size, lot size, and quoting
convention that every rounding decision uses.
_Avoid_: Symbol string, underlying asset, venue ticker

**Px**:
A fixed-point price in the instrument's quoting convention — `i64` at `1e-9`
scale (ADR #4). Never a float, never compared with a tolerance. Adding a `Px` to
a `Qty` does not compile.
_Avoid_: f64 price, notional value, mark

**Qty**:
A fixed-point quantity in the instrument's lot convention — `i64` at `1e-9`
scale (ADR #4), which admits fractional shares. Signed only where the type says
so; unsigned quantity plus an explicit `Side` is preferred.
_Avoid_: Position, exposure, notional

**Notional**:
The money value of a price times a quantity — `i128` at `1e-9` scale (ADR #4).
Wider than `Px` and `Qty` because cumulative traded value overflows 64 bits on an
ordinary day. The only type produced by multiplying the other two.
_Avoid_: Px, exposure, position value at an unstated mark

**Bar**:
An aggregate over trades or quotes under a stated convention — time, volume,
dollar, tick-imbalance. A convention, never a market fact. Built by
`marketdata`'s machinery from a configured definition; never by a strategy.
_Avoid_: Candle as a fact, tick, trade print

**Complete Bar**:
A bar whose window has closed, so its values can no longer change. Only complete
bars are visible to a strategy — exposing an incomplete one as complete is
look-ahead.
_Avoid_: Current bar, forming bar, latest bar

**Top Of Book**:
The best bid and best ask with their sizes, at a stated event timestamp.
_Avoid_: Depth, last trade, mid

**Mid**:
The arithmetic midpoint of top-of-book bid and ask. A derived convenience, never
a price the system can trade at.
_Avoid_: Fair value, mark price, last

**Mark Price**:
The price used for valuation and risk, chosen by an explicit, spec'd rule. It is
a policy decision, not "whatever price was handy".
_Avoid_: Mid, last trade, index price

## Order Lifecycle

**Intent**:
A strategy's request to change its target state. It has not been risk-checked
and has no venue identity. Produced by `strategy`, consumed by `risk`.
_Avoid_: Order, signal, target

**Order**:
A risk-approved instruction with a system-assigned id, tracked through a state
machine until it is terminal. It exists whether or not the venue has
acknowledged it.
_Avoid_: Intent, fill, trade

**Fill**:
A venue-reported partial or complete execution of one order, with its own
quantity, price, fee, and venue timestamp.
_Avoid_: Trade, execution report, order

**Trade**:
The realized round trip used for PnL attribution and reporting. Derived from
fills; never a synonym for one.
_Avoid_: Fill, order, position change

**Terminal State**:
An order state from which no further transition is possible: filled, cancelled,
rejected, or expired. Reconciliation may only move an order TO a terminal state,
never out of one.
_Avoid_: Inactive, done, closed

## Position And PnL

**Position**:
The system's own accounting of net quantity held per instrument, derived
entirely from fills in the event log.
_Avoid_: Exposure, venue position, inventory target

**Venue Position**:
The position as reported by the venue. Compared against `Position` during
reconciliation; a divergence halts trading and is never silently resolved.
_Avoid_: Position, exposure

**Exposure**:
A risk-side measure of economic size, computed from position and mark price
under a stated rule. Used by limits; never used for accounting.
_Avoid_: Position, notional, margin

**Realized PnL**:
Profit and loss from closed quantity, computed from fills with fees included.
_Avoid_: Unrealized PnL, mark-to-market, gross PnL

**Unrealized PnL**:
Profit and loss on open quantity, valued at the mark price at a stated
timestamp. Always reported with the mark rule and timestamp that produced it.
_Avoid_: Realized PnL, expected PnL

## Time And Clocks

**Exchange Time**:
The timestamp the venue assigns to an event. The only clock valid for ordering
market events or reasoning about market state.
_Avoid_: Receive time, local time, monotonic time

**Receive Time**:
The local wall-clock instant at which the process observed an event. Valid for
latency measurement, never for market-state logic.
_Avoid_: Exchange time, monotonic time

**Monotonic Time**:
The injected local monotonic clock, used for elapsed-time and timeout logic.
Never compared to exchange or receive time.
_Avoid_: Wall clock, exchange time

**Current Event Time**:
The exchange timestamp of the event a component is processing right now. This is
what a strategy reads instead of a clock. It is deterministic by construction and
makes look-ahead structurally impossible: nothing can observe a time later than
the event in hand.
_Avoid_: Now, wall clock, exchange time in general

**Timer Event**:
A wake-up a strategy requested for a future instant, delivered as an ordinary
event. Both the request and the firing are in the log, so both replay. This is
how a strategy acts when no market data arrives.
_Avoid_: Timeout, sleep, scheduled task

**Tick-To-Trade**:
The elapsed monotonic time from a market-data event entering the process to its
resulting order leaving the process. The headline latency budget.
_Avoid_: Round-trip latency, internal latency, wire time

## Execution Modes

**Backtest**:
A run whose feed adapter replays historical data and whose venue adapter is a
simulated venue with an explicit fill model. Uses the same engine and strategy
code as live.
_Avoid_: Simulation, paper trading, replay

**Replay**:
Re-running the engine over a recorded event log to reproduce byte-identical
output. A correctness tool, not a research tool.
_Avoid_: Backtest, simulation

**Paper**:
A run bound to a live feed adapter and a simulated venue adapter. Proves the
live data path without venue risk.
_Avoid_: Backtest, live, dry run

**Live**:
A run bound to a live feed adapter and a real venue adapter. The only mode that
can lose money.
_Avoid_: Paper, production environment

**Fill Model**:
The named, spec'd component that decides whether and at what price a simulated
order executes. Its assumptions are written down; a constant in a test harness
is not a fill model.
_Avoid_: Slippage estimate, latency model

## Strategy And Research

**Strategy**:
The component that consumes events and emits intents. It holds state but reaches
nothing — no clock, no socket, no file.
_Avoid_: Model, alpha, signal

**Model**:
A research artifact (parameters, weights, a rule set) that a strategy loads. It
is data, versioned and pinned; it is never executable code imported from
`../research/`.
_Avoid_: Strategy, alpha

**Signal**:
A strategy's numeric view of opportunity, before position sizing and before any
risk consideration.
_Avoid_: Intent, order, alpha

**Look-Ahead**:
Any use of information that would not have been available at the event's
timestamp in live trading. A correctness bug, always.
_Avoid_: Bias, overfitting, hindsight

## Risk

**Risk Gate**:
The single mandatory checkpoint every intent passes to become an order. It has
no bypass path.
_Avoid_: Risk check, limit, validator

**Limit**:
A configured bound the risk gate enforces (position, exposure, order rate,
notional, loss). Every limit names its unit and its evaluation window.
_Avoid_: Threshold, budget, guard

**Kill Switch**:
The operator control that halts new orders and, per its spec, cancels resting
ones. It MUST work from every system state, and MUST have at least one path that
does not depend on the client process being alive.
_Avoid_: Shutdown, pause, circuit breaker

**Command Event**:
An operator action recorded in the event log as an input to the session — the
only way a client reaches the engine. Because it lives in the log, replay
reproduces it and the audit trail exists with no separate logging path. The v1
set is exactly halt, resume, kill switch, and flatten.
_Avoid_: RPC call, control message, engine API

**Reduce-Only Order**:
An order that can only decrease `|position|`. The risk gate treats it
differently from a risk-increasing order: permitting it is strictly less risky
than blocking it, so it survives conditions that reject a normal order.
_Avoid_: Closing order, hedge, cancellation

**Flatten**:
The command that drives every position to zero by emitting reduce-only orders.
The only command in the control surface that creates orders, and therefore the
only one that meets the risk gate.
_Avoid_: Kill switch, halt, liquidation

**Halt**:
The state entered when an invariant is violated (reconciliation divergence,
stale data, unreachable limit config). Exiting a halt is an explicit, logged
decision, never automatic.
_Avoid_: Kill switch, disconnect, idle
