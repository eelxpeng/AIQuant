#!/usr/bin/env python3
"""Stream Kraken's book and trades as the feed line protocol.

    python3 tools/kraken-stream.py BTC/USD ETH/USD > /tmp/md &
    python3 tools/kraken-stream.py BTC/USD --depth 10 > /tmp/md &   # with depth
    cargo run -p paper -- session.conf /tmp/md live.log

The polling bridge (`kraken-bridge.py`) asks Kraken for the book every few
seconds, which leaves the data about six seconds behind the market. This one
holds a websocket open and prints an event when the market produces one. Same
output, so nothing downstream changes; only the lag does.

Why it is here and not in the Rust workspace
--------------------------------------------
Every venue's wire format is different, so a venue-specific half is
unavoidable. Keeping it outside the workspace means a TLS and websocket stack
never enters a real-money binary as a side effect of wanting market data. The
trading system still has zero third-party dependencies, and a bridge for
another venue is a new script rather than a new crate.

Dependencies
------------
None. The websocket client below is about a hundred lines of RFC 6455 against
`socket` and `ssl`, which is less than the cost of making this script's
environment a thing anyone has to set up.

Prices are never parsed as floats
---------------------------------
`json.loads(..., parse_float=str)` hands back the exact decimal text Kraken
published. Parsing `0.00018080` into a double and formatting it again produces
`0.0001808`, and a quantity that changed on the way through the bridge is
exactly the class of bug the fixed-point types exist to prevent.
"""

import argparse
import base64
import calendar
import json
import os
import socket
import ssl
import struct
import sys
import zlib
import time

HOST = "ws.kraken.com"
PATH = "/v2"


# ---------------------------------------------------------------- websocket


class Closed(Exception):
    """The peer went away, or the frame stream stopped making sense."""


class Socket:
    """A websocket client: connect, send text, read text frames."""

    def __init__(self, host, path, timeout):
        context = ssl.create_default_context()
        raw = socket.create_connection((host, 443), timeout=timeout)
        self.sock = context.wrap_socket(raw, server_hostname=host)
        self.sock.settimeout(timeout)
        self.buf = b""

        nonce = base64.b64encode(os.urandom(16)).decode()
        self.sock.sendall(
            (
                f"GET {path} HTTP/1.1\r\n"
                f"Host: {host}\r\n"
                f"Upgrade: websocket\r\n"
                f"Connection: Upgrade\r\n"
                f"Sec-WebSocket-Key: {nonce}\r\n"
                f"Sec-WebSocket-Version: 13\r\n\r\n"
            ).encode()
        )
        while b"\r\n\r\n" not in self.buf:
            self._fill()
        head, _, self.buf = self.buf.partition(b"\r\n\r\n")
        status = head.split(b"\r\n")[0]
        if b" 101" not in status:
            raise Closed(f"handshake refused: {status.decode(errors='replace')}")

    def _fill(self):
        chunk = self.sock.recv(65536)
        if not chunk:
            raise Closed("the peer closed the connection")
        self.buf += chunk

    def _need(self, n):
        while len(self.buf) < n:
            self._fill()

    def send(self, text):
        payload = text.encode()
        mask = os.urandom(4)
        n = len(payload)
        if n < 126:
            header = struct.pack("!BB", 0x81, 0x80 | n)
        elif n < 65536:
            header = struct.pack("!BBH", 0x81, 0x80 | 126, n)
        else:
            header = struct.pack("!BBQ", 0x81, 0x80 | 127, n)
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def _pong(self, payload):
        mask = os.urandom(4)
        header = struct.pack("!BB", 0x8A, 0x80 | len(payload))
        masked = bytes(b ^ mask[i % 4] for i, b in enumerate(payload))
        self.sock.sendall(header + mask + masked)

    def texts(self):
        """Yields the payload of every text frame until the peer goes away."""
        while True:
            self._need(2)
            opcode = self.buf[0] & 0x0F
            length = self.buf[1] & 0x7F
            offset = 2
            if length == 126:
                self._need(4)
                length = struct.unpack("!H", self.buf[2:4])[0]
                offset = 4
            elif length == 127:
                self._need(10)
                length = struct.unpack("!Q", self.buf[2:10])[0]
                offset = 10
            self._need(offset + length)
            payload = self.buf[offset : offset + length]
            self.buf = self.buf[offset + length :]

            if opcode == 0x1:
                yield payload.decode()
            elif opcode == 0x9:
                # A ping we do not answer is a connection the venue drops.
                self._pong(payload)
            elif opcode == 0x8:
                raise Closed("the peer sent a close frame")

    def close(self):
        try:
            self.sock.close()
        except OSError:
            pass


