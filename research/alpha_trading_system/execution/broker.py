from abc import ABC, abstractmethod
from dataclasses import dataclass


@dataclass
class OrderResult:
    order_id: str
    symbol: str
    shares: int
    side: str
    status: str


class BrokerInterface(ABC):
    @abstractmethod
    def get_buying_power(self) -> float:
        raise NotImplementedError
    
    @abstractmethod
    def submit_market_order(self, symbol: str, shares: int, side: str) -> OrderResult:
        raise NotImplementedError
    
    @abstractmethod
    def get_equity(self) -> float:
        """Account equity -- cash plus the market value of open positions.

        This, not buying power, is what a loss limit measures against: buying
        power is levered and falls when you open a position, so a loss limit
        watching it would trip on its own first trade.
        """
        raise NotImplementedError

    @abstractmethod
    def get_open_positions(self) -> list[dict]:
        raise NotImplementedError

    @abstractmethod
    def get_position(self, symbol: str) -> dict | None:
        raise NotImplementedError

    @abstractmethod
    def close_position(self, symbol: str) -> OrderResult:
        raise NotImplementedError

    @abstractmethod
    def is_market_open(self) -> bool:
        raise NotImplementedError

