# Contract Specification: Fixed-Point Value Types And Source-Typed Timestamps

**Issue**: #6

**Feature Directory**: `specs/006-value-types`

**Created**: 2026-08-12

**Status**: Draft

**Input**: Feature description: "issue #6: fixed-point value types and source-typed timestamps in the types crate"

<!--
  This is a SYSTEM CONTRACT, not a product story. Every section marked
  *(mandatory)* is required by the constitution.
-->

The `types` crate owns the numbers and the clocks that every other crate spends
its life handling. This spec says what those types must prove before any of them
is written.

Two things are being bought here, and both are compile-time:

1. **A price is not a quantity, and a quantity is not money.** Mixing them is a
   build failure, not a review finding.
2. **An exchange clock is not a receive clock is not a monotonic clock.**
   Subtracting one from another is a build failure, not a latency metric.

The representation is settled by [ADR #4](../../docs/adr/4-fixed-point-representation.md)
and is not re-derived here: `Px` is `i64` @ `1e-9`, `Qty` is `i64` @ `1e-9`,
`Notional` is `i128` @ `1e-9`. The three API questions ADR #4 left unstated — the
rounding contract on the notional product, kind-tagged spans, and what "backing
integers are private" forbids — are settled by
[ADR #6](../../docs/adr/6-value-type-api-contracts.md).

## Contract Scenarios & Testing *(mandatory)*

### S1 - Exact values across the whole supported range (Priority: P1)

Every `Px`, `Qty`, and `Notional` is an exact integer multiple of one scale unit
(`1e-9`). Building a value and reading it back returns the same value, for every
value the type can hold — including the smallest non-zero step and both ends of
the range. Same-type addition, subtraction, and negation are exact integer
operations, never approximations.

**Why this priority**: Without it, nothing else in the system can be trusted to
add up. Every position, every PnL number, and every risk limit is a sum of these
values.

**Independent Test**: TEST-001, TEST-002 — property tests over each type's full
backing range, comparing against exact integer arithmetic on scale-unit counts.

**Acceptance**:

1. **Given** any value the type can represent, **When** it is constructed and
   read back, **Then** the result equals the input exactly.
2. **Given** the smallest non-zero value (`0.000000001`), **When** it is stored
   and read back, **Then** it is not zero and has not moved.
3. **Given** two values whose sum is representable, **When** they are added,
   **Then** the result equals the integer sum of their scale-unit counts.
4. **Given** any two values, **When** they are compared, **Then** the answer is
   the integer comparison of their scale-unit counts — no tolerance is involved,
   and no API accepts one.

---

### S2 - A price plus a quantity does not build (Priority: P1)

`Px`, `Qty`, and `Notional` are three separate types. The only operation that
mixes two of them is the **notional product** — `Px` and `Qty` giving a
`Notional` — and it takes a rounding direction, because a product can need more
decimal places than the scale holds. Every other mixed-type arithmetic or
comparison fails to compile, including the bare `*` operator, which has nowhere
to put a direction. So does any conversion between a value type and `f32`/`f64`,
in either direction.

**Why this priority**: This is the entire return on three newtypes instead of
one. If it is asserted by convention rather than by the compiler, the newtypes
cost complexity and buy nothing.

**Independent Test**: TEST-006 through TEST-009 — tests that assert
non-compilation, with TEST-003 and TEST-004 as the positive control that keeps
them from passing vacuously. A commented-out line or a doc note is not a test.

**Acceptance**:

1. **Given** a `Px` and a `Qty`, **When** the code adds them, **Then** the build
   fails.
2. **Given** a `Px` and a `Qty`, **When** the code compares them, **Then** the
   build fails.
3. **Given** a `Px` and a `Qty`, **When** the code writes `px * qty`, **Then**
   the build fails — a bare operator cannot name a rounding direction.
4. **Given** a `Px`, a `Qty`, and a rounding direction, **When** the code takes
   their notional product, **Then** it builds and the result is a `Notional`.
5. **Given** a float literal, **When** the code converts it to any value type —
   or a value type to a float — **Then** the build fails.

---

### S3 - Two clock kinds never meet (Priority: P1)

`ExchangeTime`, `ReceiveTime`, and `MonotonicTime` are three separate types.
Each is ordered and subtractable against itself, and subtraction yields a span
that carries the kind it came from. Any arithmetic, comparison, or conversion
across two kinds fails to compile — including mixing two spans, or adding a
span to a timestamp of a different kind.

**Why this priority**: Clock skew is the failure this prevents, and it is
invisible in testing: an exchange timestamp minus a receive timestamp produces a
plausible number that means nothing. Principle VII forbids it; only the compiler
enforces it.

**Independent Test**: TEST-017 through TEST-020 — tests that assert
non-compilation, paired with TEST-015 and TEST-016 proving the same-kind
operations do work (so the compile-fail tests are not vacuously passing on a
missing operator).

**Acceptance**:

1. **Given** an `ExchangeTime` and a `ReceiveTime`, **When** the code subtracts
   one from the other, **Then** the build fails.
2. **Given** an `ExchangeTime` and a `MonotonicTime`, **When** the code compares
   them, **Then** the build fails.
3. **Given** two `ReceiveTime` values, **When** the code subtracts them, **Then**
   it builds and yields a receive-kind span.
4. **Given** a receive-kind span and an `ExchangeTime`, **When** the code adds
   them, **Then** the build fails.
5. **Given** any timestamp value, **When** it is constructed, **Then** no ambient
   clock is read — the instant comes from the caller.

---

### S4 - Rounding states its direction or does not build (Priority: P2)

Rounding a `Px` to a tick size, or a `Qty` to a lot size, requires the direction
as an argument. There is no default direction and no single-argument rounding
entry point. A non-positive tick or lot is rejected. The same four directions
are what the notional product takes in S2.

**Why this priority**: Every value crossing a venue boundary is rounded
(Principle VII). A default direction is how a sell order silently becomes a
worse price than intended. It is P2 only because the value types must exist
first.

**Independent Test**: TEST-010 (non-compilation of a directionless call),
TEST-013 (non-positive step rejected), TEST-014 (direction table including
negative values and exact multiples).

**Acceptance**:

1. **Given** a rounding call with no direction, **When** it is compiled, **Then**
   the build fails.
2. **Given** a value already on an exact multiple of the step, **When** it is
   rounded in any direction, **Then** it is unchanged.
3. **Given** a negative value, **When** it is rounded `Down`, **Then** it moves
   toward negative infinity, not toward zero.
4. **Given** a tick or lot of zero or a negative step, **When** rounding is
   attempted, **Then** it fails with an error and produces no value.

---

### S5 - An out-of-range value cannot be introduced quietly (Priority: P2)

Every operation that can leave the representable range at a boundary —
construction from whole units, aggregation, negation of the minimum, rounding
near the ceiling, shifting a timestamp by a span — returns a `Result` carrying a
non-allocating error. Interior arithmetic uses debug-assert overflow, per ADR #4.

**Why this priority**: This is the fail-closed rule (Principle V) applied to
numbers. Saturating or wrapping arithmetic produces a wrong number that looks
like a right one.

**Independent Test**: TEST-011, TEST-012, TEST-015 — each reaching a distinct
error arm, per Principle IV.

**Acceptance**:

1. **Given** whole units whose scaled form exceeds the backing range, **When**
   construction is attempted, **Then** it returns an error and no value exists.
2. **Given** an accumulator near the `Notional` ceiling, **When** a further
   amount is added at the aggregation boundary, **Then** it returns an error and
   the accumulator is unchanged.
3. **Given** the minimum representable value, **When** it is negated, **Then**
   it returns an error rather than wrapping to itself.
4. **Given** a timestamp near the end of its representable span, **When** a span
   is added that would carry it past the end, **Then** it returns an error.

---

### S6 - `types` is the bottom of the dependency order (Priority: P3)

The workspace exists and `crates/types` depends on no other workspace crate.

**Why this priority**: Principle I is structural, and the cheapest moment to
enforce it is before there is a second crate to point at. P3 because a violation
is currently impossible — this test is the guard that keeps it impossible.

**Independent Test**: TEST-023 — an automated check of the crate's resolved
dependencies, not a reading of `Cargo.toml` by eye.

**Acceptance**:

1. **Given** the workspace, **When** `types`'s dependency graph is resolved,
   **Then** it contains no other workspace crate.

---

### Edge Cases

The degenerate cases the template asks for (zero events, duplicate events,
out-of-order events, disconnect mid-operation, restart mid-write) do not apply:
this feature has no events, no I/O, and no state. Their equivalents here are the
numeric and clock boundaries.

- **Zero**: `0` is representable in every type, has one representation only
  (integers have no negative zero), and is the identity for addition.
- **One scale unit**: `0.000000001` must survive construction, addition,
  comparison, and rounding without collapsing to zero.
- **Both ends of the range**: `Px`/`Qty` reach ±9_223_372_036.854775807
  (`i64::MIN` is one unit further negative and has no positive counterpart);
  `Notional` reaches ±1.70141183460469231731687303715884105727e29.
- **Negating the minimum**: `-i64::MIN` is not representable — a checked error,
  never a wrap back to itself.
- **Multiplying the extremes**: `Px::MIN × Qty::MIN` is the largest product the
  types can generate (~8.507e37) and must not overflow the `i128` intermediate
  (`i128::MAX` ≈ 1.701e38).
- **A product finer than the scale**: `10.0001 × 0.000001 = 0.0000100001` needs
  ten decimal places and cannot be held at `1e-9`. The call site's stated
  direction decides which neighbouring scale unit it lands on; nothing decides it
  implicitly.
- **Step larger than the value**: rounding `0.5` to a tick of `1.0` yields `0` or
  `1.0` depending on the stated direction, never an error.
- **Rounding at the ceiling**: rounding up a value within one step of the maximum
  overflows and is a checked error.
- **Subtracting the extreme timestamps**: `MAX - MIN` for a clock kind is
  ~1.845e19 ns, more than an `i64` holds. A span is `i128`, so the subtraction is
  total; it is adding a span *back* to a timestamp that is checked.
- **Clock skew**: this crate does not detect it. Two same-kind timestamps compare
  as given, including out of order or equal; ordering policy belongs to the event
  layer. What this crate guarantees is that skew across kinds cannot be computed
  at all, because the code does not compile.
- **Timestamps outside the representable window**: nanoseconds in an `i64` cover
  roughly 1678–2262. An instant outside that has no representation at all, so
  there is no construction path to check. The window is only reachable from
  inside, by adding a span — and that is checked.

## Normative Contract *(mandatory)*

### Invariants

- **INV-001**: Every value of every value type is an exact integer multiple of
  one scale unit (`1e-9`). No operation produces a value between two scale units,
  and construction followed by inspection is the identity over the entire backing
  range.
- **INV-002**: No conversion exists between any value type and `f32` or `f64`, in
  either direction, anywhere in the crate's public surface (Principle VII).
- **INV-003**: The only operation mixing two distinct value types is the notional
  product of a `Px` and a `Qty`. Every other mixed-type arithmetic, assignment,
  or comparison fails to compile — including `px * qty`, which does not exist as
  an operator because it has nowhere to name a rounding direction.
- **INV-004**: The notional product cannot overflow for any pair of representable
  operands. The largest possible product of two `i64` scale-unit counts is
  ~8.507e37, which fits the `i128` intermediate with better than 2× headroom, and
  scaling back down only shrinks it.
- **INV-005**: No operation applies a rounding direction the call site did not
  name — this covers tick and lot rounding and the notional product alike.
  Rounding a value that is already an exact multiple of the step is the identity,
  in every direction.
- **INV-006**: Every operation that can leave the representable range at a
  boundary returns `Result`. No boundary operation panics on in-range input.
- **INV-007**: Values of two different clock kinds cannot be added, subtracted,
  compared, or converted. Each kind is ordered only against itself. A span
  carries the kind of the timestamps that produced it and cannot be mixed with a
  span, or applied to a timestamp, of any other kind.
- **INV-008**: Every operation is a pure function of its arguments — no ambient
  clock, no RNG, no global or thread-local state, no I/O. The same inputs give
  the same outputs on every run, every thread, and every supported platform.
- **INV-009**: No operation allocates, locks, formats, or performs a syscall, and
  no error value carries heap-allocated data (Principle VI).
- **INV-010**: Equality and ordering on a value type are exactly equality and
  ordering of scale-unit counts. No API anywhere accepts a tolerance or an
  epsilon.
- **INV-011**: `types` depends on no other workspace crate (Principle I).
- **INV-012**: `Px` and `Qty` occupy 8 bytes each and `Notional` 16. ADR #4's
  rejection of "`i128` for everything" is only valid while this holds.
- **INV-013**: Subtracting two timestamps of the same kind cannot overflow. The
  widest possible difference is ~1.845e19 ns, and a span is `i128`.

### Event Examples

This feature emits and consumes no events — it has no record format and gains no
serialization in this issue (that is D0.2). The equivalent concrete artifact is
the value tables below: these are the literal test vectors, and every later event
record carries values of exactly these types.

**Values and their exact scale-unit counts**

| Meaning | Value | Scale-unit count | Type |
|---|---|---|---|
| Ordinary equity price | `123.45` | `123_450_000_000` | `Px` |
| Sub-dollar tick (SEC Rule 612) | `0.0001` | `100_000` | `Px` |
| Smallest representable step | `0.000000001` | `1` | `Px` |
| BRK.A-scale price | `700000.00` | `700_000_000_000_000` | `Px` |
| Maximum price | `9223372036.854775807` | `9_223_372_036_854_775_807` | `Px` |
| One share | `1` | `1_000_000_000` | `Qty` |
| Fractional share | `0.000001` | `1_000` | `Qty` |
| `123.45 × 100` | `12345.00` | `12_345_000_000_000` | `Notional` |
| `700000.00 × 10` | `7000000.00` | `7_000_000_000_000_000` | `Notional` |

**Notional product vectors**

| `Px` | `Qty` | Direction | `Notional` | Note |
|---|---|---|---|---|
| `123.45` | `100` | any | `12345.000000000` | exact; the direction is not consulted |
| `10.0001` | `0.000001` | `TowardZero` | `0.000010000` | exact value is `0.0000100001` |
| `10.0001` | `0.000001` | `AwayFromZero` | `0.000010001` | |
| `10.0001` | `0.000001` | `Down` | `0.000010000` | |
| `-10.0001` | `0.000001` | `Down` | `-0.000010001` | toward −∞, not toward zero |
| `-10.0001` | `0.000001` | `TowardZero` | `-0.000010000` | |

**Tick rounding vectors** (tick `0.01`)

| Input | Direction | Result |
|---|---|---|
| `10.004` | `Down` | `10.00` |
| `10.004` | `Up` | `10.01` |
| `10.010` | any | `10.01` (already on a tick) |
| `-10.004` | `Down` | `-10.01` (toward −∞) |
| `-10.004` | `TowardZero` | `-10.00` |
| `-10.004` | `AwayFromZero` | `-10.01` |
| any | step `0` or negative | error, no value produced |

**Timestamp vectors** (nanoseconds; exchange and receive are since the Unix
epoch, monotonic is since an opaque process-local origin)

| Kind | Instant | Nanosecond count |
|---|---|---|
| `ExchangeTime` | `2026-08-12T13:45:30.123456789Z` | `1_786_542_330_123_456_789` |
| `ReceiveTime` | `2026-08-12T13:45:30.124011300Z` | `1_786_542_330_124_011_300` |
| `MonotonicTime` | opaque origin + 42.5 s | `42_500_000_000` |

The pairing above is the trap this feature closes: those two instants are
554_511 ns apart and that subtraction **must not compile**. It is exchange time
minus receive time, which is a number with no meaning.

### Failure Modes

| Failure | Detection | System behavior | Resulting state |
|---|---|---|---|
| Whole units whose scaled form exceeds the backing range | checked constructor | returns an out-of-range error | no value is created; the caller decides |
| `Notional` aggregation passes the `i128` ceiling | checked add at the aggregation boundary | returns an overflow error | accumulator unchanged |
| Negating the minimum representable value | checked negate | returns an out-of-range error | value unchanged |
| Rounding up within one step of the ceiling | checked round | returns an overflow error | value unchanged |
| Tick or lot size of zero or negative | rounding precondition | returns an invalid-step error, rounds nothing | fail-closed: no rounded value exists (Principle V) |
| Notional product needs more than nine decimal places | not an error | resolved by the direction the call site named | a `Notional` one scale unit away from the exact product, in a direction the caller chose |
| Interior arithmetic overflows, debug build | `debug_assert` | panics at the operation | the run stops loudly; the test fails |
| Interior arithmetic overflows, release build | not detected | wraps silently — accepted by ADR #4 | a wrong value. Mitigated, not eliminated: boundary checks keep operands in range, and INV-004 and INV-013 make the two widening operations total. Recorded here so the residual risk is visible |
| Instant outside the representable nanosecond window (≈1678–2262) | the representation | there is no construction path to check — the type cannot hold it | the value never exists |
| Adding a span that carries a timestamp past the end of that window | checked add | returns an out-of-range error | timestamp unchanged |
| Two clock kinds combined, or two span kinds mixed | the compiler | the build fails | the code never runs |
| Timestamps arrive equal, duplicated, or out of order | not this crate's concern | values compare as given | ordering policy belongs to the event layer, not to a value type |

### Replay Behavior *(Principle II)*

**Replayable**: everything. Every operation is a pure function of its arguments,
so the same inputs produce byte-identical outputs on every run.

**Not replayable**: none.

**Injected dependencies**: none — and specifically **no clock**. The three
timestamp types are values that events carry; nothing in `types` reads an
ambient clock, and no constructor calls `SystemTime::now()` or `Instant::now()`.
This is what makes seam 4 work: a strategy reads the timestamp of the event in
hand, never a clock.

### Latency Impact *(Principle VI)*

**Hot path**: yes — these types *are* the hot path's arithmetic. No numeric
budget exists yet; the tick-to-trade budget is set by D4.2 (→ #1), so no
benchmark is required for this issue.

The constraints that do bind now, and that a PR must state it satisfies:

- No allocation in any operation, and no `String` on any error path (INV-009).
- The `i128` intermediate on every notional product (ADR #4).
- `Px` and `Qty` stay 8 bytes (INV-012). A top-of-book record with two prices and
  two sizes is 32 bytes; widening either value type doubles it and halves L1
  density on every event of every day.
- A rounding direction is a compile-time constant at every call site, so the
  explicit-direction API costs nothing at runtime.
- A span is 16 bytes. Spans are locals and constants, not record fields, so the
  width buys totality (INV-013) without touching event density.

### Risk Impact *(Principle V)*

**Risk surface**: none. This feature has no orders, no positions, no limits, and
no venue contact — nothing here can reach a market.

That is a statement about this issue, not about the blast radius. Every value in
every later order, position, and accounting path is one of these types, so a
defect here is a correctness fault everywhere downstream and one that arithmetic
will not make obvious. That is why the contract is proven by property tests over
full ranges and by compile-fail tests, rather than by a handful of examples.

### Compatibility

**Durable record change**: none — this issue adds no serialization and no `serde`
dependency.

It does fix the numbers that the durable format will have to carry, and
`to_scaled` is the seam that format will be written through. ADR #4 requires
event log records to carry a format version from the first release, so that the
eventual `i128`/crypto migration is a supported operation rather than a break.
That requirement lands with the event log (D0.2 → #1); this spec records that it
is owed.

## Requirements *(mandatory)*

### Functional Requirements

**Value types**

- **FR-001**: System MUST provide `Px`, `Qty`, and `Notional` as three distinct
  types with the backing widths and scale in ADR #4 — `i64`, `i64`, `i128`, all
  at `1e-9`. The backing integer MUST NOT be a public field of any of them.
- **FR-002**: Every value MUST be an exact integer multiple of one scale unit,
  and no operation may produce a value between two scale units.
- **FR-003**: System MUST provide exactly one named conversion pair between a
  value and its scale-unit count — `from_scaled` and `to_scaled` — which round
  trips exactly over each type's whole backing range. This pair is the only
  window onto the representation; no other API may expose or accept a raw count
  (ADR #6).
- **FR-004**: Addition, subtraction, and negation MUST be defined within a single
  type only (`Px + Px`, `Qty + Qty`, `Notional + Notional`) and MUST be exact.
- **FR-005**: System MUST provide a notional product of a `Px` and a `Qty`
  yielding a `Notional`, computed through an `i128` intermediate, taking a
  rounding direction as a required argument.
- **FR-006**: The `*` operator MUST NOT be implemented between `Px` and `Qty`. A
  product whose exact value needs more than nine decimal places MUST land on the
  neighbouring scale unit the call site's direction names, and MUST NOT be
  rounded implicitly (ADR #6).
- **FR-007**: No operation mixing two of the three value types may exist other
  than FR-005. Every other mixed combination MUST fail to compile.
- **FR-008**: No conversion between a value type and `f32`/`f64` may exist in
  either direction.
- **FR-009**: Comparison and equality MUST be exact integer comparison of
  scale-unit counts, and no API may accept a tolerance or epsilon.

**Boundaries and overflow**

- **FR-010**: Every operation that can leave the representable range at a
  boundary — construction, aggregation, negation, rounding, adding a span to a
  timestamp — MUST return a `Result`.
- **FR-011**: Interior arithmetic MUST use debug-assert overflow rather than
  checked arithmetic, per ADR #4, so `?` stays off every hot-path expression.
- **FR-012**: Error values MUST NOT carry heap-allocated data — no `String`, no
  boxed error, no formatting on the error path.

**Rounding**

- **FR-013**: Rounding a `Px` to a tick size or a `Qty` to a lot size MUST take
  the direction as a required argument. No default direction and no
  single-argument rounding entry point may exist.
- **FR-014**: The available directions MUST be `Up` (toward +∞), `Down` (toward
  −∞), `TowardZero`, and `AwayFromZero`, each defined for negative values, and
  MUST be the same set the notional product accepts.
- **FR-015**: Rounding MUST reject a zero or negative step with an error and
  produce no value.
- **FR-016**: Rounding a value that is already an exact multiple of the step MUST
  return it unchanged, in every direction.

**Timestamps and spans**

- **FR-017**: System MUST provide `ExchangeTime`, `ReceiveTime`, and
  `MonotonicTime` as three distinct types at nanosecond resolution.
- **FR-018**: Values of two different clock kinds MUST NOT be addable,
  subtractable, comparable, or convertible into one another; each illegal
  combination MUST fail to compile.
- **FR-019**: Each kind MUST be ordered against itself, and subtracting two
  timestamps of the same kind MUST yield a span tagged with that kind. The span
  MUST hold nanoseconds in an `i128` so that the subtraction is total (ADR #6).
- **FR-020**: Spans of two different kinds MUST NOT be addable, subtractable, or
  comparable, and a timestamp MUST accept only a span of its own kind. Adding a
  span to a timestamp MUST be checked.
- **FR-021**: A span MUST be constructible directly from a nanosecond count with
  its clock kind named at the call site, so a configured threshold or budget
  states which clock it is measured against.
- **FR-022**: No constructor or operation on a timestamp or span type may read an
  ambient clock. `SystemTime::now()` and `Instant::now()` MUST NOT appear in the
  crate.

**Crate structure**

- **FR-023**: `crates/types` MUST depend on no other workspace crate.
- **FR-024**: No operation may allocate, lock, format, or make a syscall.

### Required Tests

Written and observed FAILING before implementation (Principle IV). Compile-fail
tests need a mechanism that asserts non-compilation; a commented-out line is not
a test.

**Value arithmetic**

- **TEST-001**: `roundtrip_is_exact_over_full_range` — property test over each
  type's whole backing range including `0`, `±1` scale unit, and both extremes;
  proves S1 / INV-001; RED before implementation.
- **TEST-002**: `same_type_arithmetic_matches_integer_arithmetic` — property test
  that addition, subtraction, and negation on non-overflowing inputs equal the
  integer operation on scale-unit counts; proves S1 / INV-001.
- **TEST-003**: `notional_product_is_exact_when_representable` — property test
  that when the exact product is a multiple of one scale unit, the result equals
  it and is identical for all four directions; proves S2 / FR-005.
- **TEST-004**: `notional_product_rounds_in_the_named_direction` — the notional
  product vectors, including the negative cases; proves FR-006 / INV-005.
- **TEST-005**: `notional_product_never_overflows` — property test over all `i64`
  pairs including both minima; proves INV-004; must not panic and must land
  inside `Notional`'s range.

**Compile-fail: value types**

- **TEST-006**: compile-fail `px_plus_qty` — proves S2 / INV-003.
- **TEST-007**: compile-fail `px_compared_to_qty` — proves S2 / INV-003; covers
  comparison, which addition does not.
- **TEST-008**: compile-fail `px_times_qty_operator` — proves FR-006: the bare
  `*` does not exist between `Px` and `Qty`.
- **TEST-009**: compile-fail `float_conversion_either_direction` — proves INV-002;
  covers both float-to-value and value-to-float.
- **TEST-010**: compile-fail `round_without_direction` — proves S4 / INV-005.

**Error arms**

- **TEST-011**: `construction_out_of_range_is_rejected` — reaches the
  out-of-range error arm for each value type.
- **TEST-012**: `aggregation_overflow_is_rejected` — accumulates `Notional` to the
  `i128` ceiling and reaches the overflow error arm; also covers negation of the
  minimum.
- **TEST-013**: `non_positive_step_is_rejected` — reaches the invalid-step error
  arm for both tick and lot rounding; proves the fail-closed branch in S4.
- **TEST-014**: `rounding_direction_table` — the tick rounding vectors, including
  negative values, exact multiples (idempotence, INV-005), and rounding up within
  one step of the ceiling (overflow error).

**Timestamps and spans**

- **TEST-015**: `same_kind_timestamp_operations` — ordering and subtraction work
  within each kind, and adding a span that carries a timestamp past the
  representable span returns an error. Also the positive control that keeps
  TEST-017…020 from passing vacuously.
- **TEST-016**: `span_subtraction_never_overflows` — property test including
  `MAX - MIN` for each kind; proves INV-013.
- **TEST-017**: compile-fail `exchange_minus_receive` — proves S3 / INV-007.
- **TEST-018**: compile-fail `exchange_compared_to_monotonic` — proves S3 /
  INV-007.
- **TEST-019**: compile-fail `monotonic_converted_to_exchange` — proves S3 /
  INV-007; covers conversion, which arithmetic does not.
- **TEST-020**: compile-fail `span_kinds_mixed` — a receive-kind span added to an
  `ExchangeTime`, and two spans of different kinds compared; proves FR-020.

**Structural**

- **TEST-021**: `no_operation_allocates` — a counting allocator observes zero
  allocations across the full operation surface; proves INV-009.
- **TEST-022**: `value_type_sizes_are_pinned` — `Px` and `Qty` are 8 bytes,
  `Notional` is 16; proves INV-012.
- **TEST-023**: `types_has_no_workspace_dependency` — resolved dependency graph
  contains no other workspace crate; proves S6 / INV-011.

### Key Entities

- **Px**: a fixed-point price in the instrument's quoting convention
  (`CONTEXT.md` → Px).
- **Qty**: a fixed-point quantity in the instrument's lot convention
  (`CONTEXT.md` → Qty).
- **Notional**: the money value of a price times a quantity (`CONTEXT.md` →
  Notional).
- **Scale Unit**: the `1e-9` step every value is an exact multiple of
  (`CONTEXT.md` → Scale Unit).
- **Rounding Direction**: the named direction a value is moved when it is
  rounded to a tick or lot, or when a notional product does not land on a scale
  unit (`CONTEXT.md` → Rounding Direction).
- **Exchange Time**: the timestamp the venue assigned (`CONTEXT.md` → Exchange
  Time).
- **Receive Time**: the local instant the process observed an event
  (`CONTEXT.md` → Receive Time).
- **Monotonic Time**: the injected local monotonic clock's reading (`CONTEXT.md`
  → Monotonic Time).
- **Span**: the elapsed nanoseconds between two timestamps of one clock kind,
  carrying that kind (`CONTEXT.md` → Span).

## Success Criteria *(mandatory)*

- **SC-001**: `cargo test -p types` passes with every test in Required Tests
  present, and the PR body shows the same suite RED before implementation with
  the reason each test failed.
- **SC-002**: Every illegal operation named in INV-002, INV-003, INV-005, and
  INV-007 has a test that asserts it does not build. Deleting any single guard
  from the implementation makes exactly that test fail.
- **SC-003**: The property tests cover each type's whole range — both extremes,
  zero, and the single-scale-unit step are generated cases, not assumed ones —
  with at least 1,000 cases per property.
- **SC-004**: No floating-point type appears in the crate's public surface, and
  no operation allocates, proven by TEST-009 and TEST-021 rather than by review.
- **SC-005**: The full gate passes offline with no credentials and no network:
  `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
  `cargo test --workspace`.
- **SC-006**: No public path can produce a value outside the representable range:
  every such path either returns `Result` or is total by construction, and the
  PR names any operation that allocates (expected: none).

## Assumptions

- The scale is `1e-9` for all three types and the backing widths are ADR #4's.
  Not re-derived, not re-opened. If implementation shows the table is wrong, that
  is an ADR amendment, not a mid-implementation judgment call.
- Timestamps are nanosecond counts in an `i64`: exchange and receive since the
  Unix epoch, monotonic since an opaque process-local origin whose absolute value
  is meaningless. The ≈1678–2262 span this affords is accepted.
- The rounding directions are `Up`, `Down`, `TowardZero`, `AwayFromZero`. No
  nearest-with-tie mode: no call site needs one yet, and a tie policy chosen in
  advance is speculative configuration.
- `Qty` is signed, per ADR #4's table. `CONTEXT.md`'s preference for an unsigned
  quantity plus an explicit `Side` is a usage convention enforced where `Side`
  exists, not a property of this type.
- Errors are a small non-allocating enum. No `String`, no boxed error, no
  `Display` formatting on the error path.
- No `serde`, no decimal-string parsing, and no `Display`/`FromStr` in this
  issue. The durable representation belongs to the event log.
- The mechanism that asserts non-compilation is an implementation choice
  (`trybuild` is conventional). This spec requires the assertion, not the crate.
- The workspace `Cargo.toml` and the `crates/types` skeleton land in this issue
  because nothing else can exist without them, not because packaging is in scope.

## Deferrals

No issue exists yet for the deliverables below; the master roadmap owns them
until one is filed, and this repo does not auto-create issues.

- Decimal string parsing, formatting, and serialization → #1 (D0.2, event log)
- Event record format versioning required by ADR #4 → #1 (D0.2)
- Division (`Notional / Qty -> Px`, average fill price) and scalar multiplication → #1
- Nearest-with-tie rounding mode, if a call site ever needs one → #1
- The named cross-clock conversion a skew model would own — the only sanctioned
  way to relate two clock kinds (ADR #6) → #1
- Tick-to-trade latency budget and benchmarks for these operations → #1 (D4.2)
- CI enforcement of the no-float and no-allocation checks → #1 (D0.4)
- `Instrument`, `Side`, and id types → #1
- The crypto-range representation change (`i128`, per-instrument scale, or a
  decimal type), whose cost is recorded in ADR #4 → #1
