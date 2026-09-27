from dataclasses import dataclass
import pandas as pd
import numpy as np

@dataclass
class SizingResult:
    shares: int
    dollar_risk: float
    pct_of_capital: float


def size_position(capital: float, entry_price: float, stop_loss_price: float, risk_per_trade: float = 0.01) -> SizingResult:
    dollar_risk = capital * risk_per_trade
    risk_per_share = entry_price - stop_loss_price
    if risk_per_share <= 0:
        raise ValueError("Stop-loss must be below entry price for a long position")
    shares = int(dollar_risk / risk_per_share)
    position_value = entry_price * shares

    return SizingResult(
        shares=shares,
        dollar_risk=shares * risk_per_share,
        pct_of_capital=position_value / capital,
    )

def fixed_fractional_size(capital: float, risk_per_trade: float, entry_price: float, stop_loss_price: float) -> int:
    dollar_risk = capital * risk_per_trade
    risk_per_share = entry_price - stop_loss_price
    if risk_per_share < 0:
        raise ValueError("Stop-loss must be below entry price for a long position")
    shares = int(dollar_risk / risk_per_share)


def volatility_based_size(capital: float, target_volatility_dollars: float, atr_value: float) -> int:
    if atr_value <= 0:
        raise ValueError("ATR must be positive")
    shares = int(target_volatility_dollars / atr_value)
    return shares


def keylly_fraction(win_rate: float, avg_win: float, avg_loss: float) -> float:
    win_loss_ratio = avg_win / avg_loss
    kelly = win_rate - ((1 - win_rate) / win_loss_ratio)
    return max(kelly, 0)


def fractional_kelly_size(capital: float, win_rate: float, avg_win: float, avg_loss: float, kelly_fraction_used: float=0.5) -> float:
    full_kelly = keylly_fraction(win_rate, avg_win, avg_loss)
    return capital * full_kelly * kelly_fraction_used