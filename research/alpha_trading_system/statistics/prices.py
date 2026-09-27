"""Shared plumbing for the statistics CLIs: symbol mapping and price loading."""

import argparse

import pandas as pd

DEFAULT_SYMBOL = "USD.CAD"


def to_yahoo_symbol(symbol: str) -> str:
    """Map an IB-style FX pair ("USD.CAD") onto a Yahoo ticker ("USDCAD=X")."""
    s = symbol.strip().upper()
    base, dot, quote = s.partition(".")
    if dot and len(base) == 3 and len(quote) == 3 and base.isalpha() and quote.isalpha():
        return f"{base}{quote}=X"
    return s


def to_trading_days(series: pd.Series) -> pd.Series:
    """Re-stamp a daily series on plain dates, dropping timezone and time.

    Yahoo stamps each daily bar at local midnight *in the listing venue's own
    timezone*: a US equity lands on ``America/New_York``, an FX pair on
    ``Europe/London``. Those are the same trading day and yet never compare
    equal, so joining two such series on the raw index silently yields zero
    overlapping rows. Normalising first is what makes them line up.
    """
    index = series.index
    if isinstance(index, pd.DatetimeIndex):
        if index.tz is not None:
            index = index.tz_localize(None)
        series = series.copy()
        series.index = index.normalize()
    return series


def load_close(symbol: str, period: str = "5y", refresh: bool = False, start=None, end=None):
    """Return ``(yahoo_ticker, close_series)`` for ``symbol``, indexed by date.

    ``start``/``end`` pin a fixed historical range and override ``period``.
    """
    from data.store import get_price_history  # deferred: pulls in yfinance

    ticker = to_yahoo_symbol(symbol)
    df = get_price_history(ticker, period=period, force_refresh=refresh, start=start, end=end)
    return ticker, to_trading_days(df["Close"].dropna())


def load_closes(symbols, period: str = "5y", refresh: bool = False, start=None, end=None):
    """Load several symbols and align them on the days they all traded.

    Returns ``(tickers, frame)`` -- a ``{symbol: yahoo_ticker}`` map and a
    DataFrame of closes whose columns are the symbols as given, inner-joined
    so every row is a day every symbol traded.
    """
    tickers, closes = {}, {}
    for symbol in symbols:
        tickers[symbol], closes[symbol] = load_close(symbol, period, refresh, start, end)
    frame = pd.concat(closes, axis=1, join="inner").dropna()
    if start is not None or end is not None:
        # Belt and braces: a cached file may be wider than the range asked for.
        frame = frame.loc[pd.Timestamp(start) if start else None:
                          pd.Timestamp(end) if end else None]
    return tickers, frame


def header(symbol: str, ticker: str, close: pd.Series) -> str:
    return (
        f"{symbol} ({ticker})  {len(close)} daily closes  "
        f"{close.index[0].date()} to {close.index[-1].date()}  "
        f"last {close.iloc[-1]:.5f}"
    )


def add_common_flags(parser: argparse.ArgumentParser) -> argparse.ArgumentParser:
    """Add the flags every statistics CLI shares."""
    parser.add_argument("--period", default="5y", help="history window (default: %(default)s)")
    parser.add_argument("--start", default=None, metavar="YYYY-MM-DD",
                        help="first date; with --end, pins a fixed range and overrides --period")
    parser.add_argument("--end", default=None, metavar="YYYY-MM-DD",
                        help="last date, inclusive")
    parser.add_argument(
        "--alpha", type=float, default=0.05, help="significance level (default: %(default)s)"
    )
    parser.add_argument("--refresh", action="store_true", help="bypass the parquet cache")
    return parser


def base_parser(description: str) -> argparse.ArgumentParser:
    """A parser for the single-symbol CLIs."""
    parser = argparse.ArgumentParser(
        description=description, epilog="Run from the repo root."
    )
    parser.add_argument(
        "symbol",
        nargs="?",
        default=DEFAULT_SYMBOL,
        help='symbol to test; FX pairs may be written "USD.CAD" (default: %(default)s)',
    )
    return add_common_flags(parser)
