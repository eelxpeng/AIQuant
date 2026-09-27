from strategy.base import Strategy
from strategy.indicators import atr
import pandas as pd
from risk.manager import PortfolioRiskManager
from risk.position_sizing import SizingResult

def simple_size_position(capital: float, entry_price: float, stop_price: float, risk_per_trade: float = 0.01) -> SizingResult:
    return SizingResult(
        shares=capital / entry_price,
        dollar_risk=capital,
        pct_of_capital=1.0,
    )


def size_momentum_positions(capital: float, selected_symbols: list[str], universe_data: dict,
                            date: pd.Timestamp, risk_per_trade: float = 0.01) -> dict:
    sizes = {}
    for symbol in selected_symbols:
        df = universe_data[symbol].loc[:date]
        entry_price = df["Close"].iloc[-1]
        atr_value = atr(df).iloc[-1]
        stop_price = entry_price - (atr_value * 2.5)
        sizing = simple_size_position(capital / len(selected_symbols), entry_price, stop_price, risk_per_trade)
        sizes[symbol] = sizing

    return sizes


class MomentumRotationStrategy:
    def __init__(self, universe_data: dict, top_n: int = 3, lookback_days: int = 126, rebalance_freq: str = "MS", atr_multiple: float = 2.5):
        self.universe_data = universe_data
        self.top_n = top_n
        self.lookback_days = lookback_days
        self.rebalance_freq = rebalance_freq
        self.atr_multiple = atr_multiple

    def generate_rebalance_schedule(self) -> pd.DatetimeIndex:
        any_df = next(iter(self.universe_data.values()))
        return pd.date_range(any_df.index.min(), any_df.index.max(), freq=self.rebalance_freq)
    
    def rank_at_date(self, momentum_scores: pd.DataFrame, date: pd.Timestamp) -> list[str]:
        available = momentum_scores.loc[:date].iloc[-1].dropna()
        ranked = available.sort_values(ascending=False)
        return list(ranked.head(self.top_n).index)
    