# ---------------------------------------------------------------- the feed


def nanos(stamp):
    """`2026-08-17T04:00:50.582749Z` to nanoseconds since the epoch.

    Parsed by hand rather than through `datetime`, which holds microseconds and
    would silently drop the last three digits of a nanosecond timestamp. Kraken
    publishes microseconds today; a venue that publishes more should not lose
    them to this function.
    """
    date, _, rest = stamp.partition("T")
    rest = rest.rstrip("Z")
    hms, _, frac = rest.partition(".")
    y, mo, d = (int(p) for p in date.split("-"))
    h, mi, s = (int(p) for p in hms.split(":"))
    # Whole seconds through the calendar library, which knows about leap years
    # so this function does not have to. Only the sub-second part is hand-rolled,
    # and only because `datetime` would round it to microseconds.
    seconds = calendar.timegm((y, mo, d, h, mi, s, 0, 0, 0))
    frac = (frac + "000000000")[:9]
    return seconds * 1_000_000_000 + int(frac)


def emit(line):
    print(line, flush=True)


def note(message):
    print(f"# {message}", file=sys.stderr, flush=True)


# --------------------------------------------------------------- checksums


def field(text, precision):
    """A level's price or quantity as the checksum wants it.

    Fixed number of decimals, decimal point removed, leading zeros stripped.
    Done on the venue's own decimal text rather than a float, because the
    checksum is over the exact digits it published and a round trip through a
    double changes them.
    """
    whole, _, frac = text.partition(".")
    frac = (frac + "0" * precision)[:precision]
    return (whole + frac).lstrip("0") or "0"


def book_checksum(book, price_precision, qty_precision):
    """Kraken's CRC32 over the top ten of each side, asks first.

    Verified against 490 consecutive live updates with no mismatch before this
    was relied on for anything — an incorrect implementation would report the
    book as broken on every message and resynchronise for ever.
    """
    parts = []
    for side in ("asks", "bids"):
        for level in book[side][:10]:
            parts.append(field(level["price"], price_precision))
            parts.append(field(level["qty"], qty_precision))
    return zlib.crc32("".join(parts).encode())


def apply_levels(book, key, updates):
    """Folds one side's deltas in. A quantity of zero removes the level."""
    side = book[key]
    for update in updates:
        price = update["price"]
        side[:] = [level for level in side if level["price"] != price]
        if update["qty"] != "0" and float(update["qty"]) > 0:
            side.append(update)
    side.sort(key=lambda level: float(level["price"]), reverse=(key == "bids"))
    del side[10:]


def symbol_of(pair):
    """`BTC/USD` to `BTCUSD`, which is what a session config declares."""
    return pair.replace("/", "")


