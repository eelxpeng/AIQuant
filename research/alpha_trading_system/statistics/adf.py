"""Augmented Dickey-Fuller test for a unit root.

The ADF regression is

    dy_t = gamma * y_{t-1} + sum_i delta_i * dy_{t-i} + deterministic + e_t

and the statistic is the t-ratio on gamma. Under H0 the series has a unit
root (non-stationary); a sufficiently negative statistic rejects that.

The estimation is statsmodels'. What this module adds is the bits statsmodels
leaves to the caller: dropping NaNs, a stationary/not verdict at a stated
significance level, a readable summary, and a main entry that pulls a symbol
through the project's price cache.
"""

from dataclasses import dataclass

import numpy as np
import pandas as pd
from statsmodels.tsa.stattools import adfuller

from statistics.prices import base_parser, header, load_close

REGRESSIONS = ("n", "c", "ct", "ctt")
AUTOLAGS = ("AIC", "BIC", "t-stat")


@dataclass(frozen=True)
class ADFResult:
    """Outcome of one ADF regression."""

    statistic: float
    pvalue: float
    lags: int
    nobs: int
    critical_values: dict
    regression: str
    autolag: str | None

    def is_stationary(self, alpha: float = 0.05) -> bool:
        """Reject the unit root at ``alpha``? True means "looks stationary"."""
        return self.pvalue < alpha

    def summary(self, name: str = "series", alpha: float = 0.05) -> str:
        crit = "  ".join(f"{lvl} {v:8.4f}" for lvl, v in self.critical_values.items())
        lag = f"{self.lags}" + (f" (chosen by {self.autolag})" if self.autolag else "")
        verdict = (
            f"reject the unit root at {alpha:.0%} -- stationary"
            if self.is_stationary(alpha)
            else f"cannot reject the unit root at {alpha:.0%} -- non-stationary"
        )
        return (
            f"ADF test: {name}\n"
            f"  regression      {self.regression}\n"
            f"  observations    {self.nobs}\n"
            f"  lags            {lag}\n"
            f"  statistic       {self.statistic:.4f}\n"
            f"  p-value         {self.pvalue:.4f}\n"
            f"  critical values {crit}\n"
            f"  verdict         {verdict}"
        )

    def __str__(self) -> str:
        return self.summary()


def adf_test(
    series,
    max_lag: int | None = None,
    regression: str = "c",
    autolag: str | None = "AIC",
) -> ADFResult:
    """Run the Augmented Dickey-Fuller test.

    Parameters
    ----------
    series
        The series to test. NaNs are dropped.
    max_lag
        Largest difference lag considered. Defaults to Schwert's
        ``ceil(12 * (nobs/100) ** 0.25)``, clipped to what the sample supports.
    regression
        Deterministic terms: ``"n"`` none, ``"c"`` constant (default),
        ``"ct"`` constant and trend, ``"ctt"`` adds a quadratic trend.
    autolag
        ``"AIC"``, ``"BIC"``, ``"t-stat"``, or ``None`` to use ``max_lag`` as given.

    Returns
    -------
    ADFResult
    """
    x = pd.Series(series).dropna().to_numpy(dtype=float)
    result = adfuller(
        x,
        maxlag=max_lag,
        regression=regression,
        autolag=autolag,
        result_object=True,
    )
    return ADFResult(
        statistic=float(result.statistic),
        pvalue=float(result.pvalue),
        lags=int(result.lags),
        nobs=int(result.nobs),
        critical_values={k: float(v) for k, v in result.critical_values.items()},
        regression=regression,
        autolag=autolag,
    )



def main(argv=None) -> int:
    parser = base_parser("Test a price series for a unit root (ADF).")
    parser.add_argument(
        "--regression",
        default="c",
        choices=REGRESSIONS,
        help="deterministic terms in the ADF regression (default: %(default)s)",
    )
    parser.add_argument("--max-lag", type=int, default=None, help="largest lag considered")
    parser.add_argument(
        "--autolag",
        default="AIC",
        choices=(*AUTOLAGS, "none"),
        help="lag-selection criterion (default: %(default)s)",
    )
    args = parser.parse_args(argv)

    ticker, close = load_close(args.symbol, args.period, args.refresh, args.start, args.end)
    print(header(args.symbol, ticker, close), "\n")

    autolag = None if args.autolag == "none" else args.autolag
    kwargs = dict(max_lag=args.max_lag, regression=args.regression, autolag=autolag)
    log_close = np.log(close)
    series = [
        (f"{args.symbol} close (level)", close),
        (f"log {args.symbol}", log_close),
        (f"log {args.symbol} daily change", log_close.diff().dropna()),
    ]
    for label, s in series:
        print(adf_test(s, **kwargs).summary(name=label, alpha=args.alpha))
        print()

    level = adf_test(close, **kwargs)
    diff = adf_test(close.diff().dropna(), **kwargs)
    if not level.is_stationary(args.alpha) and diff.is_stationary(args.alpha):
        reading = (
            f"{args.symbol} looks I(1): the level carries a unit root, its first "
            f"difference does not. Trade the change, not the level."
        )
    elif level.is_stationary(args.alpha):
        reading = f"{args.symbol} rejects the unit root in levels -- mean reversion is on the table."
    else:
        reading = (
            f"{args.symbol} does not reject the unit root in levels, and neither does its "
            f"first difference -- check the sample before reading anything into that."
        )
    print(reading)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
