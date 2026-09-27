import pandas as pd
import numpy as np

def sma(close: pd.Series, window: int) -> pd.Series:
    return close.rolling(window=window).mean()

def ema(close: pd.Series, span: int) -> pd.Series:
    return close.ewm(span=span, adjust=False).mean()    # do not adjust in the very first few values

def rsi(close: pd.Series, window: int=14) -> pd.Series:
    delta = close.diff()
    gains = delta.where(delta > 0, 0.0)
    losses = -delta.where(delta < 0, 0.0)

    avg_gain = gains.rolling(window=window).mean()
    avg_loss = losses.rolling(window=window).mean

    relative_strength = avg_gain / avg_loss
    return 100 - (100 / (1 + relative_strength))

def atr(df: pd.DataFrame, window: int = 14) -> pd.Series:
    """Average True Range"""
    high_low = df["High"] - df["Low"]
    high_close_prev = (df["High"] - df["Close"].shift(1)).abs()
    low_close_prev = (df["Low"] - df["Close"].shift(1)).abs()

    true_range = pd.concat([high_low, high_close_prev, low_close_prev], axis=1).max(axis=1)
    return true_range.rolling(window=window).mean()