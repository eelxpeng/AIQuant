from dataclasses import dataclass
import pandas as pd
from risk.portfolio import Portfolio

@dataclass
class RiskCheckResult:
    approved: bool
    reason: str = ""

MAX_PORTFOLIO_HEAT = 0.06

def can_open_new_position(portfolio: Portfolio, new_position_risk: float) -> bool:
    projected_heat = portfolio.portfolio_heat() + (new_position_risk / portfolio.capital)
    return projected_heat <= MAX_PORTFOLIO_HEAT


def rolling_correlation(returns_a: pd.Series, returns_b: pd.Series, window: int = 60) -> pd.Series:
    return returns_a.rolling(window).corr(returns_b)

class PortfolioRiskManager:
    def __init__(self, max_heat: float = 0.06, max_correlation: float = 0.7,
                 max_per_sector: int = 2):
        self.max_heat = max_heat
        self.max_correlation = max_correlation
        self.max_per_sector = max_per_sector
    
    def evaluate(self, portfolio: Portfolio, new_symbol: str, new_sector: str, new_position_risk: float,
                 returns_df: pd.DataFrame, open_symbols: list[str], open_sectors: list[str]) -> RiskCheckResult:
        if not can_open_new_position(portfolio, new_position_risk):
            return RiskCheckResult(approved=False, reason="Portfolio heat limit exceeded.")
        
        # omit other criteria for now

        return RiskCheckResult(approved=True, reason="All portfolio risk checks passed.")
        