# Contract Specification: [FEATURE NAME]

**Issue**: #[N]

**Feature Directory**: `specs/[NNN-short-name]`

**Created**: [DATE]

**Status**: Draft

**Input**: Feature description: "$ARGUMENTS"

<!--
  This is a SYSTEM CONTRACT, not a product story. It defines what the feature
  must prove before implementation starts. Every section marked *(mandatory)*
  is required by the constitution (.specify/memory/constitution.md).

  Write in plain words (Principle IX). Every domain noun used here must exist in
  CONTEXT.md with its _Avoid_ line — add it in the same PR if it does not.
-->

## Contract Scenarios & Testing *(mandatory)*

<!--
  Scenarios are prioritized and INDEPENDENTLY TESTABLE: implementing just one
  leaves the system coherent and provable. Each names the test that proves it.
-->

### S1 - [Brief Title] (Priority: P1)

[The behavior in plain language.]

**Why this priority**: [What is blocked or unprovable without it]

**Independent Test**: [The contract / golden / replay test that proves this scenario alone]

**Acceptance**:

1. **Given** [initial state], **When** [event], **Then** [observable outcome]
2. **Given** [initial state], **When** [event], **Then** [observable outcome]

---

### S2 - [Brief Title] (Priority: P2)

[As above.]

**Why this priority**:

**Independent Test**:

**Acceptance**:

1. **Given** …, **When** …, **Then** …

---

[Add scenarios as needed, each with a priority.]

### Edge Cases

<!--
  Include the degenerate cells, not just the happy path. For this domain that
  means at least: zero events, duplicate events, out-of-order events,
  disconnect mid-operation, restart mid-write, and clock skew.
-->

- What happens when [boundary condition]?
- How does the system behave when [failure]?

## Normative Contract *(mandatory)*

### Invariants

<!--
  A property that holds on EVERY run and EVERY interleaving. Numbered, so tests
  and reviews can cite them. If it can be violated without a test failing, it is
  a wish, not an invariant.
-->

- **INV-001**: [Property that must always hold]
- **INV-002**: [Property that must always hold]

### Event Examples

<!--
  Concrete records with real field values — not a schema sketch. Show the actual
  shape a consumer or a replay will see.
-->

```json
{ "seq": 1, "kind": "…", "exchange_time": "…", "…": "…" }
```

### Failure Modes

<!--
  What goes wrong, what the system does, and which state it lands in. Fail-closed
  by default (Principle V): on ambiguity, reject rather than assume.
-->

| Failure | Detection | System behavior | Resulting state |
|---|---|---|---|
| [what breaks] | [how it is noticed] | [what happens] | [state after] |

### Replay Behavior *(Principle II)*

<!--
  What must reproduce identically on replay, and what legitimately cannot — with
  the reason. Name every injected dependency (clock, RNG, ordering) this feature
  relies on.
-->

**Replayable**: [what reproduces byte-identically]

**Not replayable**: [none — or what, and why that is acceptable]

**Injected dependencies**: [clock / rng / ordering source, or "none"]

### Latency Impact *(Principle VI)*

<!--
  State the budget this touches as a percentile, or declare the feature off the
  hot path with a reason. "Probably fine" is not an entry.
-->

**Hot path**: [yes — budget p99 ≤ Xµs | no — reason]

### Risk Impact *(Principle V)*

<!--
  Which limits, gates, or accounting this interacts with. A feature that can
  influence an order has a risk surface even if it never creates one.
-->

**Risk surface**: [none — reason | the limits/gates touched, and how]

### Compatibility

<!--
  Effect on persisted records and any stable boundary: additive, breaking, or
  none. A durable record change needs its fixture-compatibility test named here.
-->

**Durable record change**: [none | additive — fixture test | breaking — migration plan]

## Requirements *(mandatory)*

### Functional Requirements

<!--
  Each requirement testable. Mark genuine ambiguity with
  [NEEDS CLARIFICATION: question] — maximum 3, resolved by /speckit-clarify.
-->

- **FR-001**: System MUST [specific, testable capability]
- **FR-002**: System MUST [specific, testable capability]

### Required Tests

<!--
  Written and observed FAILING before implementation (Principle IV). Every
  fail-closed branch and error arm needs a test that actually reaches it.
  Timing- or race-sensitive tests must state that they run repeatedly under load.
-->

- **TEST-001**: [test name] — proves [S1 / INV-001]; RED before implementation
- **TEST-002**: [test name] — proves [failure mode]; reaches the error arm

### Key Entities

- **[Entity]**: [what it represents; the CONTEXT.md term it uses]

## Success Criteria *(mandatory)*

<!--
  Measurable and implementation-agnostic. A criterion someone can run.
-->

- **SC-001**: [Measurable outcome]
- **SC-002**: [Measurable outcome]

## Assumptions

- [Assumption, and the default chosen because the description did not specify]

## Deferrals

<!--
  Every deferral links a tracking issue ON THE SAME LINE (constitution gate).
  A deferral without an issue number is rejected in review.
-->

- [Deferred item] → #[issue]
