from pathlib import Path
import pandas as pd
import yfinance as yf

DATA_DIR = Path("data/cache")
DATA_DIR.mkdir(parents=True, exist_ok=True)

DEFAULT_PERIOD = "5y"


def _cache_path(symbol: str, tag: str = "") -> Path:
    # The default period keeps the bare "{SYMBOL}.parquet" name it always had.
    # Anything else carries its own tag, so a 10y or a fixed-date fetch cannot
    # quietly answer a later request for a different window from the cache.
    stem = symbol.upper() if not tag else f"{symbol.upper()}_{tag}"
    return DATA_DIR / f"{stem}.parquet"


def _tag(period: str, start, end) -> str:
    if start or end:
        return f"{start or 'begin'}_{end or 'today'}"
    return "" if period == DEFAULT_PERIOD else period


def fetch_and_clean(symbol: str, period: str = DEFAULT_PERIOD, start=None, end=None) -> pd.DataFrame:
    ticker = yf.Ticker(symbol)
    if start or end:
        # yfinance treats `end` as exclusive; bump it so a caller asking for a
        # date actually gets that date's bar.
        stop = (pd.Timestamp(end) + pd.Timedelta(days=1)).date().isoformat() if end else None
        df = ticker.history(start=start, end=stop, auto_adjust=True)
    else:
        df = ticker.history(period=period, auto_adjust=True)
    if df.empty:
        window = f"{start} to {end}" if (start or end) else f"period {period}"
        raise ValueError(f"No data returned for '{symbol}' over {window}")

    return df


def get_price_history(
    symbol: str,
    period: str = DEFAULT_PERIOD,
    force_refresh: bool = False,
    start=None,
    end=None,
) -> pd.DataFrame:
    """Daily bars for ``symbol``.

    Pass ``period`` for a window measured back from today, or ``start``/``end``
    (``YYYY-MM-DD``) for a fixed historical range -- which is what reproducing a
    published result needs. ``end`` is inclusive.
    """
    cache_file = _cache_path(symbol, _tag(period, start, end))
    if cache_file.exists() and not force_refresh:
        return pd.read_parquet(cache_file)

    df = fetch_and_clean(symbol, period=period, start=start, end=end)
    df.to_parquet(cache_file)

    return df
