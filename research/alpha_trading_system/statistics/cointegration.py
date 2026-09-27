"""Engle-Granger cointegration test -- the "CADF" test on a pair.

Two I(1) series are cointegrated if some linear combination of them is I(0).
The augmented Engle-Granger procedure is two steps:

    1. regress y on x (plus a constant) -- the slope is the hedge ratio
    2. ADF-test the residual spread for a unit root, with no constant

Step 2 is a *different* test from a plain ADF even though it runs the same
regression. Because the spread was fitted rather than observed, OLS has
already minimised its variance, so the residual looks far more stationary
than it is. The null distribution has to shift left to compensate -- that is
MacKinnon's N, the number of series in the system. Skip the correction and
two independent random walks test as cointegrated about 56% of the time
instead of 5%.

:func:`statsmodels.tsa.stattools.coint` does exactly the two steps above and
applies the correction, so it is the test -- but it returns only the
statistic and p-value. It discards the stage-1 fit, which is where the hedge
ratio and the tradeable spread live. This module keeps them, and runs the
pair both ways round, since Engle-Granger is not symmetric in y and x.
"""

import argparse
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pandas as pd
import statsmodels.api as sm
from statsmodels.tsa.stattools import coint

from statistics.half_life import HalfLifeResult, half_life
from statistics.prices import add_common_flags, header, load_closes

DEFAULT_PAIR = ("EWA", "EWC")
TRENDS = ("n", "c", "ct", "ctt")


@dataclass(frozen=True)
class CointegrationResult:
    """One direction of an Engle-Granger test, with the spread it implies."""

    name_y: str
    name_x: str
    statistic: float
    pvalue: float
    critical_values: dict
    hedge_ratio: float
    intercept: float
    nobs: int
    trend: str
    y: pd.Series
    x: pd.Series
    spread: pd.Series
    spread_half_life: HalfLifeResult | None

    def is_cointegrated(self, alpha: float = 0.05) -> bool:
        return self.pvalue < alpha

    @property
    def relation(self) -> str:
        return f"{self.name_y} - {self.hedge_ratio:.4f} * {self.name_x}"

    # --- the adapter statistics.plotting draws through ----------------------
    @property
    def legs(self) -> pd.DataFrame:
        return pd.concat([self.y, self.x], axis=1)

    @property
    def spread_label(self) -> str:
        return f"{self.name_y} − {self.hedge_ratio:.4f} · {self.name_x}"

    @property
    def plot_headline(self) -> str:
        verdict = "cointegrated" if self.pvalue < 0.05 else "NOT cointegrated at 5%"
        return f"{self.name_y} / {self.name_x} spread — {verdict}"

    @property
    def plot_subtitle(self) -> str:
        hl = self.spread_half_life
        parts = [
            f"Engle-Granger p = {self.pvalue:.4f}",
            f"hedge ratio {self.hedge_ratio:.4f}",
            f"{self.nobs} observations",
        ]
        if hl is not None and hl.reverts:
            parts.append(f"spread half-life {hl.half_life:.1f} days")
        return "   ·   ".join(parts)

    def summary(self, alpha: float = 0.05) -> str:
        crit = "  ".join(f"{k} {v:8.4f}" for k, v in self.critical_values.items())
        verdict = (
            f"reject no-cointegration at {alpha:.0%} -- cointegrated"
            if self.is_cointegrated(alpha)
            else f"cannot reject no-cointegration at {alpha:.0%}"
        )
        hl = self.spread_half_life
        if hl is None:
            hl_line = "n/a -- the spread is degenerate"
        elif hl.reverts:
            hl_line = f"{hl.half_life:.1f} periods"
        else:
            hl_line = "undefined (spread does not revert)"
        return (
            f"Engle-Granger: {self.name_y} on {self.name_x}\n"
            f"  observations    {self.nobs}\n"
            f"  hedge ratio     {self.hedge_ratio:.4f}   intercept {self.intercept:.4f}\n"
            f"  spread          {self.relation}\n"
            f"  CADF statistic  {self.statistic:.4f}\n"
            f"  p-value         {self.pvalue:.4f}\n"
            f"  critical values {crit}\n"
            f"  spread half-life {hl_line}\n"
            f"  verdict         {verdict}"
        )

    def __str__(self) -> str:
        return self.summary()


def _align(y, x):
    """Inner-join two series on their index, so holidays cannot misalign them."""
    frame = pd.concat(
        [pd.Series(y).rename("y"), pd.Series(x).rename("x")], axis=1, join="inner"
    ).dropna()
    if len(frame) < 20:
        raise ValueError(f"only {len(frame)} overlapping observations -- not enough")
    return frame["y"], frame["x"]


