import numpy as np
import pandas as pd
from dataclasses import dataclass
from backtest.trade import Trade

def sharpe_ratio(returns: pd.Series, risk_free_rate: float = 0.04, periods_per_year: int = 252) -> float:
    """risk-adjusted return"""
    excess_returns = returns - (risk_free_rate / periods_per_year)
    if excess_returns.std() == 0:
        return 0.0
    return (excess_returns.mean() / excess_returns.std()) * np.sqrt(periods_per_year)

def sortino_ratio(returns: pd.Series, risk_free_rate: float = 0.04, periods_per_year: int = 252) -> float:
    excess_returns = returns - (risk_free_rate / periods_per_year)
    downside_returns = excess_returns[excess_returns < 0]
    downside_std = downside_returns.std()
    if downside_std == 0 or pd.isna(downside_std):
        return 0.0
    return (excess_returns.mean() / downside_std) * np.sqrt(periods_per_year)

def max_drawdown(equity_curve: pd.Series) -> float:
    running_max = equity_curve.expanding().max()
    drawdown = (equity_curve - running_max) / running_max
    return drawdown.min()


@dataclass
class PerformanceReport:
    total_return: float
    sharpe: float
    sortino: float
    max_drawdown: float

def generate_performance_report(equity_curve: pd.Series, returns: pd.Series, trades: list[Trade], periods_per_year: int = 252) -> PerformanceReport:
    return PerformanceReport(
        total_return=(equity_curve.iloc[-1] / equity_curve.iloc[0]) - 1,
        sharpe=sharpe_ratio(returns, periods_per_year=periods_per_year),
        sortino=sortino_ratio(returns, periods_per_year=periods_per_year),
        max_drawdown=max_drawdown(equity_curve)
    )