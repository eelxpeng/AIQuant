# ultra_trade Feature Specs

A spec is the contract between architecture and pull requests. It defines what a
feature must prove *before* implementation starts.

## Directory naming

`specs/<issue-number>-<short-name>/` — after the feature's GitHub issue number.

Issue numbers are globally unique, so two agents working in parallel can never
collide on a spec id. Sequential numbers can, and they carry no link back to the
work item.

Spec Kit defaults to sequential numbering, so the issue number must be passed
explicitly:

```sh
.specify/scripts/bash/create-new-feature.sh \
  --number <issue> --short-name <short-name> "<description>"
```

Numbers under 100 are zero-padded (issue #2 → `specs/002-event-log/`). That is
expected; the number is still the issue number.

## Required spec shape

Every spec MUST include:

- **Scenarios** — numbered (`S1`, `S2`), each one an observable behavior.
- **Invariants** — numbered (`INV-001`), each one a property that must hold on
  every run and every interleaving.
- **Event examples** — concrete records the feature emits or consumes, with real
  field values. Not a schema sketch.
- **Failure modes** — what goes wrong, what the system does, and which state it
  lands in. Include the degenerate cases: zero events, duplicate events,
  out-of-order events, disconnect mid-order, restart mid-fill.
- **Replay behavior** — what this feature must reproduce identically on replay,
  and what (if anything) is legitimately not replayable and why.
- **Latency impact** — the budget this touches, or an explicit "hot path: no"
  with a reason.
- **Risk impact** — which limits or gates this interacts with, or an explicit
  "risk surface: none" with a reason.
- **Required tests** — numbered (`TEST-001`), written and observed RED before
  implementation.

For features with durable state, resume, or reconnection, also enumerate the
**states × operations matrix**, including the degenerate cells, and a
**kill-window matrix**: what happens if the process dies before the first fill,
between fills, after a fill but before the log flush, and during reconciliation.

## What a PR cites

Every implementation PR body names:

- **Spec**: `specs/<issue>-<name>/`
- **Scenarios**: the scenario IDs it implements
- **Required tests**: the test IDs it adds or turns green
- **Tasks**: the task IDs it completes
- **Design deltas**: how the implementation differs from the intake issue
  ("none" is acceptable and must be stated)
- **Out of scope**: spec scenarios intentionally left for later PRs, each with a
  tracking issue

## Preferred PR sequence

Keeping these separable keeps design review, test review, and implementation
review separable too:

1. **Spec PR** — the contract only.
2. **RED-test PR** — failing contract tests and fixtures.
3. **Implementation PR** — makes a narrow scenario pass.
4. **Compat/perf PR** — replay fixtures, benchmark gates.

## Deferrals

Every `out of scope`, `deferred`, or `TODO` line in a spec MUST link a tracking
issue on the same line. A deferral without an issue number is rejected in
review — that is how deferred work becomes forgotten work.

## Active specs

<!-- Add one line per spec as it lands. -->

_(none yet)_
