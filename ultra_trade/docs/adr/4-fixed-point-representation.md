# Fixed-point representation for Px, Qty, and Notional

- **Issue**: #4
- **Status**: accepted
- **Date**: 2026-08-12

## Context

Principle VII forbids floating point in any order or accounting path, so money
and quantities need a fixed-point representation. The choice has to be made
before D0.1 (`types`) lands, because it threads through every arithmetic site in
the system and is expensive to revisit afterwards.

The decision looks like one question and is actually three:

1. **Type discipline** — one shared `Fixed` type, or distinct `Px` / `Qty` /
   `Notional`? Drives correctness.
2. **Scale policy** — one global scale, or per-instrument? Drives whether this is
   easy or hard.
3. **Representation** — scaled integer or a decimal crate? Drives performance.

What the traded markets demand sets the answer to (2), and (2) constrains the
rest. The near-term market is **US equities**; crypto is planned but not
imminent.

| | Need | Source |
|---|---|---|
| Price floor | `1e-4` | SEC Rule 612 sub-dollar tick ($0.0001); half-penny ticks also exist |
| Price ceiling | ~`1e6` | BRK.A trades around $700k |
| Qty floor | ~`1e-6` | fractional shares are in scope |
| Notional ceiling | `1e9`–`1e12` | position value and cumulative daily traded value |

`i64` at a `1e-9` scale spans ±9.22e9 with nine decimal places — five orders of
magnitude of precision headroom and three of range against that table. Equities
do not stress it.

Crypto would. SHIB near `1e-8` gets one significant digit at `1e-9` scale, and
dropping the scale to `1e-18` collapses `i64`'s ceiling to ~9.2. There is no
scale that serves both; crypto forces a wider integer, a per-instrument scale, or
a decimal type. That is a known future change, recorded under Consequences.

## Decision

**Three distinct newtypes, each with the representation it needs.**

| Type | Backing | Scale | Range |
|---|---|---|---|
| `Px` | `i64` | `1e-9` | ±9.22e9 |
| `Qty` | `i64` | `1e-9` | ±9.22e9 |
| `Notional` | `i128` | `1e-9` | ±1.7e29 |

- `Px + Qty` MUST NOT compile. `Px * Qty` yields `Notional`. This is dimensional
  analysis, it costs nothing at runtime, and it eliminates the class of bugs
  where a price is added to a quantity or prices are summed across instruments.
- `Notional` is `i128` because `Px * Qty` at `1e-9` each already forces an `i128`
  intermediate on every multiply, and because cumulative daily traded notional
  would exceed `i64`'s $9.2e9 ceiling on an ordinary day. Distinct types are what
  make this possible: a single shared `Fixed` type would have to pick one width
  and either waste cache on prices or overflow on aggregates.
- Backing integers are **private**. No code outside `types` touches them; all
  arithmetic goes through the newtype APIs.
- `SCALE` is a named constant, as a single source of truth — not as a
  switchability mechanism (see Alternatives, "backing type alias").

**Overflow: checked at boundaries, `debug_assert` in the interior.**

Checked arithmetic returning `Result` at construction, parsing, venue I/O, and
aggregation — the places where an out-of-range value can enter or leave. Interior
arithmetic uses debug-mode overflow panics. This keeps `?` off every hot-path
expression while making an out-of-range value impossible to *introduce* silently.

**Rounding: explicit direction at every venue boundary**, per Principle VII.
There is no default rounding mode; a call site that does not state a direction
does not compile.

## Alternatives rejected

**`rust_decimal`** — 10–50× the cost of `i64` arithmetic on the hot path. Failed
property: Principle VI latency budgets. It would also embed a library's internal
representation in the durable log format, so replaying a five-year-old log would
depend on a pinned crate version.

**`bigdecimal`** — allocates. Failed property: the hot path forbids allocation,
outright and with no argument available.

**`i128` for everything now** — doubles every price and size from 8 to 16 bytes.
A top-of-book record with two prices and two sizes goes from 32 to 64 bytes,
halving L1 density. Failed property: hot-path cache efficiency, paid on every
event every day, to serve a market not yet traded. The migration is bounded
anyway (see Consequences).

**A single shared `Fixed` type** — forces one backing width for both `Px` and
`Notional`. Failed property: each quantity carrying the representation it
actually needs; you get either wasted cache or aggregate overflow, with no third
option.

**`Px` stored as a count of ticks** — tempting, because tick rounding becomes
impossible to forget when it *is* the representation, and comparison is exact
integer comparison. Failed property: self-describing durable records. Tick tables
vary by price band and change over time, so a stored tick count's meaning depends
on which table was live when it was written, and long-horizon replay breaks.
Tick rounding stays an explicit operation at the venue boundary.

**A backing type alias (`type Backing = i64`) for later switchability** — does
not do what it appears to. Call sites are already insulated by the newtype;
`Px`'s internals are one file either way. The migration cost lives in the event
log record format and every golden fixture on disk, and a type alias has no reach
there. Failed property: honest cost representation — it would make a log-format
migration read as "just change the constant," inviting exactly the flip that
makes historical fixtures unreadable.

**`Fixed<const SCALE: i32>` const generic** — the real version of parameterized
scale, and viral across every signature that touches a number. Failed property:
proportionality. Equities need one scale; paying a type-system cost everywhere to
avoid a decision we have already made is speculative configuration (constitution:
no speculative abstractions).

**Saturating arithmetic** — fast, and silently produces a wrong number that
looks like a right one. Failed property: fail-closed (Principle V).

## Consequences

**Constrains.** Every arithmetic site goes through `Px` / `Qty` / `Notional`.
Backing integers stay private to `types`. Every multiply carries an `i128`
intermediate. Boundary arithmetic carries `?`.

**Enables.** Each quantity gets an independently chosen width, which is the whole
return on the type discipline.

**Makes record-format versioning load-bearing.** The `i128` migration is cheap in
code and expensive on disk. Event log records MUST carry a format version from
the first release, so that migration is a supported operation — old logs replay
through a v1 reader, new ones write v2 — rather than a break. Add this in D0.2;
retrofitting it after logs exist is the expensive order.

**Crypto forces a change, and it is not a constant flip.** Any instrument priced
below roughly `1e-6` exhausts the useful precision of `i64` @ `1e-9`. Moving to
`i128`, a per-instrument scale, or a decimal type is an architecture change with
a log-format migration and a fixture regeneration. Recorded here so the cost is
visible before someone pays it by accident.

**What would make us revisit**: trading crypto or any instrument priced below
~`1e-6`; a measured cache or latency problem attributable to value width; or a
venue requiring more than nine decimal places.
