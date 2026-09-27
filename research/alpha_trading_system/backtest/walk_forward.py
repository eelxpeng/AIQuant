import pandas as pd
from dataclasses import dataclass
from backtest.engine import BacktestEngine
from backtest.performance import generate_performance_report, PerformanceReport

@dataclass
class WalkForwardWindow:
    train_start: pd.Timestamp
    train_end: pd.Timestamp
    test_start: pd.Timestamp
    test_end: pd.Timestamp
    out_of_sample_report: PerformanceReport

def generate_windows(df: pd.DataFrame, train_days: int, test_days: int, step_days: int) -> list[tuple]:
    windows = []
    start_idx = 0
    while start_idx + train_days + test_days <= len(df):
        train_start = df.index[start_idx]
        train_end = df.index[start_idx + train_days - 1]
        test_start = df.index[start_idx + train_days]
        test_end = df.index[min(start_idx + train_days + test_days - 1, len(df) - 1)]
        windows.append((train_start, train_end, test_start, test_end))
        start_idx += step_days
    
    return windows

def run_walk_forward(df: pd.DataFrame, strategy_factory, train_days: int = 252, test_days: int = 63, step_days: int = 63) -> list[WalkForwardWindow]:
    raw_windows = generate_windows(df, train_days, test_days, step_days)
    results = []

    for train_start, train_end, test_start, test_end in raw_windows:
        test_df = df.loc[test_start:test_end]
        
        strategy = strategy_factory()
        engine = BacktestEngine(strategy, starting_capital=100_000)
        equity = engine.run(test_df, symbol="AAPL")

        returns = equity["equity"].pct_change().dropna()
        report = generate_performance_report(equity["equity"], returns, engine.closed_trades)

        results.append(WalkForwardWindow(
            train_start=train_start,
            train_end=train_end,
            test_start=test_start,
            test_end=test_end,
            out_of_sample_report=report
        ))
    
    return results


if __name__ == "__main__":
    from strategy.moving_average_crossover import MovingAverageCrossover
    from data.store import get_price_history
    
    df = get_price_history("AAPL", period="5y")
    results = run_walk_forward(
        df=df,
        strategy_factory=lambda: MovingAverageCrossover(fast_window=20, slow_window=50),
        train_days=252,
        test_days=63,
        step_days=63,
    )

    for w in results:
        r = w.out_of_sample_report
        print(f"{w.test_start.date()} to {w.test_end.date()}:"
              f"return={r.total_return:+.1%}, sharpe={r.sharpe:.2f}")