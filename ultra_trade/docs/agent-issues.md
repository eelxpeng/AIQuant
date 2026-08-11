# Issue Reference

Read this when writing or editing a roadmap, workstream, or hub issue. The
fill-in-the-blanks form is `.github/ISSUE_TEMPLATE/workstream.md` (GitHub offers
it when you click **New issue**). This file is the *why* behind each section and
the rules that make an issue usable.

An issue is the intake for the whole flow: its number names the spec directory,
the branch, and the PR link. A vague issue produces a vague spec, and the spec is
the thing everything downstream is derived from.

## The three-line header

Every roadmap or workstream issue opens with three plain-language lines:

```text
Goal:       the measurable deliverable this advances, in one sentence
Done means: a condition someone can test
First PR:   the first concrete slice someone could open today
```

**Goal** must name a deliverable from master roadmap #1. If it cannot, this is a
parking-lot item, not a workstream — do not add it to a phase.

**Done means** must be testable by someone who did not write it. For spec-backed
work, cite the spec's Success Criteria rather than restating them; restated
criteria drift from the spec they copied.

**First PR** forces you to check the work can actually start. If you cannot name
a slice someone could open today, the issue is either blocked (say so) or not yet
understood (find out first).

### Worked example

```text
Goal: hit the Phase 0 deliverable D0.2 "replay reproduces a recorded session
      byte-identically" (#1).

Done means:
  - `cargo test -p engine --test replay` reproduces the 10k-event fixture with
    zero diff, and
  - the test fails if an ambient clock is reintroduced.

First PR: the typed event record + the append-only in-memory log, with the RED
          replay test.
```

### Counter-examples

```text
Goal: define the event log semantics
```

Names no deliverable. Either tie it to something measurable in #1, or park it.

```text
Done means: the event log works well
```

Not testable. What command proves it, and what output counts as passing?

## Two readers

Write every issue for two audiences at once:

- **Humans** skim the header and `## Why now` to judge whether the goal is real
  or hollow. Keep those short.
- **Agents** need enough context to produce a good PR with no hidden session
  history. That is what `## Agent Pickup Plan` is for, and it may be long.

## The Agent Pickup Plan

Required in every workstream issue. **Do not delete it because the issue looks
like one PR** — a one-PR issue still needs to say what to read, what to change,
what not to touch, and how to prove the result.

The bar: *two agents with no hidden context would choose roughly the same first
PR shape.*

- **Read first** — the exact issue, spec, doc, code, test, and prior PR links to
  read before editing.
- **Current facts** — status, decisions already made, blockers removed,
  dependencies. Use issue and PR numbers, never vague history like "we discussed
  this".
- **PR plan** — the expected PR order. Each slice names the files, tests, or
  artifacts it touches and the boundary where it stops.
- **Verification** — commands, CI checks, benchmark runs, and the evidence that
  belongs in the PR body.
- **Risk and latency impact** — which limits or gates it touches and which
  latency budget, or an explicit "none" with a reason. Silence here reads as "not
  considered", which is the state that lets a risk-surface change land unnoticed.
- **Do not touch / stop if** — scope limits, active branches to avoid, ambiguous
  choices that need a human, and follow-up work that should become a linked issue
  instead of growing the PR.

Cite specs and docs; do not paste them. A copied doc adds no decision and goes
stale.

## Scope items

No bare `Define X`. Every scope item names an artifact, file, or test.

```text
- [ ] Define the record shape                    ← rejected
- [ ] Add the Heartbeat variant to crates/event/src/record.rs   ← fine
```

## Deriving issues

Derive backward from deliverables — "what blocks D0.2?" — not forward from
architecture concepts. Architecture boundaries belong in `docs/ARCHITECTURE.md`,
ADRs, and the constitution, not spread across many child issues.

Do not pre-stage issues for work that cannot start yet. Keep future plans as one
section in the hub issue, and open child issues when work can begin.

## Hub issues

A phase hub holds only:

- the phase title
- the phase deliverables
- the child-workstream list
- an explicit gate line, if the phase is gated

Use a plain bullet list (`- #N title`), **not checkboxes**. A workstream's status
is its issue's open/closed state, so a manual tick goes stale the moment someone
forgets it.

Never hand-write a narrative status snapshot in a hub body. Status lives in the
child issues.

## Research handoff issues

An issue promoting a `../research/` result into `ultra_trade/` is a normal
workstream issue with two extra requirements in its pickup plan:

- **What data the strategy needs at decision time.** This is where look-ahead
  gets caught, before it becomes code.
- **Which parameters are the model, and how they are pinned.** The model crosses
  as versioned data, never as imported code (root `README.md`).

## Division of labor

Splitting work and setting priority stay human-owned. AI-written breakdowns look
complete but optimize for structure over goals, and AI is reliable mainly where
its output can be checked against something.

**Humans own**: which deliverables matter, how a phase splits into workstreams,
priority, and assignees.

**AI owns, with human approval before any GitHub write**: drafting a hub from a
one-sentence intent, keeping hub lists in sync, linting issues for measurability
and staleness, and proposing gap candidates as a list.

Never auto-create or auto-close a roadmap, workstream, or constitution issue.

## Creating one

The web UI offers the template automatically. From the CLI, name it explicitly:

```sh
gh issue create --repo eelxpeng/AIQuant --template workstream.md
```

The issue number it returns is what you pass to `/speckit-specify`.
