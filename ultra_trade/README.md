# ultra_trade

Real-time quantitative trading system, in Rust.

This repository is the production trading system. It is developed spec-first
with coding agents, under the rules in `AGENTS.md` and
`.specify/memory/constitution.md`.

Its sibling `../research/` is the exploratory side — models, evals, notebooks.
**Research results enter this repo through a spec, never by copying code.** A
model that works in a notebook is a hypothesis, not a design.

## Layout

| Path | Holds |
|---|---|
| `AGENTS.md` | always-loaded agent rules (`CLAUDE.md` symlinks here) |
| `.specify/memory/constitution.md` | the durable principles and merge gates |
| `docs/ARCHITECTURE.md` | crate boundaries and load-bearing seams |
| `docs/adr/` | cross-cutting decision records |
| `CONTEXT.md` | domain glossary — one meaning per term |
| `specs/<issue>-<name>/` | per-feature behavior contracts |
| `.agents/skills/` | agent procedures (`.claude` symlinks here) |
| `STATUS.md` | what is built, what is not, and where the work is up to |
| `docs/SYSTEM-MAP.md` | how the pieces fit together, with diagrams |

`STATUS.md` tracks what is built and what is next. Priority and scope splits
live in GitHub issues — roadmap #1 and its phase hubs.

This project sits inside the `ai_quant` monorepo. Issue templates and CI live at
the repo root (`../.github/`), because GitHub only reads them from there.

**Open Claude Code sessions in this directory, not at the repo root** — agent
instructions load from the working directory upward, so a root-level session
would miss `AGENTS.md` entirely.

## Spec-driven development

Intended for features large or risky enough to be worth designing on paper
first, using [GitHub Spec Kit](https://github.com/github/spec-kit).

**In practice most of the system so far was built without it.** The full
specify/plan/tasks cycle was too slow for the core build, and those PRs say so
in a "Design deltas" section instead. What survived from the discipline, and is
not optional, is the part that catches mistakes: a **decision record before the
code it decides** (`docs/adr/`), and a **failing test before the behaviour**.
The crash-recovery contract was written that way and was worth every line.

Use the full cycle when a change is hard to reverse. Say so in the PR when you
skip it.

```bash
uv tool install specify-cli --from git+https://github.com/github/spec-kit.git
specify init --here --ai claude
```

Then, per feature:

```
/speckit-specify    # the what and why
/speckit-clarify    # resolve ambiguities (optional)
/speckit-plan       # implementation plan + contracts
/speckit-tasks      # dependency-ordered, test-first tasks
/speckit-analyze    # cross-artifact consistency (optional)
/speckit-implement  # execute; contract tests fail first, then pass
```

## The non-negotiables

Read the constitution for the full set. The four that block the most damage:

1. **Deterministic replay** — same input events → byte-identical orders. No ambient clock, RNG, or iteration order on an order path.
2. **Backtest/live parity** — one code path; modes differ only in which adapters are bound. No `if backtest` in engine or strategy code, ever.
3. **Fail-closed risk** — every order goes through the risk gate; ambiguity rejects; the kill switch works from every state.
4. **Spec-first, test-first** — a behavior without a spec and a failing test does not get built.

## Verification

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace          # must pass offline, with no credentials present
```
