import numpy as np
import pandas as pd

def log_returns(close: pd.Series) -> pd.Series:
    return np.log(close / close.shift(1))

def simple_returns(close: pd.Series) -> pd.Series:
    return close.pct_change()