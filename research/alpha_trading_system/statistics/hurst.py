"""Hurst exponent from the scaling of multi-period variance.

For a series whose q-period changes scale as Var(y_{t+q} - y_t) ~ q^(2H),
regressing log-variance on log-q gives the exponent as half the slope:

    H = 0.5  independent increments -- a random walk
    H > 0.5  persistent, trending
    H < 0.5  anti-persistent, mean-reverting

The estimator is a few lines; the interpretation is the hard part, so this
module reports a null band alongside the number. The band comes from
reshuffling the series' own increments, which destroys the ordering while
keeping the return distribution intact. Any small-sample bias in the
estimator then shows up identically in the band and cancels out -- a raw H
of 0.55 means nothing until you know the random-walk band is [0.39, 0.57].
"""

from dataclasses import dataclass

import numpy as np
import pandas as pd

from statistics.prices import base_parser, header, load_close


MIN_OBS = 40  # below this the longest usable horizon is too short to fit a slope


def default_lags(nobs: int, max_lag: int | None = None, count: int = 18) -> np.ndarray:
    """Log-spaced horizons from 2 up to ``max_lag`` (default: nobs/10, capped at 120)."""
    top = max_lag if max_lag is not None else min(120, nobs // 10)
    if top < 2:
        raise ValueError(f"max_lag {top} is too short for a Hurst fit")
    return np.unique(np.round(np.logspace(np.log10(2), np.log10(top), count)).astype(int))


@dataclass(frozen=True)
class HurstResult:
    """Outcome of one Hurst fit, with its random-walk reference band."""

    exponent: float
    r_squared: float
    lags: tuple
    nobs: int
    null_low: float
    null_high: float
    null_median: float
    n_sims: int
    alpha: float

    @property
    def has_band(self) -> bool:
        return not np.isnan(self.null_low)

    @property
    def regime(self) -> str:
        """Where H sits relative to the null band -- ``"unknown"`` without one."""
        if not self.has_band:
            return "unknown"
        if self.exponent > self.null_high:
            return "trending"
        if self.exponent < self.null_low:
            return "mean-reverting"
        return "random walk"

    def is_random_walk(self) -> bool:
        """Does H sit inside the reshuffled-increments band? False if none was built."""
        return self.regime == "random walk"

    def summary(self, name: str = "series") -> str:
        verdict = {
            "trending": "above the band -- persistent / trending",
            "mean-reverting": "below the band -- anti-persistent / mean-reverting",
            "random walk": "inside the band -- indistinguishable from a random walk",
            "unknown": "no null band computed (n_sims=0) -- H alone says little",
        }[self.regime]
        band_line = (
            f"  random-walk band [{self.null_low:.4f}, {self.null_high:.4f}] at "
            f"{1 - self.alpha:.0%}, from {self.n_sims} reshuffles "
            f"(median {self.null_median:.4f})\n"
            if self.has_band
            else ""
        )
        return (
            f"Hurst exponent: {name}\n"
            f"  observations    {self.nobs}\n"
            f"  horizons        {self.lags[0]}..{self.lags[-1]} ({len(self.lags)} log-spaced)\n"
            f"  exponent H      {self.exponent:.4f}\n"
            f"  log-log fit R2  {self.r_squared:.4f}\n"
            f"{band_line}"
            f"  verdict         {verdict}"
        )

    def __str__(self) -> str:
        return self.summary()


def _fit(level: np.ndarray, lags: np.ndarray):
    """Return (H, r_squared) from the log-variance / log-horizon regression."""
    variances = np.array([np.var(level[q:] - level[:-q], ddof=1) for q in lags])
    if not np.all(variances > 0):
        raise ValueError("series has zero variance at some horizon -- is it constant?")
    x, y = np.log(lags), np.log(variances)
    slope, intercept = np.polyfit(x, y, 1)
    resid = y - (slope * x + intercept)
    r_squared = 1.0 - resid.var() / y.var() if y.var() > 0 else float("nan")
    return 0.5 * slope, float(r_squared)


def hurst_exponent(
    series,
    max_lag: int | None = None,
    lags=None,
    n_sims: int = 500,
    alpha: float = 0.05,
    seed: int = 0,
) -> HurstResult:
    """Estimate the Hurst exponent of a level series and its random-walk band.

    Parameters
    ----------
    series
        The *level* series (a price, or a log price). NaNs are dropped.
        Do not pass returns -- the increments are taken internally.
    max_lag
        Longest horizon in the fit. Defaults to nobs/10, capped at 120.
    lags
        Explicit horizons, overriding ``max_lag``.
    n_sims
        Reshuffles used to build the null band. 0 skips it (band becomes NaN).
    alpha
        Band width: ``alpha=0.05`` gives the central 95%.
    seed
        Seeds the reshuffling, so the band is reproducible.

    Returns
    -------
    HurstResult
    """
    level = pd.Series(series).dropna().to_numpy(dtype=float)
    nobs = len(level)
    if nobs < MIN_OBS:
        raise ValueError(f"need at least {MIN_OBS} observations for a Hurst fit, got {nobs}")
    lags = default_lags(nobs, max_lag) if lags is None else np.asarray(sorted(set(lags)))
    if lags[0] < 1:
        raise ValueError("lags must be >= 1")
    if lags[-1] >= nobs:
        raise ValueError(f"longest lag {lags[-1]} needs more than {nobs} observations")

    exponent, r_squared = _fit(level, lags)

    if n_sims > 0:
        rng = np.random.default_rng(seed)
        increments = np.diff(level)
        null = np.empty(n_sims)
        for i in range(n_sims):
            walk = np.concatenate([[level[0]], level[0] + np.cumsum(rng.permutation(increments))])
            null[i], _ = _fit(walk, lags)
        low, high = np.percentile(null, [100 * alpha / 2, 100 * (1 - alpha / 2)])
        median = float(np.median(null))
    else:
        low = high = median = float("nan")

    return HurstResult(
        exponent=exponent,
        r_squared=r_squared,
        lags=tuple(int(q) for q in lags),
        nobs=nobs,
        null_low=float(low),
        null_high=float(high),
        null_median=median,
        n_sims=n_sims,
        alpha=alpha,
    )


def main(argv=None) -> int:
    parser = base_parser("Estimate the Hurst exponent of a price series.")
    parser.add_argument("--max-lag", type=int, default=None, help="longest horizon in the fit")
    parser.add_argument(
        "--sims", type=int, default=500, help="reshuffles for the null band (default: %(default)s)"
    )
    parser.add_argument("--seed", type=int, default=0, help="seed for the reshuffling")
    args = parser.parse_args(argv)

    ticker, close = load_close(args.symbol, args.period, args.refresh, args.start, args.end)
    print(header(args.symbol, ticker, close), "\n")

    for label, s in [(f"{args.symbol} close", close), (f"log {args.symbol}", np.log(close))]:
        result = hurst_exponent(
            s, max_lag=args.max_lag, n_sims=args.sims, alpha=args.alpha, seed=args.seed
        )
        print(result.summary(name=label))
        print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
