---
name: Workstream
about: A phase child issue tied to a measurable deliverable
title: ''
labels: ''
assignees: ''
---

Goal: <which measurable deliverable this advances — one sentence; it must trace to a deliverable in master roadmap #1>

Done means: <a condition someone can test — one line; use a short bullet list only when one line is not enough, and keep every bullet testable; for spec-backed work, cite the spec's success criteria instead of restating them>

First PR: <the first concrete slice someone could open today>

<!-- Write for two readers:
- Humans need short sections they can skim to judge whether the goal is real.
- Agents need enough pickup context to produce a similar-quality PR with no
  hidden chat context.
Humans can skip the Agent Pickup Plan. Do not delete it just because the issue
looks like one PR. -->

<!-- Filled-in example:

Goal: hit the Phase 0 deliverable D0.2 "replay reproduces a recorded session byte-identically" (#1).

Done means:
- `cargo test -p engine --test replay` reproduces the 10k-event fixture with zero diff, and
- the test fails if an ambient clock is reintroduced.

First PR: the typed event record + the append-only in-memory log, with the RED replay test. -->

## Why now

<one short paragraph: what stays broken or blocked without this>

## Scope

- [ ] <task naming an artifact, file, or test — no bare "Define X">

## Agent Pickup Plan

<!-- This section is for agents, not skim readers. Make it concrete enough that
two agents with no hidden context would choose roughly the same first PR shape.
Cite specs and docs instead of pasting them. -->

### Read first

- <exact issue / spec / doc / code / test links to read before editing>

### Current facts

- <known status, decisions already made, blockers removed, dependencies — use issue and PR numbers, not vague history>

### PR plan

1. <first slice: the files, tests, or artifacts it touches, and where it stops>
2. <next slice, or delete>

### Verification

- <exact commands, CI checks, benchmark runs, and the evidence that belongs in the PR body>

### Risk and latency impact

- <which limits/gates this touches, and which latency budget — or an explicit "none" with a reason>

### Do not touch / stop if

- <scope limits, active branches to avoid, and choices that need a human decision>

## Non-Goals

<only if a likely scope creep needs naming; otherwise delete>

## Links

- Hub: #<phase hub issue>
- Depends on: #<issues, or "none">
- Spec: `specs/<issue>-<name>/` (or "to be created by this issue")
- Docs: <cite, do not restate>
