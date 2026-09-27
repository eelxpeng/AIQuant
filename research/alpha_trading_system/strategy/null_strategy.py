import pandas as pd
from strategy.base import Strategy

class NullStrategy(Strategy):
    def __init__(self):
        super().__init__("null")

    def generate_signals(self, df: pd.DataFrame) -> pd.DataFrame:
        df = df.copy()
        df["signal"] = 0
        df["stop_loss_distance"] = 0
        return df