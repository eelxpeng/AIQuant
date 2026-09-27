import time

from execution.broker import BrokerInterface, OrderResult
from execution.circuit_breaker import CircuitBreaker

breaker = CircuitBreaker(failure_threshold=5, cooldown_seconds=300)


def submit_order_resiliently(broker: BrokerInterface, symbol: str, shares: int, side: str) -> OrderResult:
    if breaker.is_open():
        raise RuntimeError("Circuit breaker is open -- order submission halted. "
                           "The broker connection has failed repeatedly and needs time to recover.")
    try:
        result = submit_order_with_retry(broker, symbol, shares, side)
        breaker.record_success()
        return result
    except Exception:
        breaker.record_failure()
        raise


def submit_order_with_retry(broker: BrokerInterface, symbol: str, shares: int, side: str,
                            max_retries: int = 3) -> OrderResult:
    last_error = None
    for attempt in range(max_retries):
        try:
            return broker.submit_market_order(symbol, shares, side)
        except Exception as e:
            last_error = e
            if attempt == max_retries - 1:
                break
            wait_time = 2 ** attempt
            print(f"Order attempt {attempt + 1} failed: {e}. Retrying in {wait_time}s...")
            time.sleep(wait_time)
    raise RuntimeError(
        f"Failed to submit order for {symbol} after {max_retries} attempts."
    ) from last_error
