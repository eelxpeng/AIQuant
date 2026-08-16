# Agent Instructions

This workspace is `ultra_trade` — the real-time quantitative trading system.
`CLAUDE.md` is a symlink to this file, so these rules are shared by every agent.

## Instruction Hygiene

- Keep this file short and high-signal. Hard budget: 150 lines.
- Add a root rule only if it is frequent, costly when missed, hard to infer, and stable.
- Move long or task-specific procedures to `.agents/skills/` or `docs/`; keep only a trigger pointer here.
- Cite the issue number next to a rule. A rule that cannot name the failure it prevents is deleted at the next cleanup.
- When adding a rule, remove or compress a stale rule in the same area.

## Start Here

- Treat this repository as self-contained. Do NOT read, import, or depend on `../research/` for implementation guidance.
- Research results enter this repo through a **spec**, never by copying research code. Research is exploratory and unbounded; this repo is real-money and latency-bounded. A model that works in a notebook is a hypothesis, not a design.
- Read `.specify/memory/constitution.md` before behavior, architecture, risk, or test changes.
- Read `docs/ARCHITECTURE.md` before changing a crate boundary or adding a seam.
- Read `CONTEXT.md` before introducing a domain noun. If your term is not there, add it (with its `_Avoid_:` line) in the same PR.
- Repo is `eelxpeng/AIQuant`; this project is its `ultra_trade/` subdirectory. `gh` targets that repo, and CI path filters and issue templates are relative to the repo root one level up.
- Read `STATUS.md` before picking up work: it is the checkable record of what is built and what is next. Update it in the same PR as the change it describes, citing the PR number. **Priority and scope splits still live in GitHub issues** — roadmap **#1** and its phase hubs — because those are the human's to set; `STATUS.md` records state, not plans.
- Start implementation from a GitHub issue and its owning spec. If no spec owns the behavior, update the spec first or state the scope exception explicitly.
- Writing or editing an issue (workstream, hub, roadmap): read `docs/agent-issues.md`; the form is `.github/ISSUE_TEMPLATE/workstream.md`. Never auto-create or auto-close a roadmap, workstream, or constitution issue — humans own scope splits and priority.

## Core Behavior

- Answer first, then add only needed detail. Cut filler and restated content.
- Prefer the simplest spec-backed implementation. No speculative config, abstractions, modes, or extension points.
- Keep edits surgical: every changed line traces to the request, a spec task, a failing test, or cleanup your own change caused.
- For behavior changes, write or identify the failing test FIRST unless the task is docs-only.
- Claim "fixed" or "verified" only when the same check failed before your change and passes after it.
- Surface the choice before changing any of: a risk limit, order lifecycle, event schema, position accounting, or anything on the hot path.
- Never widen a risk limit, disable a guard, loosen a tolerance, or add a bypass to make a test pass. Fix the test or fix the design.
- Report what you did not run. A skipped check is a finding, not a detail.

## Money, Time, And Numbers

The domain foot-guns. These cost real money when missed.

- Money and quantities are fixed-point (`Px` / `Qty`), never `f32`/`f64`. A raw float in an order or accounting path is a blocking review finding.
- Every price and quantity crossing a venue boundary is explicitly rounded to that venue's tick/lot size, with the rounding direction stated in code.
- Timestamps carry their source. Exchange time, receive time, and local monotonic are three different clocks and are never interchangeable or subtracted across kinds.
- Never call `SystemTime::now()` on the engine path. Take the injected clock, or replay stops being deterministic.
- Comparisons on prices are exact, never epsilon-based. If you need a tolerance, the spec must say why.

## Hot Path

Hot path = market-data event in → order out.

- On the hot path: no allocation, no lock, no syscall, no formatting logger, no `unwrap` on external input, no unbounded loop.
- Every hot-path change states its latency-budget impact in the PR body and cites a benchmark run.
- Slow work goes behind the hot path as an explicit background task, never inline "just this once".
- Backpressure is a design decision, never an accident: name what is dropped, queued, or blocked when a consumer falls behind.

## Verification

- Focused tests for the change, then the full gate before opening or updating a PR:
  `cargo fmt --check` · `cargo clippy --all-targets -- -D warnings` · `cargo test --workspace`
- Put exact commands and results in the PR test plan.
- Determinism is a test, not a hope. An engine-path change must show the replay test still reproduces byte-identical output.
- No live venue, live credentials, or real network in default tests. Live smokes are opt-in and gated behind an explicit env flag.
- A test whose result depends on timing, concurrency, or a race is not proven by one green run. Run it repeatedly under load and cite the clean streak.

## Git And PR Safety

- A PR is the smallest mergeable unit. For every finding during a PR's life ask: does THIS merge require it? No → follow-up issue plus a separate PR.
- Implementation PR bodies carry a "Design deltas" section versus the intake issue ("none" is fine and must be stated).
- Never force-push a shared branch. Before any force-push, verify `git log --oneline <base>..HEAD` shows exactly the intended commits.
- Never commit credentials, API keys, venue endpoints, account identifiers, or captured production market data.

## Spec Kit

- Flow: `/speckit-specify` → `/speckit-clarify` → `/speckit-plan` → `/speckit-tasks` → `/speckit-analyze` → `/speckit-implement`.
- Feature directories are `specs/<issue-number>-<short-name>/`, numbered by the GitHub issue. **This overrides the `/speckit-specify` default and the `feature_numbering: sequential` setting in `.specify/init-options.json`** — pass the issue explicitly: `.specify/scripts/bash/create-new-feature.sh --number <issue> --short-name <name> "<description>"`. Issue numbers cannot collide between parallel agents; sequential ones can. Spec Kit zero-pads under 100 (issue #2 → `specs/002-event-log/`); that is expected, and the number is still the issue.
- Specs must include invariants, event examples, failure modes, replay behavior, and mandatory RED tests. Full shape in `specs/README.md`.
- Before coding, name the issue, owning spec, scenario, invariant, required tests, and task IDs.
- Do not tick a spec task unless every noun and verb in it is implemented. Leave partial tasks open with a NOTE on the landed subset.

## Knowledge Placement

- Short, stable, repo-wide rules → this file.
- Long procedures → `.agents/skills/<name>/SKILL.md` or `docs/`, with a trigger pointer here.
- Design decisions → the owning spec's `DECISIONS.md`, or `docs/adr/<issue>-<slug>.md` for cross-cutting ones.
- Correctness-critical invariants live in this file, `docs/`, the constitution, a spec, or a test — never only in a chat transcript.
