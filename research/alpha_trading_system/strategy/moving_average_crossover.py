import pandas as pd
from strategy.base import Strategy
from strategy.indicators import sma, atr

class MovingAverageCrossover(Strategy):
    def __init__(self, fast_window: int = 20, slow_window: int = 50, atr_multiple: float = 2.5):
        super().__init__(name=f"MA Crossover({fast_window}/{slow_window})")
        self.fast_window = fast_window
        self.slow_window = slow_window
        self.atr_multiple = atr_multiple
    
    def generate_signals(self, df: pd.DataFrame) -> pd.DataFrame:
        df = df.copy()
        fast_ma = sma(df["Close"].shift(1), self.fast_window)
        slow_ma = sma(df["Close"].shift(1), self.slow_window)

        df["fast_ma"] = fast_ma
        df["slow_ma"] = slow_ma
        df["ma_spread"] = fast_ma - slow_ma

        crossed_above = (fast_ma > slow_ma) & (fast_ma.shift(1) <= slow_ma.shift(1))
        crossed_below = (fast_ma < slow_ma) & (fast_ma.shift(1) >= slow_ma.shift(1))

        df["signal"] = 0
        df.loc[crossed_above, "signal"] = 1
        df.loc[crossed_below, "signal"] = -1

        df["atr"] = atr(df, window=14)
        df["stop_loss_distance"] = df["atr"] * self.atr_multiple

        return df

