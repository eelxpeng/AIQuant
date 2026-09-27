import pandas as pd

def compute_momentum_scores(universe_data: dict, lookback_days: int = 126) -> pd.DataFrame:
    scores = {}
    for symbol, df in universe_data.items():
        momentum = df["Close"].shift(1).pct_change(lookback_days)
        scores[symbol] = momentum
    
    return pd.DataFrame(scores)
