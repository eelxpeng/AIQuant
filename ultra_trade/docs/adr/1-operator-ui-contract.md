# The operator UI contract

- **Issue**: #1 (needs its own issue — see the other ADRs' "Naming" note)
- **Status**: accepted
- **Date**: 2026-08-18

## Context

Everything this system does is a terminal and a text file. `journal` prints a
recording, `sweep` prints a table, `paper` prints a running commentary, and an
operator halts a session by typing `halt` into the process's standard input.

That has been enough to build eight rungs, and it is now the largest gap by
user-visible surface (`STATUS.md`, rung 10). Two things go wrong without a UI:

- **Nobody looks.** A session that did something strange says so in a file
  somebody has to think to read. The equity curve, the refusal counts, the
  feed-lag tail and the book resets are all derivable today and none of them
  are in front of anyone.
- **The operator's reach is a terminal.** Halting a session requires being at
  the console that started it. The `client` process has been outstanding since
  `docs/ARCHITECTURE.md` was written and this is it.

A UI is also the first thing in this repository that a person *interacts with
while money is at stake*, so the interesting decisions are all about what it is
not allowed to do.

## Decisions

### D-1 · The UI is not in the Rust workspace

A web server, its TLS, its async runtime and their dependency trees do not
enter this workspace. The trading path has **zero third-party dependencies** and
a user interface is not a reason to acquire the first one.

This is the same rule that keeps the venue bridges in `tools/`, for the same
reason: a real-money binary should not link a stack it acquired for a
convenience. The UI is a program in `tools/` that reads what the Rust tools
produce.

**Consequence**: the UI can be rewritten, replaced, or thrown away without a
Cargo file changing. That is the point.

### D-2 · The UI never decodes the log itself

It calls `journal`. It does not open a `.log` file, and it does not know that a
record is eighty bytes.

The format has a version, a CRC per record, a torn-tail rule, a segment chain
and a book-reset semantic, and **it has been bumped twice in the last month**.
A second decoder would be a second answer to "what happened in this session",
and the moment the format moved it would be quietly wrong rather than loudly
broken — which is the failure mode this codebase spends the most effort
avoiding.

`journal` already re-derives everything through `report`, which re-derives
through `oms`. The UI is the fourth consumer of one answer, not a second
implementation.

### D-3 · Reading and writing are two different programs

The read path — recordings, live monitoring, sweep results — touches nothing.
The write path — halt, resume, kill, flatten — is the only part that can affect
money, and it is a **separate binary, separately enabled, off by default**.

Bundling them would mean the thing somebody leaves open on a second monitor all
day is also the thing that can flatten a book. Splitting them means the viewer
can be careless and the console cannot.

**The console binds to loopback only and there is no authentication.** Stated
plainly rather than implied: this is not a service to expose, and until
somebody decides how an operator proves who they are, it is a single-machine
tool. That decision is deferred, not overlooked.

### D-4 · The log is the acknowledgement

An operator command is a line. Writing a line tells you it was *written*, not
that the engine acted on it — and the gap between those is exactly where a UI
invents a lie, by colouring a button green because the write returned.

Every command the engine accepts becomes a record: a `CommandEvent` for the
command, and a `StateChanged` for what it did. So:

> **A command is pending until it appears in the recording. The UI shows it as
> pending, and never as done, until then.**

This falls out of the design rather than being added to it — the log already
exists to answer "why did this happen", and "did my command land" is the same
question asked sooner.

### D-5 · What a disconnect means, per command

A UI that loses its connection between sending and confirming does not know
whether the command landed. The safe behaviour depends on the command, and this
is the table it depends on:

| Command | Sent twice | On a disconnect, the UI should |
|---|---|---|
| **halt** | idempotent — already halted is a no-op | offer a retry freely |
| **resume** | idempotent — already running is a no-op | offer a retry freely |
| **kill** | idempotent — terminal, and re-killing does nothing | offer a retry freely, and this is the one that matters most |
| **flatten** | **not idempotent** | show it as unknown and require the operator to look at the position before retrying |

