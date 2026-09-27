from abc import ABC, abstractmethod
import pandas as pd

class Strategy(ABC):
    def __init__(self, name: str):
        self.name = name
    
    @abstractmethod
    def generate_signals(self, df: pd.DataFrame) -> pd.DataFrame:
        raise NotImplementedError
    
    def validate_no_lookahead(self, df: pd.DataFrame) -> bool:
        same_bar_return = df["Close"].pct_change()
        correlation = df["signal"].corr(same_bar_return)
        if abs(correlation) > 0.5:
            print(f"Warning: {self.name} signal is suspiciously correlated with same-bar returns "
                  f"with same-bar returns ({correlation:.2f}) -- check for lookahead bias")
        
        return abs(correlation) <= 0.5
    
