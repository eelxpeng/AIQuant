# research

Exploratory work: model development, feature research, evals, notebooks.

**Free-style on purpose.** No spec gate, no constitution, no test-first rule.
The point of this directory is to move fast and be wrong cheaply.

## The one rule

Nothing here is imported by `../ultra_trade/`.

A result crosses into the trading system as a **spec** plus, where relevant,
*data* — parameters, weights, a rule set — versioned and pinned. Never as code.
See the root `README.md` and `../ultra_trade/AGENTS.md`.

## Keep artifacts out of git

Data captures, model weights, and eval outputs go to a store outside the repo.
Commit the pointer and the hash so a result stays reproducible without putting
the bytes in history.

## Handing a result over

When something is worth productionizing, open an issue using the workstream
template and describe:

- what the result is, and the evidence it rests on
- what data the strategy would need at decision time (this is where look-ahead
  gets caught)
- which parameters are the model, and how they should be pinned

The issue number becomes the spec directory name in `ultra_trade/specs/`, so the
research finding and its implementation share one identity.
