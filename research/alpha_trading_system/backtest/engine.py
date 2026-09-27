import pandas as pd
import matplotlib.pyplot as plt
from backtest.trade import Trade

from risk.portfolio import Portfolio, OpenPosition
from risk.position_sizing import size_position
from risk.manager import PortfolioRiskManager

class BacktestEngine:
    def __init__(self, strategy, starting_capital: float = 100_000,
                 risk_per_trade: float = 0.01, risk_manager: PortfolioRiskManager = None):
        self.strategy = strategy
        self.starting_capital = starting_capital
        self.portfolio = Portfolio(capital=starting_capital)
        self.risk_per_trade = risk_per_trade
        self.risk_manager = risk_manager or PortfolioRiskManager()
        self.open_trades: dict[str, Trade] = {}
        self.closed_trades: list[Trade] = []
        self.equity_curve: list[dict] = []
    
    def check_stops(self, symbol:str, bar: pd.Series, stop_price: float):
        if symbol not in self.open_trades:
            return
        if bar["Low"] <= stop_price:
            trade = self.open_trades.pop(symbol)
            trade.exit_date = bar.name
            trade.exit_price = stop_price
            trade.exit_reason = "stop_loss"
            self.closed_trades.append(trade)
            self.portfolio.capital += trade.pnl
            self.portfolio.open_positions = [
                p for p in self.portfolio.open_positions if p.symbol != symbol
            ]
    
    def _process_signal(self, symbol: str, bar: pd.Series, signal_row: pd.Series, returns_df: pd.DataFrame):
        if signal_row["signal"] == 1 and symbol not in self.open_trades:
            entry_price = bar["Open"]
            stop_price = entry_price - signal_row["stop_loss_distance"]
            sizing = size_position(self.portfolio.capital, entry_price, stop_price, self.risk_per_trade)
            if sizing.shares <= 0:
                return
            
            open_symbols = [p.symbol for p in self.portfolio.open_positions]
            check = self.risk_manager.evaluate(self.portfolio, symbol, "unknown_sector", sizing.dollar_risk, returns_df, open_symbols, [])
            if not check.approved:
                return
            
            self.open_trades[symbol] = Trade(
                symbol=symbol,
                entry_date=bar.name,
                entry_price=entry_price,
                shares=sizing.shares,
            )
            self.portfolio.open_positions.append(
                OpenPosition(
                    symbol=symbol,
                    dollar_risk=sizing.dollar_risk,
                )
            )
        
        elif signal_row["signal"] == -1 and symbol in self.open_trades:
            trade = self.open_trades.pop(symbol)
            trade.exit_date = bar.name
            trade.exit_price = bar["Open"]
            trade.exit_reason = "signal"
            self.closed_trades.append(trade)
            self.portfolio.capital += trade.pnl
            self.portfolio.open_positions = [
                p for p in self.portfolio.open_positions if p.symbol != symbol
            ]
    
    def run(self, df: pd.DataFrame, symbol: str, returns_df: pd.DataFrame = None) -> pd.DataFrame:
        signals = self.strategy.generate_signals(df)
        for i in range(len(signals)):
            bar = signals.iloc[i]

            if symbol in self.open_trades:
                trade = self.open_trades[symbol]
                stop_price = trade.entry_price - bar["stop_loss_distance"]
                self.check_stops(symbol, bar, stop_price)
            
            self._process_signal(symbol, bar, bar, returns_df if returns_df is not None else pd.DataFrame())

            unrealized_pnl = sum(t.shares * (bar["Close"] - t.entry_price) for s, t in self.open_trades.items() if s == symbol)
            total_equity = self.portfolio.capital + unrealized_pnl

            self.equity_curve.append(
                {
                    "date": bar.name,
                    "equity": total_equity,
                }
            )
        
        equity_curve = pd.DataFrame(self.equity_curve).set_index("date")

        # Buy-and-hold benchmark, normalized to the same starting capital.
        # Fractional shares make its first value exactly starting_capital.
        first_close = signals["Close"].iloc[0]
        equity_curve["buy_and_hold"] = (
            self.starting_capital * signals["Close"] / first_close
        )
        return equity_curve


if __name__ == "__main__":
    from data.store import get_price_history
    from strategy.moving_average_crossover import MovingAverageCrossover
    from strategy.null_strategy import NullStrategy
    from backtest.performance import generate_performance_report

    df = get_price_history("AAPL", period="3y")
    strategy = MovingAverageCrossover(fast_window=20, slow_window=50)
    # strategy = NullStrategy()

    engine = BacktestEngine(strategy, starting_capital=100_000, risk_per_trade=0.01)
    equity_curve = engine.run(df, symbol="AAPL")
    returns = equity_curve["equity"].pct_change().dropna()
    performance_report = generate_performance_report(equity_curve["equity"], returns, engine.closed_trades)

    print(f"Starting capital: $100,000")
    print(f"Strategy ending equity: ${equity_curve['equity'].iloc[-1]:,.2f}")
    print(f"Buy-and-hold ending equity: ${equity_curve['buy_and_hold'].iloc[-1]:,.2f}")
    print(f"Completed trades: {len(engine.closed_trades)}")
    print(f"Performance Report: {performance_report}")

    fig, (price_ax, equity_ax) = plt.subplots(
        2, 1, figsize=(11, 8), sharex=True,
        gridspec_kw={"height_ratios": [1, 1.4]},
    )

    price_ax.plot(df.index, df["Close"], color="#457B9D", linewidth=1.3)
    price_ax.set_title("AAPL Price and Backtest Performance")
    price_ax.set_ylabel("AAPL Close ($)")
    price_ax.grid(alpha=0.3)

    equity_ax.plot(
        equity_curve.index, equity_curve["equity"],
        label="MA crossover", color="#1D3557", linewidth=1.5,
    )
    equity_ax.plot(
        equity_curve.index, equity_curve["buy_and_hold"],
        label="Buy and hold AAPL", color="#E76F51", linewidth=1.3,
    )
    equity_ax.axhline(
        engine.starting_capital, color="gray", linestyle="--",
        linewidth=0.9, alpha=0.7, label="Starting capital",
    )
    equity_ax.set_xlabel("Date")
    equity_ax.set_ylabel("Portfolio Equity ($)")
    equity_ax.grid(alpha=0.3)
    equity_ax.legend()

    fig.tight_layout()
    plt.show()
