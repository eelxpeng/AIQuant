"""Lo-MacKinlay variance ratio test for a random walk.

If a log price is a random walk, the variance of its q-period changes grows
linearly in q, so

    VR(q) = Var(y_t - y_{t-q}) / (q * Var(y_t - y_{t-1})) = 1

    VR > 1  positive serial correlation -- trending / momentum
    VR < 1  negative serial correlation -- mean reversion

Estimation is arch's, which gives the heteroskedasticity-robust standard
errors of Lo & MacKinlay (1988). That robustness is not optional on daily FX:
under the homoskedastic statistic, volatility clustering alone is enough to
reject a random walk that is perfectly well behaved in its mean.

Unlike the ADF test, this is informative at every horizon -- a series can be
a random walk day to day and mean-revert over a quarter, so read the sweep
across q rather than any single row.
"""

from dataclasses import dataclass

import numpy as np
import pandas as pd
from arch.unitroot import VarianceRatio

from statistics.prices import base_parser, header, load_close

DEFAULT_HORIZONS = (2, 4, 8, 16, 32, 64)


@dataclass(frozen=True)
class VarianceRatioResult:
    """Outcome of one variance ratio test at a single horizon."""

    horizon: int
    vr: float
    statistic: float
    pvalue: float
    nobs: int
    robust: bool

    def rejects_random_walk(self, alpha: float = 0.05) -> bool:
        return self.pvalue < alpha

    @property
    def direction(self) -> str:
        return "trending" if self.vr > 1 else "mean-reverting"

    def verdict(self, alpha: float = 0.05) -> str:
        if not self.rejects_random_walk(alpha):
            return "random walk"
        return self.direction


def variance_ratio_test(
    series,
    horizon: int = 2,
    robust: bool = True,
    trend: str = "c",
    overlap: bool = True,
    debiased: bool = True,
) -> VarianceRatioResult:
    """Run the variance ratio test at one horizon.

    Parameters
    ----------
    series
        The *level* series (normally a log price). NaNs are dropped.
        Do not pass returns -- the increments are taken internally.
    horizon
        The q in VR(q), in periods. Must be at least 2.
    robust
        Use heteroskedasticity-robust standard errors (default, and advisable).
    trend
        ``"c"`` allows a drift (default), ``"n"`` does not.
    overlap
        Use overlapping windows (default) -- far more efficient at long horizons.
    debiased
        Apply the finite-sample bias correction.

    Returns
    -------
    VarianceRatioResult
    """
    level = pd.Series(series).dropna().to_numpy(dtype=float)
    if horizon > len(level) // 2:
        raise ValueError(
            f"horizon {horizon} needs more than {len(level)} observations to mean anything"
        )
    test = VarianceRatio(
        level, lags=horizon, trend=trend, debiased=debiased, robust=robust, overlap=overlap
    )
    return VarianceRatioResult(
        horizon=horizon,
        vr=float(test.vr),
        statistic=float(test.stat),
        pvalue=float(test.pvalue),
        nobs=int(test.nobs),
        robust=robust,
    )


def variance_ratio_sweep(series, horizons=DEFAULT_HORIZONS, **kwargs):
    """Run :func:`variance_ratio_test` across horizons, skipping any too long."""
    level = pd.Series(series).dropna()
    return [
        variance_ratio_test(level, horizon=q, **kwargs)
        for q in sorted(horizons)
        if q <= len(level) // 2
    ]


def format_sweep(results, name: str = "series", alpha: float = 0.05) -> str:
    """Render a sweep as a table."""
    kind = "robust" if results and results[0].robust else "homoskedastic"
    lines = [
        f"Variance ratio: {name}",
        f"  {results[0].nobs} observations, {kind} standard errors, "
        f"H0: random walk, rejected at {alpha:.0%}",
        f"  {'horizon':>8}{'VR(q)':>10}{'statistic':>12}{'p-value':>10}   verdict",
    ]
    for r in results:
        flag = " *" if r.rejects_random_walk(alpha) else "  "
        lines.append(
            f"  {r.horizon:>8}{r.vr:>10.4f}{r.statistic:>12.4f}{r.pvalue:>10.4f}{flag} {r.verdict(alpha)}"
        )
    return "\n".join(lines)


def main(argv=None) -> int:
    parser = base_parser("Test a price series for a random walk (Lo-MacKinlay variance ratio).")
    parser.add_argument(
        "--horizons",
        default=",".join(str(q) for q in DEFAULT_HORIZONS),
        help="comma-separated horizons q (default: %(default)s)",
    )
    parser.add_argument(
        "--no-robust",
        action="store_true",
        help="use homoskedastic standard errors instead of the robust ones",
    )
    parser.add_argument(
        "--trend", default="c", choices=("c", "n"), help="allow a drift (default: %(default)s)"
    )
    args = parser.parse_args(argv)

    horizons = [int(q) for q in args.horizons.split(",") if q.strip()]
    ticker, close = load_close(args.symbol, args.period, args.refresh, args.start, args.end)
    print(header(args.symbol, ticker, close), "\n")

    for label, s in [(f"{args.symbol} close", close), (f"log {args.symbol}", np.log(close))]:
        results = variance_ratio_sweep(
            s, horizons=horizons, robust=not args.no_robust, trend=args.trend
        )
        print(format_sweep(results, name=label, alpha=args.alpha))
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
