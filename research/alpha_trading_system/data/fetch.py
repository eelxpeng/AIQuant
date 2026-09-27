import yfinance as yf
import pandas as pd
import matplotlib.pyplot as plt

def fetch_daily_history(symbol: str, period: str = "1y") -> pd.DataFrame:
    ticker = yf.Ticker(symbol)
    history = ticker.history(period=period)
    if history.empty:
        raise ValueError(f"No data returned for symbol {symbol}")
    return history

if __name__ == "__main__":
    df = fetch_daily_history("AAPL", period="6mo")
    print(df.tail())
    print(f"\nColumns: {list(df.columns)}")

    plt.figure(figsize=(10,5))
    plt.plot(df.index, df["Close"], color="#1D3557", linewidth=1.5)
    plt.title("AAPL -- 6-Month Daily Close")
    plt.xlabel("Date")
    plt.ylabel("Price ($)")
    plt.grid(alpha=0.3)
    plt.tight_layout()
    plt.show()