def stream(pairs, seconds, depth=0):
    """One connection's worth of events. Returns when it drops."""
    ws = Socket(HOST, PATH, timeout=30)
    try:
        if depth > 0:
            # The precisions the checksum is computed at. Without them the
            # check cannot run, and it is skipped rather than guessed.
            ws.send(
                json.dumps({"method": "subscribe", "params": {"channel": "instrument"}})
            )
            # Depth supersedes the ticker: the book's top follows from its
            # levels, so subscribing to both would set the same top twice.
            ws.send(
                json.dumps(
                    {
                        "method": "subscribe",
                        "params": {
                            "channel": "book",
                            "symbol": pairs,
                            "depth": depth,
                        },
                    }
                )
            )
        else:
            ws.send(
                json.dumps(
                    {
                        "method": "subscribe",
                        "params": {
                            "channel": "ticker",
                            "symbol": pairs,
                            # Book updates, not trade prints: the engine's book
                            # is what the strategies read and the risk gate ages.
                            "event_trigger": "bbo",
                        },
                    }
                )
            )
        ws.send(
            json.dumps(
                {"method": "subscribe", "params": {"channel": "trade", "symbol": pairs}}
            )
        )

        deadline = None if seconds is None else time.time() + seconds
        # A book snapshot carries no timestamp of its own, so it inherits the
        # last one seen rather than being stamped with a local clock — a receive
        # time in an exchange-time field is the mixing the types forbid.
        last_seen = 0
        # Our copy of each book, kept only so the venue's checksum can be
        # checked against it. Nothing downstream reads it.
        books = {}
        precisions = {}
        for text in ws.texts():
            if deadline is not None and time.time() > deadline:
                return
            message = json.loads(text, parse_float=str)
            channel = message.get("channel")
            if channel == "ticker":
                for row in message.get("data", []):
                    emit(
                        "Q {} {} {} {} {} {}".format(
                            symbol_of(row["symbol"]),
                            nanos(row["timestamp"]),
                            row["bid"],
                            row["bid_qty"],
                            row["ask"],
                            row["ask_qty"],
                        )
                    )
            elif channel == "book":
                snapshot = message.get("type") == "snapshot"
                for row in message.get("data", []):
                    pair = row["symbol"]
                    symbol = symbol_of(pair)
                    stamp = nanos(row["timestamp"]) if "timestamp" in row else last_seen
                    last_seen = stamp

                    book = books.setdefault(pair, {"bids": [], "asks": []})
                    if snapshot:
                        # A snapshot replaces the book, so anything downstream
                        # is holding must go first. The same line covers a first
                        # subscription and a resynchronisation, which means
                        # neither can leave a stale level behind.
                        book["bids"].clear()
                        book["asks"].clear()
                        emit(f"R {symbol} {stamp}")
                    apply_levels(book, "bids", row.get("bids", []))
                    apply_levels(book, "asks", row.get("asks", []))

                    # One line per changed level, then the marker that puts the
                    # whole update in force. Nothing downstream may act on half
                    # of it (ADR, order-book depth D-2).
                    for side, key in (("B", "bids"), ("S", "asks")):
                        for lvl in row.get(key, []):
                            emit(
                                "L {} {} {} {} {}".format(
                                    symbol, stamp, side, lvl["price"], lvl["qty"]
                                )
                            )
                    emit(f"A {symbol} {stamp}")

                    # The venue's own opinion of whether our book is right. A
                    # depth feed is deltas, so one dropped update leaves a book
                    # that is quietly wrong — plausible prices, incorrect sizes,
                    # and a fill model that walks them without complaint.
                    want = row.get("checksum")
                    precision = precisions.get(pair)
                    if want is None or precision is None:
                        continue
                    got = book_checksum(book, precision[0], precision[1])
                    if got != want:
                        note(
                            f"{symbol} book checksum {got} != {want}; "
                            f"resynchronising"
                        )
                        return "resync"
            elif channel == "trade":
                if message.get("type") == "snapshot":
                    # Kraken opens a trade subscription with its recent history.
                    # Emitting that would replay minutes of the past at full
                    # speed into a live session, and a strategy would trade on
                    # prices that are already gone.
                    note(f"skipped {len(message.get('data', []))} historical trades")
                    continue
                for row in message.get("data", []):
                    emit(
                        "T {} {} {} {} {}".format(
                            symbol_of(row["symbol"]),
                            nanos(row["timestamp"]),
                            row["price"],
                            row["qty"],
                            "B" if row["side"] == "buy" else "S",
                        )
                    )
            elif channel == "instrument":
                for pair in message.get("data", {}).get("pairs", []):
                    precisions[pair["symbol"]] = (
                        int(pair["price_precision"]),
                        int(pair["qty_precision"]),
                    )
            elif channel == "status":
                for row in message.get("data", []):
                    note(f"kraken {row.get('system')} api {row.get('api_version')}")
            elif message.get("method") == "subscribe" and not message.get("success"):
                note(f"subscription refused: {text[:200]}")
    finally:
        ws.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("pair", nargs="+", help="Kraken v2 pairs, e.g. BTC/USD")
    parser.add_argument(
        "--depth",
        type=int,
        default=0,
        help="levels a side to stream; 0 (the default) streams top of book only",
    )
    parser.add_argument(
        "--seconds", type=int, default=None, help="stop after this long"
    )
    parser.add_argument(
        "--retries",
        type=int,
        default=5,
        help="reconnections to attempt before giving up",
    )
    args = parser.parse_args()

    started = time.time()
    attempts = 0
    while True:
        left = None
        if args.seconds is not None:
            left = args.seconds - (time.time() - started)
            if left <= 0:
                return 0
        try:
            note(f"connecting to {HOST}{PATH} for {' '.join(args.pair)}")
            why = stream(args.pair, left, args.depth)
            if why == "resync":
                # Reconnecting is the bluntest resynchronisation and the one
                # with no edge cases: the fresh subscription opens with a
                # snapshot, which downstream already treats as a reset.
                attempts = 0
                continue
            if args.seconds is not None:
                return 0
            raise Closed("the stream ended")
        except (Closed, OSError, ssl.SSLError) as e:
            attempts += 1
            if attempts > args.retries:
                note(f"giving up after {attempts} attempts: {e}")
                return 1
            # A gap is visible downstream anyway: the risk gate ages the book
            # and refuses on a stale quote. Saying so here makes the reason
            # findable rather than inferred from a hole in the recording.
            pause = min(2**attempts, 30)
            note(f"disconnected ({e}); reconnecting in {pause}s")
            time.sleep(pause)


if __name__ == "__main__":
    sys.exit(main())
