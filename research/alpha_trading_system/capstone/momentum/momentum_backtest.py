import pandas as pd
from capstone.momentum_strategy import MomentumRotationStrategy
from capstone.momentum_signal import compute_momentum_scores
from capstone.momentum_strategy import size_momentum_positions

def backtest_momentum_rotation(universe_data: dict, strategy: MomentumRotationStrategy, starting_capital: float = 100_000) -> pd.DataFrame:
    momentum_scores = compute_momentum_scores(universe_data, strategy.lookback_days)
    rebalance_dates = strategy.generate_rebalance_schedule()

    capital = starting_capital
    holdings = {}
    equity_curve = []

    benchmark_holdings = {}
    for symbol in universe_data:
        date = rebalance_dates[0]
        entry_price = universe_data[symbol].loc[:date]["Close"].iloc[-1]
        benchmark_holdings[symbol] = starting_capital / len(universe_data) / entry_price

    for i, date in enumerate(rebalance_dates[:-1]):
        next_date = rebalance_dates[i + 1]

        for symbol, shares in holdings.items():
            exit_price = universe_data[symbol].loc[:date]["Close"].iloc[-1]
            capital += shares * exit_price
        
        holdings = {}
        top_symbols = strategy.rank_at_date(momentum_scores, date)
        sizes = size_momentum_positions(capital, top_symbols, universe_data, date)

        for symbol, sizing in sizes.items():
            entry_price = universe_data[symbol].loc[:date]["Close"].iloc[-1]
            holdings[symbol] = sizing.shares
            capital -= sizing.shares * entry_price
        
        period_end_value = capital + sum(shares * universe_data[s].loc[:next_date]["Close"].iloc[-1]
                                         for s, shares in holdings.items())
        benchmark_value = sum(universe_data[s].loc[:next_date]["Close"].iloc[-1] * shares for s, shares in benchmark_holdings.items())
        equity_curve.append({"date": next_date, "equity": period_end_value, "benchmark": benchmark_value})

    return pd.DataFrame(equity_curve).set_index("date")

if __name__ == "__main__":
    import matplotlib.pyplot as plt
    from capstone.momentum_universe import build_universe_data, UNIVERSE
    from backtest.performance import generate_performance_report

    universe_data = build_universe_data(UNIVERSE)
    strategy = MomentumRotationStrategy(universe_data=universe_data)

    equity = backtest_momentum_rotation(universe_data=universe_data, strategy=strategy)
    returns = equity["equity"].pct_change().dropna()
    benchmark_returns = equity["benchmark"].pct_change().dropna()

    report = generate_performance_report(equity["equity"], returns, [], periods_per_year=12)
    benchmark_report = generate_performance_report(equity["benchmark"], benchmark_returns, [], periods_per_year=12)

    print(f"Total return: {report.total_return:.1%}")
    print(f"Sharpe ratio: {report.sharpe:.2f}")

    print(f"Benchmark Total return: {benchmark_report.total_return:.1%}")
    print(f"Benchmark Sharpe ratio: {benchmark_report.sharpe:.2f}")

    plt.figure(figsize=(11, 8))
    plt.plot(equity.index, equity["equity"], label="Momentum Rotation Strategy", color="#457B9D", linewidth=1.5)
    plt.plot(
        equity.index, equity["benchmark"],
        label="Equal-Weight Buy & Hold (benchmark)", color="#E76F51", linewidth=1.5, linestyle='--'
    )
    plt.grid(alpha=0.3)
    plt.legend()
    plt.show()
