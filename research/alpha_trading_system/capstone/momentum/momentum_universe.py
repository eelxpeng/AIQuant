

UNIVERSE = ["AAPL", "MSFT", "GOOGL", "AMZN", "NVDA", "JNJ", "XOM", "JPM", "PG", "UNH"]

from data.store import get_price_history

def build_universe_data(symbols: list[str], period: str = "5y") -> dict:
    data = {}
    for symbol in symbols:
        data[symbol] = get_price_history(symbol, period=period)
    return data
