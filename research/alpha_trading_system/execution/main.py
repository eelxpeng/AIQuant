"""Paper-trading loop: one symbol, one strategy, against Alpaca's paper venue.

The shape is deliberately small. Each pass through the loop asks four
questions in order, and any of them can stop the session:

    may I trade at all?   kill switch, daily loss limit, market hours
    what does the strategy say?
    does risk approve, and how big?
    submit, and confirm the fill

Two things worth knowing about the timing. The strategy reads daily bars and
its moving averages are computed on `Close.shift(1)`, so a decision made today
uses only bars that closed before today -- there is no look-ahead even though
the loop runs intraday. And because a daily bar does not change during the
session, the same signal is seen on every pass; what stops it being acted on
twice is the position check, not the signal.
"""

import argparse
import os
import time
from pathlib import Path

from dotenv import load_dotenv

from data.store import get_price_history
from execution.alpaca_broker import AlpacaBroker
from execution.resilient_execution import submit_order_resiliently
from monitoring.failsafes import DailyLossLimit, check_kill_switch, send_heartbeat
from monitoring.logger import StructuredLogger
from risk.manager import PortfolioRiskManager
from risk.portfolio import Portfolio
from risk.position_sizing import size_position
from strategy.moving_average_crossover import MovingAverageCrossover

# The .env sits next to this file, not at the repo root where the loop is run.
load_dotenv(Path(__file__).resolve().parent / ".env")

logger = StructuredLogger("alpha_main")


def verify_paper_mode(broker: AlpacaBroker) -> str:
    """Refuse to run against anything that is not a paper account."""
    account_number = broker.get_account_number()
    if not account_number.startswith("PA"):
        raise RuntimeError(
            "SAFETY STOP: connected account does not look like a paper trading account "
            f"(account {account_number!r} does not start with 'PA'). Refusing to proceed. "
            "Verify your API keys and the 'paper' flag before running this again."
        )
    if os.environ.get("ALPACA_PAPER", "true").strip().lower() not in ("1", "true", "yes"):
        raise RuntimeError("SAFETY STOP: ALPACA_PAPER is not set to true in .env.")
    print(f"Confirmed paper trading account: {account_number}")
    return account_number


def decide(symbol: str, strategy: MovingAverageCrossover, refresh: bool):
    """Latest strategy row for `symbol`.

    `force_refresh` is not optional here: `get_price_history` caches to parquet,
    so without it the loop would re-read the same frozen bars all session and
    never see a new day.
    """
    df = get_price_history(symbol, period="1y", force_refresh=refresh)
    signals = strategy.generate_signals(df)
    return df, signals.iloc[-1]


def run_trading_loop(
    symbol: str = "AAPL",
    poll_seconds: int = 300,
    risk_per_trade: float = 0.01,
    max_daily_loss_pct: float = 0.03,
    once: bool = False,
    dry_run: bool = False,
) -> int:
    broker = AlpacaBroker(
        api_key=os.environ["ALPACA_API_KEY"],
        secret_key=os.environ["ALPACA_SECRET_KEY"],
        paper=True,
    )
    account_number = verify_paper_mode(broker)

    # Equity, not buying power: buying power is levered and drops when a
    # position opens, so a loss limit watching it trips on its own first trade.
    starting_equity = broker.get_equity()
    loss_limit = DailyLossLimit(max_daily_loss_pct=max_daily_loss_pct,
                                starting_capital=starting_equity)
    risk_manager = PortfolioRiskManager()
    strategy = MovingAverageCrossover(fast_window=20, slow_window=50)

    logger.info("session_start", symbol=symbol, account=account_number,
                starting_equity=starting_equity, dry_run=dry_run,
                held_at_start=[p["symbol"] for p in broker.get_open_positions()])

    try:
        while True:
            send_heartbeat()

            if not check_kill_switch():
                logger.warning("kill_switch_activated")
                break

            current_equity = broker.get_equity()
            if not loss_limit.check(current_equity, logger):
                logger.error("trading_halted_daily_loss_limit", equity=current_equity)
                break

            if not broker.is_market_open():
                logger.info("market_closed", next_open=str(broker.next_market_open()))
                if once:
                    break
                time.sleep(poll_seconds)
                continue

            df, latest = decide(symbol, strategy, refresh=True)
            signal = int(latest["signal"])
            position = broker.get_position(symbol)
            logger.info("signal_evaluated", symbol=symbol, signal=signal,
                        fast_ma=float(latest["fast_ma"]), slow_ma=float(latest["slow_ma"]),
                        holding=position["shares"] if position else 0)

            if signal == 1 and position is None:
                entry_price = float(latest["Close"])
                stop_distance = float(latest["stop_loss_distance"])
                sizing = size_position(current_equity, entry_price,
                                       entry_price - stop_distance, risk_per_trade)
                check = risk_manager.evaluate(
                    Portfolio(capital=current_equity), symbol, "technology",
                    current_equity * risk_per_trade,
                    df["Close"].pct_change().dropna().to_frame(),
                    [p["symbol"] for p in broker.get_open_positions()], [],
                )
                logger.info("risk_check", approved=check.approved, reason=check.reason,
                            shares=sizing.shares, entry=entry_price,
                            stop=entry_price - stop_distance)

                if not check.approved:
                    pass
                elif sizing.shares <= 0:
                    logger.warning("sizing_rejected", reason="stop too wide for the risk budget")
                elif sizing.shares * entry_price > broker.get_buying_power():
                    logger.warning("sizing_rejected", reason="insufficient buying power",
                                   needed=sizing.shares * entry_price)
                elif dry_run:
                    logger.info("dry_run_order_skipped", side="buy", shares=sizing.shares)
                else:
                    result = submit_order_resiliently(broker, symbol, sizing.shares, "buy")
                    status = broker.wait_for_fill(result.order_id)
                    logger.info("order_filled", order_id=result.order_id, side="buy",
                                shares=sizing.shares, status=status)

            elif signal == -1 and position is not None:
                if dry_run:
                    logger.info("dry_run_order_skipped", side="sell",
                                shares=position["shares"])
                else:
                    result = broker.close_position(symbol)
                    status = broker.wait_for_fill(result.order_id)
                    logger.info("position_closed", order_id=result.order_id,
                                shares=position["shares"], status=status,
                                unrealized_pl=position["unrealized_pl"])

            if once:
                break
            time.sleep(poll_seconds)

    except KeyboardInterrupt:
        logger.warning("interrupted_by_operator")
    finally:
        final_equity = broker.get_equity()
        logger.info("session_end", final_equity=final_equity,
                    pnl=final_equity - starting_equity,
                    open_positions=broker.get_open_positions())
    return 0


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description="Paper-trade one symbol against Alpaca.")
    parser.add_argument("symbol", nargs="?", default="AAPL")
    parser.add_argument("--poll", type=int, default=300, help="seconds between passes")
    parser.add_argument("--risk", type=float, default=0.01, help="risk per trade, fraction of equity")
    parser.add_argument("--max-daily-loss", type=float, default=0.03)
    parser.add_argument("--once", action="store_true", help="one pass, then exit")
    parser.add_argument("--dry-run", action="store_true",
                        help="evaluate and log, but submit nothing")
    args = parser.parse_args(argv)
    return run_trading_loop(
        symbol=args.symbol, poll_seconds=args.poll, risk_per_trade=args.risk,
        max_daily_loss_pct=args.max_daily_loss, once=args.once, dry_run=args.dry_run,
    )


if __name__ == "__main__":
    raise SystemExit(main())
