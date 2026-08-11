# Architecture Decision Records

An ADR records a decision that spans more than one spec. A decision scoped to a
single feature belongs in that spec's `DECISIONS.md` instead.

## Naming

`docs/adr/<owning-issue>-<slug>.md` — issue-number naming, like specs. No
sequential numbers.

## Rule

A decision ADR merges **before** the implementation that depends on it. Writing
it afterwards turns a decision record into a description, which is worth much
less: the point is to make the alternatives visible while they are still live.

## Shape

```markdown
# <Decision title>

- **Issue**: #N
- **Status**: proposed | accepted | superseded by #M
- **Date**: YYYY-MM-DD

## Context
What forces this decision, in plain words. What breaks if we get it wrong.

## Decision
The option chosen, stated as a rule someone can follow.

## Alternatives rejected
Each one, and the specific reason it lost. "Simpler" is not a reason; name the
property it failed.

## Consequences
What this now constrains, what it costs, and what would make us revisit it.
```