def engle_granger(
    y,
    x,
    trend: str = "c",
    max_lag: int | None = None,
    autolag: str | None = "aic",
    name_y: str = "y",
    name_x: str = "x",
) -> CointegrationResult:
    """Test whether ``y`` and ``x`` are cointegrated, regressing y on x.

    Parameters
    ----------
    y, x
        The two *level* series (normally log prices). Aligned on their index.
    trend
        Deterministic terms in the cointegrating regression: ``"c"`` (default),
        ``"n"``, ``"ct"``, ``"ctt"``. With ``"n"`` statsmodels has no 2010
        critical values, so those come back NaN.
    max_lag, autolag
        Passed to the stage-2 ADF. ``autolag`` is ``"aic"``, ``"bic"``,
        ``"t-stat"`` or None.
    name_y, name_x
        Labels used in the summary.

    Returns
    -------
    CointegrationResult

    Notes
    -----
    Not symmetric: ``engle_granger(a, b)`` and ``engle_granger(b, a)`` can
    disagree. Use :func:`both_directions` and read them together.
    """
    if trend not in TRENDS:
        raise ValueError(f"trend must be one of {TRENDS}, got {trend!r}")
    ys, xs = _align(y, x)
    ys, xs = ys.rename(name_y), xs.rename(name_x)

    statistic, pvalue, crit = coint(
        ys.to_numpy(), xs.to_numpy(), trend=trend, maxlag=max_lag, autolag=autolag
    )

    # statsmodels throws the stage-1 fit away; re-run it to recover the hedge
    # ratio and the spread. Identical design, so the residuals match exactly.
    design = sm.add_constant(xs.to_numpy()) if trend != "n" else xs.to_numpy()[:, None]
    stage1 = sm.OLS(ys.to_numpy(), design).fit()
    hedge = float(stage1.params[-1])
    const = float(stage1.params[0]) if trend != "n" else 0.0
    spread = pd.Series(stage1.resid, index=ys.index, name=f"{name_y}-{name_x} spread")

    try:
        spread_hl = half_life(spread)
    except ValueError:
        # Perfectly collinear inputs leave a constant residual. statsmodels has
        # already warned; return the result rather than dying on the half-life.
        spread_hl = None

    return CointegrationResult(
        name_y=name_y,
        name_x=name_x,
        statistic=float(statistic),
        pvalue=float(pvalue),
        critical_values={k: float(v) for k, v in zip(("1%", "5%", "10%"), crit)},
        hedge_ratio=hedge,
        intercept=const,
        nobs=len(ys),
        trend=trend,
        y=ys,
        x=xs,
        spread=spread,
        spread_half_life=spread_hl,
    )


def both_directions(a, b, name_a: str = "a", name_b: str = "b", **kwargs):
    """Run Engle-Granger both ways round. Returns ``(a_on_b, b_on_a)``."""
    return (
        engle_granger(a, b, name_y=name_a, name_x=name_b, **kwargs),
        engle_granger(b, a, name_y=name_b, name_x=name_a, **kwargs),
    )


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="Test two price series for cointegration (Engle-Granger).",
        epilog="Run from the repo root. Defaults to the EWA/EWC pair.",
    )
    parser.add_argument(
        "symbols",
        nargs="*",
        default=list(DEFAULT_PAIR),
        metavar="SYMBOL",
        help=f"exactly two symbols (default: {' '.join(DEFAULT_PAIR)})",
    )
    parser.add_argument(
        "--trend",
        default="c",
        choices=TRENDS,
        help="deterministic terms in the cointegrating regression (default: %(default)s)",
    )
    parser.add_argument("--max-lag", type=int, default=None, help="largest stage-2 ADF lag")
    parser.add_argument(
        "--autolag",
        default="aic",
        choices=("aic", "bic", "t-stat", "none"),
        help="stage-2 lag selection (default: %(default)s)",
    )
    parser.add_argument(
        "--raw", action="store_true", help="use raw prices instead of log prices"
    )
    parser.add_argument(
        "--plot",
        nargs="?",
        const="auto",
        metavar="PATH",
        help="plot the spread; optional output path (default: <A>_<B>_spread.png)",
    )
    parser.add_argument("--dark", action="store_true", help="dark palette for the plot")
    parser.add_argument("--show", action="store_true", help="open the plot window too")
    add_common_flags(parser)
    args = parser.parse_args(argv)

    if len(args.symbols) != 2:
        parser.error(f"need exactly two symbols, got {len(args.symbols)}")
    sym_a, sym_b = args.symbols

    tickers, frame = load_closes((sym_a, sym_b), args.period, args.refresh, args.start, args.end)
    if len(frame) == 0:
        raise SystemExit(f"No overlapping trading days between {sym_a} and {sym_b}.")
    close_a, close_b = frame[sym_a], frame[sym_b]
    print(header(sym_a, tickers[sym_a], close_a))
    print(header(sym_b, tickers[sym_b], close_b))
    kind = "raw prices" if args.raw else "log prices"
    print(f"\nTesting on {kind}.\n")

    series_a, series_b = (close_a, close_b) if args.raw else (np.log(close_a), np.log(close_b))
    autolag = None if args.autolag == "none" else args.autolag
    forward, reverse = both_directions(
        series_a,
        series_b,
        name_a=sym_a,
        name_b=sym_b,
        trend=args.trend,
        max_lag=args.max_lag,
        autolag=autolag,
    )
    for result in (forward, reverse):
        print(result.summary(alpha=args.alpha))
        print()

    if args.plot:
        from statistics.plotting import plot_spread

        path = (
            Path(f"{sym_a}_{sym_b}_spread.png".replace("/", "-"))
            if args.plot == "auto"
            else Path(args.plot)
        )
        written = plot_spread(forward, path=path, dark=args.dark, show=args.show)
        print(f"Plot of {forward.name_y} on {forward.name_x} written to {written}\n")

    votes = [r.is_cointegrated(args.alpha) for r in (forward, reverse)]
    if all(votes):
        reading = (
            f"{sym_a} and {sym_b} are cointegrated in both directions -- the finding does not "
            f"hinge on which series is regressed on which."
        )
    elif any(votes):
        winner = forward if votes[0] else reverse
        reading = (
            f"Cointegrated in one direction only ({winner.name_y} on {winner.name_x}). "
            f"Engle-Granger is not symmetric, so treat this as weak -- Johansen is the "
            f"direction-free test if it matters."
        )
    else:
        reading = f"No cointegration between {sym_a} and {sym_b} at {args.alpha:.0%} either way."
    print(reading)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