**Flatten is the exception and the reason this table exists.** It raises
reduce-only intents for whatever is not flat. Sent twice before the first
fills, it raises a second set against a position the first is already
addressing. Reduce-only bounds the damage — neither set can increase
`|position|` — but the operator can still end up flatter than intended, in a
moment they are already having a bad time.

So the UI never auto-retries a flatten. It says "unknown" and shows the
position, which is what a person needs anyway.

### D-6 · The UI cannot make the engine wait

Nothing the UI does may block, slow, or backpressure a session. It reads a file
the session is already writing; it writes a line the session polls for. A UI
that hangs, crashes, or is never started changes nothing about the run.

This is why the read path is "re-read the recording" rather than a socket the
engine pushes into. A push would put a consumer on the far side of the engine
whose slowness is the engine's problem, and *backpressure is a design decision,
never an accident*.

### D-7 · The read path is polling, and says how stale it is

The UI re-reads the recording on a timer. It is therefore always behind, and it
**displays how far** — the same discipline the feed-lag measurement follows.

A live view that silently shows a two-second-old position looks identical to
one showing the current position, and the difference matters when somebody is
deciding whether to flatten.

## The capability matrix

What each half may do, spelled out including the degenerate cases.

| | **Viewer** | **Console** |
|---|---|---|
| Read a finished recording | yes | yes |
| Follow a running session | yes | yes |
| Run a sweep | yes — it is a backtest, it cannot touch a live session | yes |
| Send halt / resume / kill | **no** | yes |
| Send flatten | **no** | yes, without auto-retry (D-5) |
| Reach the engine's memory | no — there is no such path | no |
| Change a config or a limit | **no**, and not planned | **no** |

The last row is deliberate. A risk limit is changed by editing a config and
restarting, which is a reviewable act that leaves a file behind. A button that
widens a limit at three in the morning is precisely the affordance this system
should not have.

## What this requires that does not exist yet

Stated so the implementation is not a surprise. Neither is large; both are
prerequisites rather than details.

- **Structured output from the Rust tools.** `journal --json` and
  `sweep --json`. Today everything is aligned text for a human, and a UI
  parsing that would break the first time a column moved. This is the seam D-2
  depends on and it should land first, on its own, with its own tests.
- **`paper` must accept commands from somewhere other than stdin.** It reads
  them from the terminal that launched it, so a separate process cannot send
  one. A `--commands <path>` reading a named pipe is the smallest change that
  makes the console possible at all. Note that today, when the market source is
  stdin, commands are disabled entirely — a pipe removes that limitation too.
- **A way to tell a live session from a finished one.** The viewer needs to
  know whether to keep polling. The recording being appended to is the signal;
  nothing currently reports it.

## Alternatives rejected

| Alternative | Why it lost |
|---|---|
| A Rust web server in the workspace | Buys the trading path an async runtime and a TLS stack for a convenience (D-1). |
| The UI reads the log format directly | A second decoder of a format that has bumped twice this month, wrong quietly rather than loudly (D-2). |
| One program that both views and controls | The thing left open all day becomes the thing that can flatten a book (D-3). |
| The engine pushes to the UI over a socket | Puts a consumer the engine has to wait for on the hot side of it (D-6). |
| Confirm a command by the write succeeding | Colours the button green when the line was written, not when the engine acted (D-4). |
| Auto-retry every command on a disconnect | Correct for three of the four, and doubles a flatten in the moment it is least welcome (D-5). |
| Let the UI edit risk limits | A limit widened by a button at three in the morning leaves no reviewable artefact. |

## Consequences

**What this constrains.** The UI is two programs outside the workspace, reading
`journal`, writing a line, and never in a position to make the engine wait. Its
liveness is visible and its commands are pending until the log says otherwise.
Nothing it does can change a limit.

**What it costs.** Polling means the view is always slightly stale, and the
staleness has to be shown rather than hidden. Two programs is more to build
than one. `journal --json` is a second output format to keep working.

**What it does not settle.** Authentication, and therefore anything beyond a
single machine (D-3). Whether the viewer should render an order book, which is
a question about what is useful rather than what is safe. Whether a sweep
launched from the UI should be allowed to consume the machine a live session is
running on — a resource question this record does not answer.
