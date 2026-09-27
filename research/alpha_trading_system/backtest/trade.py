from dataclasses import dataclass
from datetime import datetime

@dataclass
class Trade:
    symbol: str
    entry_date: datetime
    entry_price: float
    shares: int
    exit_date: datetime = None
    exit_price: float = None
    exit_reason: str = None

    @property
    def is_open(self) -> bool:
        return self.exit_date is None
    
    @property
    def pnl(self) -> float:
        if self.is_open:
            raise ValueError("Cannot compute P&L for a still-open trade")
        return (self.exit_price - self.entry_price) * self.shares
    
    @property
    def return_pct(self) -> float:
        if self.is_open:
            raise ValueError("Cannot compute P&L for a still-open trade")
        return (self.exit_price - self.entry_price) / self.entry_price

