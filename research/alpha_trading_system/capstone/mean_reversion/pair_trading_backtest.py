import argparse

import matplotlib.pyplot as plt
import numpy as np
import pandas as pd

from backtest.performance import max_drawdown, sharpe_ratio, sortino_ratio
from statistics.plotting import plot_panels
from statistics.prices import add_common_flags, load_closes


DEFAULT_PAIR = ("EWC", "EWA")


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
        help="exactly two symbols, x then y -- the spread is y - beta * x, so "
             f"'GLD USO' means USO - beta * GLD (default: {' '.join(DEFAULT_PAIR)})",
    )
    parser.add_argument("--lookback", type=int, default=20, help="lookback window")
    parser.add_argument(
        "--raw", action="store_true", help="use raw prices instead of log prices"
    )
    parser.add_argument(
        "--linear", action="store_true",
        help="size continuously at -z units instead of the +/-1 entry/exit band",
    )
    parser.add_argument(
        "--normalize", default="fixed", choices=("fixed", "deployed"),
        help="return base: 'fixed' divides by a constant $1 of capital, "
             "'deployed' divides by the gross actually on at the time, which is "
             "Chan's convention (default: %(default)s)",
    )
    add_common_flags(parser)
    args = parser.parse_args(argv)

    if len(args.symbols) != 2:
        parser.error(f"need exactly two symbols, got {len(args.symbols)}")
    sym_a, sym_b = args.symbols

    tickers, df = load_closes([sym_a, sym_b], period=args.period, refresh=args.refresh,
                              start=args.start, end=args.end)
    prices = df                      # raw closes: what the P&L is earned on
    if not args.raw:
        df = np.log(df)              # fit basis: what the hedge ratio is fitted on
    W = args.lookback

    equity = []
    for end in range(W, len(df) + 1):
        window = df.iloc[end-W:end]
        hedge_ratio = np.polyfit(window[sym_a], window[sym_b], 1)[0]
        yport = df[sym_b].iloc[end-1] - hedge_ratio * df[sym_a].iloc[end-1]
        equity.append({
            "date": df.index[end-1],
            "beta": hedge_ratio,
            "yport": yport,
        })
    equity = pd.DataFrame(equity).set_index("date")

    spread = equity["yport"]
    rolling_mean = spread.rolling(W).mean()
    rolling_std = spread.rolling(W).std()
    zscore = (spread - rolling_mean) / rolling_std

    equity["zscore"] = zscore

    entry_zscore = 1.0
    exit_zscore = 0.0

    cost_bps = 1.0                   # round-trip cost on traded notional

    # ---- how many units to hold ------------------------------------------
    # One unit is one unit of the spread: long 1 of sym_b, short `beta` of
    # sym_a.
    if args.linear:
        # Continuous sizing: hold -z units, so the position grows with how
        # stretched the spread is and is never flat. No thresholds at all.
        equity["units"] = -equity["zscore"].fillna(0.0)
    else:
        # Banded sizing: +/-1 unit, a state rather than a size, however far the
        # z-score runs. Exit is tested before entry, so a z-score that leaps
        # clean across the band closes and reopens on the same bar instead of
        # sitting the move out.
        units = []
        position = 0
        for z in equity["zscore"]:
            if np.isnan(z):
                position = 0
            else:
                if position == 1 and z >= -exit_zscore:
                    position = 0
                elif position == -1 and z <= exit_zscore:
                    position = 0
                if position == 0:
                    if z <= -entry_zscore:
                        position = 1
                    elif z >= entry_zscore:
                        position = -1
            units.append(position)
        equity["units"] = units

    # ---- what one unit is worth, leg by leg -------------------------------
    # Normalised so the two legs sum to $1 gross, which makes the strategy
    # return a return on capital rather than an unscaled P&L. On log prices
    # beta is an elasticity (dollar weights); on raw prices it is a share
    # ratio, so the price levels come into the weights.
    if args.raw:
        leg_b = prices[sym_b].reindex(equity.index)
        leg_a = -equity["beta"] * prices[sym_a].reindex(equity.index)
    else:
        leg_b = pd.Series(1.0, index=equity.index)
        leg_a = -equity["beta"]
    gross = leg_b.abs() + leg_a.abs()
    equity["pos_b"] = equity["units"] * leg_b / gross
    equity["pos_a"] = equity["units"] * leg_a / gross

    # ---- P&L ---------------------------------------------------------------
    # Yesterday's position earns today's move. That shift is the whole
    # difference between a backtest and a look-ahead: beta and the z-score at
    # time t are computed from a window ending at t, so they are knowable only
    # at t's close and can only be traded into t+1.
    #
    # Note this earns `pos.shift(1) * return`, NOT `spread.diff()`. Because
    # beta is refitted daily, spread_t - spread_{t-1} mixes the price move with
    # the hedge-ratio change and silently credits you for rebalancing.
    rets = prices.pct_change().reindex(equity.index)
    pnl = (
        equity["pos_b"].shift(1) * rets[sym_b] + equity["pos_a"].shift(1) * rets[sym_a]
    ).fillna(0.0)
    if args.normalize == "deployed":
        # Chan divides the P&L by the gross actually on at the time. Watch what
        # that does: positions are units x weights, so the units cancel top and
        # bottom and only their sign survives. Under this base, sizing at -z
        # earns exactly what sizing at -sign(z) earns.
        deployed = (equity["pos_b"].abs() + equity["pos_a"].abs()).shift(1)
        equity["gross_ret"] = (pnl / deployed.replace(0.0, np.nan)).fillna(0.0)
    else:
        # Fixed base: $1 of capital throughout, so holding 3 units really is
        # three times the risk of holding 1.
        equity["gross_ret"] = pnl
    turnover = (
        equity["pos_b"].diff().abs().fillna(equity["pos_b"].abs())
        + equity["pos_a"].diff().abs().fillna(equity["pos_a"].abs())
    )
    equity["ret"] = equity["gross_ret"] - turnover * cost_bps / 1e4
    equity["nav"] = (1.0 + equity["ret"]).cumprod()

    # ---- what happened -----------------------------------------------------
    invested = equity["units"] != 0
    years = len(equity) / 252
    total = equity["nav"].iloc[-1] - 1.0
    net_sharpe = sharpe_ratio(equity["ret"], risk_free_rate=0.0)
    sizing = ("linear, -z units" if args.linear
              else f"banded, enter |z| >= {entry_zscore:g}, exit |z| <= {exit_zscore:g}")
    print(f"{sym_b} - beta x {sym_a}, {W}-day window, {sizing}, "
          f"{args.normalize} base, {cost_bps:g} bps\n")
    print(f"  observations     {len(equity)} ({years:.1f} years)")
    print(f"  in the market    {invested.mean():.1%} of days")
    print(f"  gross exposure   mean {(equity['pos_b'].abs() + equity['pos_a'].abs()).mean():.2f}x, "
          f"peak {(equity['pos_b'].abs() + equity['pos_a'].abs()).max():.2f}x")
    print(f"  position changes {int((equity['units'].diff().fillna(equity['units']) != 0).sum())}")
    print(f"  turnover         {turnover.sum():.0f}x gross, "
          f"{(turnover * cost_bps / 1e4).sum():.2%} paid away")
    print(f"  total return     {total:+.2%}")
    print(f"  annualised       {(1 + total) ** (1 / years) - 1:+.2%}")
    print(f"  Sharpe           {net_sharpe:.2f}   "
          f"(before costs {sharpe_ratio(equity['gross_ret'], risk_free_rate=0.0):.2f})")
    print(f"  Sortino          {sortino_ratio(equity['ret'], risk_free_rate=0.0):.2f}")
    print(f"  max drawdown     {max_drawdown(equity['nav']):.2%}")

    z_panel = f"z-score — dashed lines are the entry band at +/-{entry_zscore:g}"
    written = plot_panels(
        {
            z_panel: equity["zscore"],
            "units held — positive is long the spread, negative short": equity["units"],
            "equity — $1 of gross exposure, net of costs": equity["nav"],
        },
        path=f"{sym_b}_{sym_a}_backtest.png",
        title=f"{sym_b} / {sym_a} pair trade — {total:+.1%} net over {years:.1f} years",
        subtitle=f"{W}-day window   ·   {sizing}   ·   {args.normalize} base   ·   "
                 f"{cost_bps:g} bps   ·   Sharpe {net_sharpe:.2f}",
        hlines={
            z_panel: (-entry_zscore, exit_zscore, entry_zscore),
            "units held — positive is long the spread, negative short": (0.0,),
            "equity — $1 of gross exposure, net of costs": (1.0,),
        },
    )
    print(f"\n  plot             {written}")
    plt.close("all")

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
