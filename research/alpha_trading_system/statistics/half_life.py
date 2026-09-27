"""Half-life of mean reversion for an Ornstein-Uhlenbeck process.

Discretising dy = lambda * (y - mu) dt + sigma dW gives the regression

    dy_t = lambda * y_{t-1} + c + e_t

and a deviation from the mean decays by half in

    half-life = -log(2) / lambda      periods

The intercept is always fitted on a price series, so the series may revert to
its own level (mu = -c / lambda) rather than to zero; there is no
deterministic time trend. Dropping it asserts the series reverts to zero,
which is false for any price and quietly turns lambda into garbage -- USD.CAD
without an intercept estimates lambda = +0.00006, i.e. "no reversion", purely
because the rate lives near 1.37 rather than 0. ``intercept=False`` exists for
the one case where the claim holds: an already-demeaned spread.

One trap: that regression is exactly the Dickey-Fuller regression, so the
t-ratio on lambda does NOT follow a t-distribution under the unit-root null --
it follows the Dickey-Fuller distribution, which is shifted well to the left.
Reading OLS's own p-value off it overstates the evidence badly. This module
reports the MacKinnon p-value from :mod:`statistics.adf` instead, and treats
the interval around the half-life as valid only once reversion is established.
"""

import math
from dataclasses import dataclass

import numpy as np
import pandas as pd
import statsmodels.api as sm

from statistics.adf import adf_test
from statistics.prices import base_parser, header, load_close

# Rule of thumb: a half-life is only identified if the sample spans several of them.
MIN_HALF_LIVES = 4.0


@dataclass(frozen=True)
class HalfLifeResult:
    """Half-life of mean reversion, with the evidence that it exists at all."""

    half_life: float
    lam: float
    stderr: float
    mean_level: float | None
    nobs: int
    ci_low: float
    ci_high: float
    df_stat: float
    df_pvalue: float
    alpha: float
    has_intercept: bool

    @property
    def reverts(self) -> bool:
        """Is lambda negative, i.e. is there any reversion to measure?"""
        return self.lam < 0

    @property
    def significant(self) -> bool:
        """Does the Dickey-Fuller test reject a unit root at ``alpha``?"""
        return self.df_pvalue < self.alpha

    @property
    def sample_half_lives(self) -> float:
        """How many half-lives the sample spans. Below ~4 the estimate is mush."""
        return self.nobs / self.half_life if self.half_life > 0 else 0.0

    def summary(self, name: str = "series", periods_per_year: int = 252) -> str:
        lines = [
            f"Half-life: {name}",
            f"  observations    {self.nobs}",
            f"  lambda          {self.lam:+.6f} (se {self.stderr:.6f})",
        ]
        if self.reverts:
            ci = (
                f"[{self.ci_low:.1f}, infinity)"
                if math.isinf(self.ci_high)
                else f"[{self.ci_low:.1f}, {self.ci_high:.1f}]"
            )
            lines += [
                f"  half-life       {self.half_life:.1f} periods "
                f"({self.half_life / periods_per_year:.2f} years)",
                f"  {1 - self.alpha:.0%} interval    {ci} periods",
            ]
            if self.mean_level is not None:
                lines.append(f"  reverts toward  {self.mean_level:.5f}")
        else:
            lines.append(
                "  half-life       undefined -- lambda is not negative, so nothing reverts"
            )
        lines.append(
            f"  Dickey-Fuller   {self.df_stat:.4f}  p {self.df_pvalue:.4f}  -> "
            + ("reversion is significant" if self.significant else "NOT significant")
        )
        if not self.reverts:
            return "\n".join(lines)
        if not self.significant:
            lines.append(
                "  caution         a unit root cannot be rejected, so the half-life "
                "above is a number without a process behind it"
            )
        elif self.sample_half_lives < MIN_HALF_LIVES:
            lines.append(
                f"  caution         the sample spans only {self.sample_half_lives:.1f} "
                f"half-lives; {MIN_HALF_LIVES:.0f}+ is wanted before trusting the figure"
            )
        return "\n".join(lines)

    def __str__(self) -> str:
        return self.summary()


def half_life(series, intercept: bool = True, alpha: float = 0.05) -> HalfLifeResult:
    """Estimate the half-life of mean reversion, in periods.

    Parameters
    ----------
    series
        The *level* series (a price, log price, or spread). NaNs are dropped.
    intercept
        Fit a constant, so the series may revert to a non-zero mean (default).
        ``False`` forces reversion to zero -- for an already-demeaned spread.
    alpha
        Significance level, used both for the interval and the DF verdict.

    Returns
    -------
    HalfLifeResult
        ``half_life`` is ``inf`` when lambda >= 0, i.e. nothing reverts.
    """
    level = pd.Series(series).dropna().to_numpy(dtype=float)
    if len(level) < 3:
        raise ValueError(f"need at least 3 observations, got {len(level)}")
    if np.all(level == level[0]):
        raise ValueError("series is constant -- the regression is degenerate")

    dy, lagged = np.diff(level), level[:-1]
    design = sm.add_constant(lagged) if intercept else lagged[:, None]
    fit = sm.OLS(dy, design).fit()
    idx = 1 if intercept else 0
    lam = float(fit.params[idx])
    stderr = float(fit.bse[idx])

    # The same regression as an ADF test with zero lags, which is where the
    # correctly-distributed p-value for lambda comes from.
    df = adf_test(level, max_lag=0, autolag=None, regression="c" if intercept else "n")

    ln2 = math.log(2.0)
    hl = -ln2 / lam if lam < 0 else float("inf")

    # -log(2)/lambda is monotone in lambda over lambda < 0, so transforming the
    # endpoints is exact -- but it REVERSES them: the most negative lambda is the
    # fastest reversion, hence the shortest half-life. Once the upper end of the
    # lambda interval reaches 0 the half-life is unbounded above.
    lo_lam, hi_lam = (float(v) for v in fit.conf_int(alpha=alpha)[idx])
    ci_low = -ln2 / lo_lam if lo_lam < 0 else float("inf")
    ci_high = -ln2 / hi_lam if hi_lam < 0 else float("inf")

    mean_level = None
    if intercept and lam != 0:
        mean_level = float(-fit.params[0] / lam)

    return HalfLifeResult(
        half_life=hl,
        lam=lam,
        stderr=stderr,
        mean_level=mean_level,
        nobs=len(dy),
        ci_low=ci_low,
        ci_high=ci_high,
        df_stat=df.statistic,
        df_pvalue=df.pvalue,
        alpha=alpha,
        has_intercept=intercept,
    )


def main(argv=None) -> int:
    parser = base_parser("Estimate the half-life of mean reversion for a price series.")
    parser.add_argument(
        "--periods-per-year",
        type=int,
        default=252,
        help="periods per year, for the years column (default: %(default)s)",
    )
    args = parser.parse_args(argv)

    ticker, close = load_close(args.symbol, args.period, args.refresh, args.start, args.end)
    print(header(args.symbol, ticker, close), "\n")

    for label, s in [(f"{args.symbol} close", close), (f"log {args.symbol}", np.log(close))]:
        result = half_life(s, alpha=args.alpha)
        print(result.summary(name=label, periods_per_year=args.periods_per_year))
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
