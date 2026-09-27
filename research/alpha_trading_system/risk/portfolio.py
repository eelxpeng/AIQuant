from dataclasses import dataclass, field

@dataclass
class OpenPosition:
    symbol: str
    dollar_risk: float

@dataclass
class Portfolio:
    capital: float
    open_positions: list[OpenPosition] = field(default_factory=list)

    def portfolio_heat(self) -> float:
        total_risk = sum(p.dollar_risk for p in self.open_positions)
        return total_risk / self.capital

