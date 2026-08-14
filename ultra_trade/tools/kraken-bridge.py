#!/usr/bin/env python3
"""Bridge Kraken's public market data into the feed protocol.

    python3 tools/kraken-bridge.py XBTUSD > /tmp/md

This is the venue-specific half of a live feed, and it deliberately lives
outside the Rust workspace. Every venue's wire format differs, so the normalized
side (`crates/adapters/live`) takes one protocol and the venue side is a script
like this one. Nothing here is linked into the trading system; it only writes
lines to stdout.

Standard library only — no pip install, no TLS stack in the trading binary.

Protocol emitted, one event per line:

    Q <symbol> <exchange_nanos> <bid_px> <bid_qty> <ask_px> <ask_qty>
    T <symbol> <exchange_nanos> <px> <qty> <B|S>

Two honest limitations of polling REST rather than reading a websocket:

  Quotes carry no venue timestamp. Kraken's Ticker endpoint does not provide
  one, so a quote is stamped with this bridge's observation time. That is not
  the venue's clock. It is close enough to build a book from and it is NOT
  suitable for measuring latency — a real latency budget needs a feed that
  timestamps at the source.

  Quotes are sampled, not streamed. Between two polls the book moved and this
  did not see it. Trades are not sampled: the Trades endpoint returns every
  trade since a cursor, so no trade is missed after the first poll.

  The first poll's trades are discarded. Kraken answers a cursor-less request
  with its last thousand trades, reaching over an hour back, and emitting them
  would replay an hour of history at full speed into a live session. A strategy
  that needs warming up should be warmed on a recorded session, not have
  history smuggled in as live data.

Numbers are never parsed through a float. `json.loads(parse_float=str)` keeps
them as text, and the decimal is converted to integer nanoseconds and passed
through as written — the same discipline the Rust side enforces, for the same
reason.
"""

import argparse
import json
import sys
import time
import urllib.error
import urllib.request

API = "https://api.kraken.com/0/public"
USER_AGENT = "ultra_trade-kraken-bridge/0.1"


def fetch(path, params):
    """One request, returning parsed JSON with numbers left as strings."""
    query = "&".join(f"{k}={v}" for k, v in params.items())
    request = urllib.request.Request(
        f"{API}/{path}?{query}", headers={"User-Agent": USER_AGENT}
    )
    with urllib.request.urlopen(request, timeout=10) as response:
        body = response.read().decode()
    # parse_float=str: a price must never go through binary floating point.
    payload = json.loads(body, parse_float=str, parse_int=str)
    if payload.get("error"):
        raise RuntimeError(f"kraken said: {payload['error']}")
    return payload["result"]


def seconds_to_nanos(text):
    """Converts a decimal number of seconds to integer nanoseconds, exactly."""
    whole, _, fraction = str(text).partition(".")
    fraction = (fraction + "000000000")[:9]
    return int(whole) * 1_000_000_000 + int(fraction)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pair", nargs="+", help="Kraken pairs, e.g. XBTUSD ETHUSD")
    parser.add_argument(
        "--symbol",
        action="append",
        default=[],
        help="what to call each pair in the feed, in the same order "
        "(defaults to the pair name)",
    )
    parser.add_argument(
        "--interval",
        type=float,
        default=3.0,
        help="seconds between polls; Kraken's public limit is about one "
        "request per second and each poll makes two (default 3)",
    )
    parser.add_argument(
        "--seconds",
        type=float,
        default=0,
        help="stop after this long; 0 runs until killed",
    )
    args = parser.parse_args()
    pairs = args.pair
    names = args.symbol + pairs[len(args.symbol):]
    if len(names) != len(pairs):
        parser.error("give either no --symbol or one per pair")

    started = time.monotonic()
    # Per pair, so one slow or quiet instrument cannot stall another.
    since = {p: None for p in pairs}
    last_quote = {p: None for p in pairs}
    consecutive_failures = 0
    quotes = trades = 0

    print(f"# kraken {list(zip(pairs, names))}, polling every {args.interval}s", flush=True)

    while True:
        if args.seconds and time.monotonic() - started >= args.seconds:
            break

        try:
            for pair, symbol in zip(pairs, names):
                ticker = fetch("Ticker", {"pair": pair})
                key = next(iter(ticker))
                entry = ticker[key]
                # a = [price, whole lot volume, lot volume]; b likewise.
                ask_px, _, ask_qty = entry["a"]
                bid_px, _, bid_qty = entry["b"]

                quote = (bid_px, bid_qty, ask_px, ask_qty)
                if quote != last_quote[pair]:
                    # No venue timestamp on this endpoint, so this is when the
                    # bridge saw it. See the module note.
                    observed = time.time_ns()
                    print(
                        f"Q {symbol} {observed} {bid_px} {bid_qty} {ask_px} {ask_qty}",
                        flush=True,
                    )
                    last_quote[pair] = quote
                    quotes += 1

                params = {"pair": pair}
                if since[pair] is not None:
                    params["since"] = since[pair]
                recent = fetch("Trades", params)
                first_poll = since[pair] is None
                since[pair] = recent.get("last", since[pair])
                for pair_key, rows in recent.items():
                    if pair_key == "last":
                        continue
                    if first_poll:
                        # Without a cursor Kraken hands back its last thousand
                        # trades, which reach over an hour into the past.
                        # Emitting those would replay an hour of history at
                        # full speed and let a strategy trade on prices that
                        # are long gone — the session's first second would be a
                        # backtest wearing a live session's clothes. Take the
                        # cursor, drop the backlog.
                        print(
                            f"# skipped {len(rows)} historical {symbol} trades "
                            "on the first poll",
                            file=sys.stderr,
                            flush=True,
                        )
                        continue
                    for row in rows:
                        # [price, volume, time, buy/sell, order type, misc, id]
                        price, volume, when, side = row[0], row[1], row[2], row[3]
                        print(
                            f"T {symbol} {seconds_to_nanos(when)} {price} {volume} "
                            f"{'B' if side == 'b' else 'S'}",
                            flush=True,
                        )
                        trades += 1

            consecutive_failures = 0

        except (urllib.error.URLError, urllib.error.HTTPError, RuntimeError, KeyError) as e:
            # A bridge that dies on one bad response is useless. A bridge that
            # never gives up hides an outage. Retry a few times, then stop and
            # let the session see the feed end.
            consecutive_failures += 1
            print(f"# poll failed ({consecutive_failures}): {e}", file=sys.stderr, flush=True)
            if consecutive_failures >= 5:
                print("# giving up after five consecutive failures", file=sys.stderr)
                break

        time.sleep(args.interval)

    print(
        f"# bridge stopping: {quotes} quote updates, {trades} trades",
        file=sys.stderr,
        flush=True,
    )


if __name__ == "__main__":
    main()
