"""Johansen cointegration test for a basket of series.

Where Engle-Granger regresses one series on the others -- and so gives a
different answer depending on which you put on the left -- Johansen treats the
basket symmetrically. It fits a vector error-correction model and asks how many
independent stationary combinations the system supports. That count is the
cointegrating *rank*:

    rank 0    no cointegration; the series wander independently
    rank 1    one stationary combination -- the tradeable spread
    rank r    r independent combinations (at most n-1 for n series)

Two statistics decide it, both read as a sequence of tests rather than one
verdict. Starting at r = 0, reject and move up; the first hypothesis you cannot
reject is the rank. The trace statistic tests "rank <= r" against "rank > r";
the maximum-eigenvalue statistic tests it against exactly "rank = r + 1".

statsmodels' ``coint_johansen`` returns the raw statistic and critical-value
tables and stops there -- it does not walk the sequence, and it hands back
eigenvectors without saying which one to trade. This module does both: it
resolves the rank and normalises the leading eigenvector into portfolio
weights, so the output is a spread you can actually hold.

Critical values are tabulated, not computed -- only at 90%, 95% and 99%, and
only for up to 12 series. There are no p-values to be had.
"""

import argparse
import warnings
from dataclasses import dataclass
from pathlib import Path

import numpy as np
import pandas as pd
from statsmodels.tsa.vector_ar.vecm import coint_johansen

from statistics.half_life import HalfLifeResult, half_life
from statistics.prices import add_common_flags, load_closes, to_yahoo_symbol

DEFAULT_BASKET = ("EWA", "EWC", "IGE")
MAX_VARIABLES = 12  # statsmodels tabulates critical values no further
CRIT_COLUMN = {0.10: 0, 0.05: 1, 0.01: 2}
DET_ORDERS = {-1: "no deterministic term", 0: "constant", 1: "linear trend"}


@dataclass(frozen=True)
class JohansenResult:
    """Outcome of one Johansen test, with the spread its leading vector implies."""

    names: tuple
    trace_stats: np.ndarray
    trace_crit: np.ndarray
    max_eig_stats: np.ndarray
    max_eig_crit: np.ndarray
    eigenvalues: np.ndarray
    vectors: np.ndarray
    rank_trace: int
    rank_max_eig: int
    nobs: int
    det_order: int
    k_ar_diff: int
    alpha: float
    legs: pd.DataFrame
    spread: pd.Series
    spread_half_life: HalfLifeResult | None

    @property
    def weights(self) -> pd.Series:
        """The leading cointegrating vector, scaled to 1 unit of the first series."""
        return pd.Series(self.vectors[:, 0], index=list(self.names), name="weight")

    def is_cointegrated(self) -> bool:
        """Does the trace sequence find at least one cointegrating relation?"""
        return self.rank_trace >= 1

    @property
    def agree(self) -> bool:
        return self.rank_trace == self.rank_max_eig

    @property
    def spread_label(self) -> str:
        w = self.weights
        terms = [f"{w.iloc[0]:.0f}·{w.index[0]}"] + [
            f"{v:+.4f}·{k}" for k, v in w.iloc[1:].items()
        ]
        return " ".join(terms)

    @property
    def plot_headline(self) -> str:
        verdict = (
            f"rank {self.rank_trace} at {self.alpha:.0%}"
            if self.is_cointegrated()
            else f"NOT cointegrated at {self.alpha:.0%}"
        )
        return f"{', '.join(self.names)} — {verdict}"

    @property
    def plot_subtitle(self) -> str:
        hl = self.spread_half_life
        parts = [
            f"Johansen trace rank {self.rank_trace}",
            f"max-eigenvalue rank {self.rank_max_eig}",
            f"{self.nobs} observations",
        ]
        if hl is not None and hl.reverts:
            parts.append(f"spread half-life {hl.half_life:.1f} days")
        return "   ·   ".join(parts)

    def summary(self) -> str:
        pct = f"{self.alpha:.0%}"
        lines = [
            f"Johansen: {', '.join(self.names)}",
            f"  observations    {self.nobs}",
            f"  specification   {DET_ORDERS[self.det_order]}, "
            f"{self.k_ar_diff} lagged difference(s)",
            "",
            f"  {'H0':<10}{'trace':>10}{'crit ' + pct:>11}   "
            f"{'max-eig':>10}{'crit ' + pct:>11}",
        ]
        for r in range(len(self.trace_stats)):
            t_flag = "*" if self.trace_stats[r] > self.trace_crit[r] else " "
            m_flag = "*" if self.max_eig_stats[r] > self.max_eig_crit[r] else " "
            label = "r = 0" if r == 0 else f"r <= {r}"
            lines.append(
                f"  {label:<10}{self.trace_stats[r]:>10.4f}{self.trace_crit[r]:>11.4f} {t_flag} "
                f"  {self.max_eig_stats[r]:>10.4f}{self.max_eig_crit[r]:>11.4f} {m_flag}"
            )
        lines += [
            "",
            f"  rank            {self.rank_trace} by trace, "
            f"{self.rank_max_eig} by max-eigenvalue"
            + ("" if self.agree else "   <- they disagree"),
        ]
        if self.is_cointegrated():
            w = self.weights
            lines.append(
                "  leading vector  " + "  ".join(f"{k} {v:+.4f}" for k, v in w.items())
            )
            lines.append(f"  spread          {self.spread_label}")
            hl = self.spread_half_life
            if hl is None:
                lines.append("  spread half-life n/a -- the spread is degenerate")
            elif hl.reverts:
                lines.append(f"  spread half-life {hl.half_life:.1f} periods")
            else:
                lines.append("  spread half-life undefined (spread does not revert)")
            plural = "" if self.rank_trace == 1 else "s"
            lines.append(
                f"  verdict         {self.rank_trace} cointegrating relation{plural} at {pct}"
            )
        else:
            lines.append(f"  verdict         no cointegration at {pct}")
        return "\n".join(lines)

    def __str__(self) -> str:
        return self.summary()


