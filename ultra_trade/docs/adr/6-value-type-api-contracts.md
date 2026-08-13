# Value and clock API contracts left open by ADR #4

- **Issue**: #6
- **Status**: accepted
- **Date**: 2026-08-12

## Context

[ADR #4](4-fixed-point-representation.md) settled the representation: three
newtypes, `i64`/`i64`/`i128`, all at `1e-9`, checked at boundaries and
debug-asserted in the interior, with no default rounding mode. Writing the spec
for #6 surfaced three questions its text does not answer. Each one changes the
compile-fail test set, and each is expensive to reverse once other crates depend
on the shape.

They are one ADR because they are one class of question: what the *API surface*
of `types` may do, given #4's representation. All three span more than one spec —
notional products appear in `risk`, `oms`, and `report`; spans appear anywhere
elapsed time is measured — so none belongs in a single spec's `DECISIONS.md`.

## Decision

**1. The notional product names its rounding direction, and `Px * Qty` is not an
operator.**

At `1e-9`, the exact product of a price and a quantity can need up to eighteen
decimal places, and nine is what `Notional` holds. This is not a corner case for
the near-term market: `10.0001 × 0.000001 = 0.0000100001` is a sub-dollar tick
times a fractional share, and it needs ten.

The rule: the product is spelled `px.notional(qty, dir)`, taking one of the four
rounding directions. `Mul` between `Px` and `Qty` MUST NOT be implemented,
because a bare operator has nowhere to put a direction. The direction is a
compile-time constant at every call site, so this costs nothing at runtime.

This does not weaken ADR #4's dimensional analysis. A price times a quantity
still yields a `Notional` and nothing else, `Px + Qty` still does not compile,
and the multiply still carries an `i128` intermediate. What changes is the
spelling — and the call site now states whether it wants to err toward zero or
away from it, which is a different answer in the risk gate than in fill
accounting.

**2. A span carries the clock kind it came from.**

Subtracting two timestamps of the same kind yields a span tagged with that kind.
Spans of two kinds cannot be added, subtracted, or compared, and a timestamp
accepts only a span of its own kind.

A span holds nanoseconds in an `i128`, which makes the subtraction total: the
widest difference two `i64` nanosecond timestamps can have is ~1.845e19 ns, more
than an `i64` holds. Adding a span back to a timestamp is checked, because that
direction can leave the timestamp's range.

Relating two clock kinds is permitted only through an explicitly named
conversion owned by a skew model — never through span arithmetic. Principle VII
requires skew to be modelled, not assumed, and an untagged span is exactly the
tool for assuming it. No such conversion is built in #6.

**3. "Backing integers are private" forbids raw arithmetic outside `types`, not
conversion.**

`from_scaled` and `to_scaled` are one named pair and the only API in the crate
that speaks in raw scale-unit counts. All arithmetic still goes through the
newtypes, which is what ADR #4's second clause was protecting. This pair is also
the seam the event log's record format will be written through (D0.2).

## Alternatives rejected

**`Mul` truncates toward zero, documented** — keeps ADR #4's wording literally
and is what most fixed-point libraries do. Failed property: no-default rounding
(Principle VII), which this repo had just spent an ADR establishing. It installs
exactly one implicit rounding site, in the most-used money computation in the
system — the site where "it is only a nanodollar" gets said, which is the same
argument that justifies `f64` one layer up.

**`Mul` returns `Result` when the product is not exactly representable** — no
rounding anywhere and no silent loss. Failed property: ADR #4's boundary/interior
split, which exists to keep `?` off hot-path expressions. It also turns an
ordinary, correct computation into a failure the caller cannot do anything about.

**One shared span type for all clock kinds** — cheaper today, one noun instead of
a type parameter. Failed property: cheap reversal. Adding the tag later is a
viral refactor across every crate that carries elapsed time — the same shape of
migration ADR #4 warns about for `i128` — while removing it later is local. The
asymmetry decides it. The tag also turns out to describe something real: a
staleness threshold is exchange-derived, a tick-to-trade budget is monotonic, and
they are not interchangeable.

**Spans that cannot be added back to a timestamp** — the minimal surface, and it
forbids the dangerous direction outright. Failed on facts: timer events are
"wake me at current event time plus five seconds", which is exactly
`timestamp + span`.

**Whole units plus a fractional part as the only construction surface** — keeps
raw counts entirely inside `types` and honors ADR #4's wording most literally.
Failed property: an obviously-correct round-trip test. Signed decomposition is
ambiguous at the range ends — `i64::MIN` becomes `(-9_223_372_036, -854_775_808)`
and `-0.5` becomes `(0, -500_000_000)`, a negatively-signed zero-units case — so
it puts a bug surface inside the one test whose job is to be trivially right.
`to_scaled` is the identity on `i64`; there is nothing to get wrong.

**An `i64`-backed span with checked subtraction** — 8 bytes instead of 16. Failed
property: totality. Latency measurement would carry `?` for an overflow that is
unreachable in practice but not unreachable by type, and spans are locals and
constants rather than record fields, so the width costs no event density.

## Consequences

**Constrains.** Every notional computation states a rounding direction. Every
signature carrying elapsed time names a clock kind. Raw scale counts appear only
in `from_scaled` / `to_scaled`, and any other API that wants one is a review
finding.

**Costs.** One extra argument at every notional site. A type parameter on spans
that is viral across later crates. Spans are 16 bytes. Callers who genuinely need
to relate two clocks must wait for a named skew conversion rather than reaching
for subtraction.

**Enables.** The compile-fail test set can cover time the way it covers money —
`exchange - receive`, a receive-derived span applied to an exchange timestamp,
and `px * qty` are all build failures rather than review findings.

**What would make us revisit**: a call site that genuinely needs nearest-with-tie
rounding; the span tag obstructing legitimate cross-clock work often enough that
one named skew conversion is not sufficient; or a decimal-string boundary API
that makes `to_scaled` redundant outside the log format.
