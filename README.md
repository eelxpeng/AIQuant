# ai_quant

Monorepo for the quantitative trading work. Two projects with deliberately
different rules.

| Project | What it is | How it is governed |
|---|---|---|
| [`ultra_trade/`](ultra_trade/) | The real-time trading system (Rust). Real money, latency-bounded. | Spec-driven and strict. See `ultra_trade/AGENTS.md` and `ultra_trade/.specify/memory/constitution.md`. |
| [`research/`](research/) | Model development, evals, notebooks. Exploratory. | Free-style. No spec gate. |

They live in one repo so a model artifact and the spec that consumes it can land
in the same commit, and so one issue number tracks a finding from research
through to production.

## The boundary between them

**Research results enter `ultra_trade/` through a spec, never by copying code.**

A model that works in a notebook is a hypothesis, not a design. What crosses the
line is *data* — parameters, weights, a rule set — versioned and pinned, loaded
by a strategy. Never an import from `research/`. `ultra_trade/AGENTS.md` states
this as a root rule and `ultra_trade/CONTEXT.md` defines `Model` accordingly.

## Working with coding agents

**Start Claude Code inside `ultra_trade/`, not at the repo root.**

Agent instructions load from the working directory upward. A session opened at
the repo root will not pick up `ultra_trade/AGENTS.md`, and your agent will run
on production trading code with none of the constitution's guardrails applied.

There is intentionally no root-level `CLAUDE.md`: a permissive one would dilute
`ultra_trade`'s constitution, and a strict one would make research miserable.

## What lives at this level

Shared plumbing only — never governance:

- `.github/` — issue templates and CI workflows. GitHub only reads these from
  the repository root, which is why they are here rather than inside a project.
  Workflows should be path-filtered (`ultra_trade/**`) so a notebook change never
  runs the Rust gate.
- `.gitignore` — credentials and large-artifact patterns that apply everywhere.
  Each project also has its own.

## Large artifacts

Keep them out of git. Market data captures, model weights, and eval outputs go
to a store outside the repo; commit the hash and the pointer. A bloated history
slows every CI checkout and every agent worktree.
