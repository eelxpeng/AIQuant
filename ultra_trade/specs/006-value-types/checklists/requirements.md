# Specification Quality Checklist: Fixed-Point Value Types And Source-Typed Timestamps

**Purpose**: Validate specification completeness and quality before proceeding to planning
**Created**: 2026-08-12
**Feature**: [spec.md](../spec.md)

## Content Quality

- [ ] No implementation details (languages, frameworks, APIs)
- [x] Focused on user value and business needs
- [ ] Written for non-technical stakeholders
- [x] All mandatory sections completed

## Requirement Completeness

- [x] No [NEEDS CLARIFICATION] markers remain
- [x] Requirements are testable and unambiguous
- [x] Success criteria are measurable
- [ ] Success criteria are technology-agnostic (no implementation details)
- [x] All acceptance scenarios are defined
- [x] Edge cases are identified
- [x] Scope is clearly bounded
- [x] Dependencies and assumptions identified

## Feature Readiness

- [x] All functional requirements have clear acceptance criteria
- [x] User scenarios cover primary flows
- [x] Feature meets measurable outcomes defined in Success Criteria
- [ ] No implementation details leak into specification

## Notes

Four items are unchecked, all deliberate and permanent for this repo. Nothing
blocks `/speckit-plan` or `/speckit-implement`.

**Deliberate — the generic item conflicts with the constitution:**

- *No implementation details* / *Written for non-technical stakeholders* /
  *No implementation details leak into specification*: this repo's spec is a
  **system contract**, not a product story (`.specify/templates/spec-template.md`,
  `specs/README.md`). The backing widths and scale (`i64`/`i64`/`i128` at `1e-9`)
  are the ratified decision of [ADR #4](../../../docs/adr/4-fixed-point-representation.md),
  and the contract is unprovable without them: TEST-004's overflow bound and
  TEST-018's size pins are statements about those exact widths. Removing them
  would reverse a merged ADR. Constitution Principle IX requires plain words, not
  the absence of technical nouns, and the spec is written to that bar.
- *Success criteria are technology-agnostic*: SC-005 names the exact gate
  commands because the constitution's merge gate and `AGENTS.md` require a PR to
  cite them. A criterion nobody can run is not a criterion here.

**Resolved:**

The spec's three `[NEEDS CLARIFICATION]` markers were genuine forks that ADR #4
did not settle, and all three changed the test set. They were answered in
`/speckit-specify` and are now recorded as decisions in
[ADR #6](../../../docs/adr/6-value-type-api-contracts.md):

- **FR-005 / FR-006** — the notional product names a rounding direction, so
  `Px * Qty` is not an operator. Added TEST-004 and compile-fail TEST-008.
- **FR-019 / FR-020** — a span carries the clock kind it came from, backed by
  `i128` so subtraction is total. Added INV-013, TEST-016, and compile-fail
  TEST-020.
- **FR-003** — `from_scaled` / `to_scaled` is the one named window onto the
  representation, chosen over signed units-plus-fraction parts because the parts
  form is ambiguous at the range ends.

FR and TEST identifiers were renumbered contiguously when these landed. The spec
had not yet been through `/speckit-plan` or `/speckit-tasks`, so nothing cited
the old numbers.