def _sequential_rank(stats, crit) -> int:
    """Walk r = 0, 1, ... and stop at the first hypothesis that survives."""
    for r, (stat, cv) in enumerate(zip(stats, crit)):
        if stat <= cv:
            return r
    return len(stats)


def _as_frame(data, names=None) -> pd.DataFrame:
    if isinstance(data, pd.DataFrame):
        frame = data.copy()
    elif isinstance(data, dict):
        frame = pd.DataFrame(data)
    else:
        frame = pd.concat([pd.Series(s) for s in data], axis=1)
    if names is not None:
        frame.columns = list(names)
    return frame.dropna()


def johansen_test(
    data,
    names=None,
    det_order: int = 0,
    k_ar_diff: int = 1,
    alpha: float = 0.05,
) -> JohansenResult:
    """Run the Johansen cointegration test on a basket of level series.

    Parameters
    ----------
    data
        A DataFrame, a dict of Series, or a sequence of Series -- the *level*
        series (normally log prices). Rows with any NaN are dropped.
    names
        Column labels, overriding whatever ``data`` carries.
    det_order
        ``-1`` no deterministic term, ``0`` a constant (default), ``1`` a
        linear trend.
    k_ar_diff
        Lagged differences in the VECM.
    alpha
        Significance level. Only ``0.10``, ``0.05`` and ``0.01`` are tabulated.

    Returns
    -------
    JohansenResult
    """
    if det_order not in DET_ORDERS:
        raise ValueError(f"det_order must be one of {sorted(DET_ORDERS)}, got {det_order}")
    if alpha not in CRIT_COLUMN:
        raise ValueError(
            f"Johansen critical values exist only for alpha in {sorted(CRIT_COLUMN)}, got {alpha}"
        )
    if k_ar_diff < 0:
        raise ValueError(f"k_ar_diff must be non-negative, got {k_ar_diff}")

    frame = _as_frame(data, names)
    n_series = frame.shape[1]
    if n_series < 2:
        raise ValueError(f"need at least 2 series, got {n_series}")
    if n_series > MAX_VARIABLES:
        raise ValueError(
            f"critical values are tabulated for at most {MAX_VARIABLES} series, got {n_series}"
        )
    if len(frame) <= n_series + k_ar_diff + 2:
        raise ValueError(f"only {len(frame)} usable rows -- not enough for the VECM")

    with warnings.catch_warnings():
        # statsmodels casts its complex eigen-decomposition to real inside the
        # trace loop and warns on every element. The imaginary parts are zero
        # for a well-posed system -- which is checked below rather than assumed.
        warnings.simplefilter("ignore", np.exceptions.ComplexWarning)
        fitted = coint_johansen(frame.to_numpy(dtype=float), det_order, k_ar_diff)

    imaginary = max(
        float(np.abs(np.imag(fitted.eig)).max()),
        float(np.abs(np.imag(fitted.evec)).max()),
    )
    if imaginary > 1e-8:
        warnings.warn(
            f"Johansen eigen-decomposition carries imaginary parts up to {imaginary:.2e}; "
            "the cointegrating vectors are not trustworthy",
            RuntimeWarning,
            stacklevel=2,
        )

    column = CRIT_COLUMN[alpha]
    trace_stats = np.asarray(fitted.lr1, dtype=float)
    trace_crit = np.asarray(fitted.cvt, dtype=float)[:, column]
    max_eig_stats = np.asarray(fitted.lr2, dtype=float)
    max_eig_crit = np.asarray(fitted.cvm, dtype=float)[:, column]

    # statsmodels returns these complex and already sorted by eigenvalue.
    eigenvalues = np.asarray(fitted.eig).real
    vectors = np.asarray(fitted.evec).real
    leading = vectors[:, 0]
    if leading[0] != 0:  # scale to 1 unit of the first series, so it reads as a portfolio
        vectors = vectors / leading[0]

    spread = pd.Series(
        frame.to_numpy(dtype=float) @ vectors[:, 0],
        index=frame.index,
        name="spread",
    )
    try:
        spread_hl = half_life(spread)
    except ValueError:
        spread_hl = None

    return JohansenResult(
        names=tuple(str(c) for c in frame.columns),
        trace_stats=trace_stats,
        trace_crit=trace_crit,
        max_eig_stats=max_eig_stats,
        max_eig_crit=max_eig_crit,
        eigenvalues=eigenvalues,
        vectors=vectors,
        rank_trace=_sequential_rank(trace_stats, trace_crit),
        rank_max_eig=_sequential_rank(max_eig_stats, max_eig_crit),
        nobs=len(frame),
        det_order=det_order,
        k_ar_diff=k_ar_diff,
        alpha=alpha,
        legs=frame,
        spread=spread,
        spread_half_life=spread_hl,
    )


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(
        description="Test a basket of price series for cointegration (Johansen).",
        epilog="Run from the repo root. Defaults to the EWA/EWC/IGE basket.",
    )
    parser.add_argument(
        "symbols",
        nargs="*",
        default=list(DEFAULT_BASKET),
        metavar="SYMBOL",
        help=f"two or more symbols (default: {' '.join(DEFAULT_BASKET)})",
    )
    parser.add_argument(
        "--det-order",
        type=int,
        default=0,
        choices=sorted(DET_ORDERS),
        help="-1 none, 0 constant, 1 linear trend (default: %(default)s)",
    )
    parser.add_argument(
        "--k-ar-diff",
        type=int,
        default=1,
        help="lagged differences in the VECM (default: %(default)s)",
    )
    parser.add_argument("--raw", action="store_true", help="use raw prices, not log prices")
    parser.add_argument(
        "--plot",
        nargs="?",
        const="auto",
        metavar="PATH",
        help="plot the spread; optional output path",
    )
    parser.add_argument("--dark", action="store_true", help="dark palette for the plot")
    parser.add_argument("--show", action="store_true", help="open the plot window too")
    add_common_flags(parser)
    args = parser.parse_args(argv)

    if len(args.symbols) < 2:
        parser.error(f"need at least two symbols, got {len(args.symbols)}")

    tickers, frame = load_closes(args.symbols, args.period, args.refresh, args.start, args.end)
    for symbol, ticker in tickers.items():
        column = frame[symbol]
        print(
            f"{symbol} ({ticker})  {len(column)} daily closes  "
            f"{column.index[0].date()} to {column.index[-1].date()}"
        )
    if len(frame) == 0:
        raise SystemExit("No overlapping trading days across those symbols.")
    if not args.raw:
        frame = np.log(frame)
    print(f"\n{len(frame)} overlapping rows, testing on "
          f"{'raw prices' if args.raw else 'log prices'}.\n")

    result = johansen_test(
        frame, det_order=args.det_order, k_ar_diff=args.k_ar_diff, alpha=args.alpha
    )
    print(result.summary())
    print()

    if args.plot:
        from statistics.plotting import plot_spread

        path = (
            Path("_".join(to_yahoo_symbol(s) for s in args.symbols) + "_spread.png")
            if args.plot == "auto"
            else Path(args.plot)
        )
        print(f"Plot written to {plot_spread(result, path=path, dark=args.dark, show=args.show)}")

    if not result.agree:
        print(
            "Note: trace and max-eigenvalue disagree on the rank. They test different "
            "alternatives, and the trace test is the more robust of the two in small "
            "samples -- but a disagreement is a reason to widen the sample, not to "
            "pick the answer you prefer."
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
