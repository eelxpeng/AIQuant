import time
from datetime import datetime

from alpaca.trading.client import TradingClient
from alpaca.trading.enums import OrderSide, TimeInForce
from alpaca.trading.requests import MarketOrderRequest

from execution.broker import BrokerInterface, OrderResult

# Statuses an order stops moving from.
TERMINAL_STATUSES = {"filled", "canceled", "expired", "rejected", "done_for_day"}


class AlpacaBroker(BrokerInterface):
    def __init__(self, api_key: str, secret_key: str, paper: bool = True):
        if not paper:
            raise ValueError("AlpacaBroker refuses to initialize with paper=False directly.")

        self.client = TradingClient(api_key=api_key, secret_key=secret_key, paper=paper)

    # --- account -----------------------------------------------------------
    def get_buying_power(self) -> float:
        # `buying_power` is a field, not a method.
        return float(self.client.get_account().buying_power)

    def get_equity(self) -> float:
        return float(self.client.get_account().equity)

    def get_account_number(self) -> str:
        return self.client.get_account().account_number

    # --- market hours ------------------------------------------------------
    def is_market_open(self) -> bool:
        return bool(self.client.get_clock().is_open)

    def next_market_open(self) -> datetime:
        return self.client.get_clock().next_open

    def next_market_close(self) -> datetime:
        return self.client.get_clock().next_close

    # --- positions ---------------------------------------------------------
    def get_open_positions(self) -> list[dict]:
        return [
            {
                "symbol": p.symbol,
                # qty arrives as a string and can be fractional.
                "shares": float(p.qty),
                "market_value": float(p.market_value),
                "unrealized_pl": float(p.unrealized_pl),
                "avg_entry_price": float(p.avg_entry_price),
            }
            for p in self.client.get_all_positions()
        ]

    def get_position(self, symbol: str) -> dict | None:
        wanted = symbol.upper()
        for position in self.get_open_positions():
            if position["symbol"] == wanted:
                return position
        return None

    # --- orders ------------------------------------------------------------
    def submit_market_order(self, symbol: str, shares: int, side: str) -> OrderResult:
        order_request = MarketOrderRequest(
            symbol=symbol,
            qty=shares,
            side=OrderSide.BUY if side == "buy" else OrderSide.SELL,
            time_in_force=TimeInForce.DAY,
        )
        order = self.client.submit_order(order_request)
        return OrderResult(
            order_id=str(order.id), symbol=symbol, shares=shares,
            side=side, status=order.status.value,
        )

    def close_position(self, symbol: str) -> OrderResult:
        """Liquidate the whole position. Alpaca sizes the order itself."""
        order = self.client.close_position(symbol)
        return OrderResult(
            order_id=str(order.id), symbol=symbol, shares=int(float(order.qty or 0)),
            side="sell", status=order.status.value,
        )

    def wait_for_fill(self, order_id: str, timeout_seconds: int = 60) -> str:
        """Poll until the order stops moving. Returns its final status.

        A submitted order comes back 'accepted', not 'filled'. Without this the
        log records an intention and the next loop reads a position that is not
        there yet.
        """
        deadline = time.time() + timeout_seconds
        status = "unknown"
        while time.time() < deadline:
            order = self.client.get_order_by_id(order_id)
            status = order.status.value
            if status in TERMINAL_STATUSES:
                return status
            time.sleep(1)
        return status
