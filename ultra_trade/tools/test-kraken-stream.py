#!/usr/bin/env python3
"""Tests for the parts of the bridge that can be wrong quietly.

    python3 tools/test-kraken-stream.py

No network. Every case here is a pure function over data captured from the
real feed, so this runs in CI beside the Rust gate.

The bridge is the one piece of this system outside the workspace, and it had no
tests while it grew a websocket client, a timestamp parser and a checksum. All
three fail in the same nasty way — plausible output that is subtly wrong — so
they are the three tested here.
"""

import datetime
import importlib.util
import pathlib
import sys

HERE = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("ks", HERE / "kraken-stream.py")
ks = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ks)

failures = []


def check(name, got, want):
    if got != want:
        failures.append(f"{name}\n     got {got!r}\n    want {want!r}")


# ---- timestamps ----------------------------------------------------------
#
# Checked against `datetime`, which is right about the calendar and wrong about
# nanoseconds — it holds microseconds, so the last three digits are the part
# only this parser can get right.

for stamp in [
    "2026-08-17T04:00:50.582749Z",
    "2026-01-01T00:00:00.000000Z",
    "2024-02-29T23:59:59.999999Z",  # leap day
    "1970-01-01T00:00:00.000000Z",
    "2026-03-01T00:00:00.000000Z",  # the day after February, in a leap year
]:
    base, _, frac = stamp.rstrip("Z").partition(".")
    when = datetime.datetime.fromisoformat(base).replace(tzinfo=datetime.timezone.utc)
    want = int(when.timestamp()) * 1_000_000_000 + int((frac + "000000000")[:9])
    check(f"nanos({stamp})", ks.nanos(stamp), want)

check(
    "nanos keeps nanosecond digits datetime would drop",
    ks.nanos("2026-12-31T23:59:59.123456789Z"),
    1798761599123456789,
)

# ---- checksum fields -----------------------------------------------------

check("field pads to the precision", ks.field("63447.0", 1), "634470")
check("field strips leading zeros", ks.field("0.10808727", 8), "10808727")
check("field truncates past the precision", ks.field("1.23456789", 3), "1234")
check("field of a bare integer", ks.field("5", 2), "500")
check("field of zero is not empty", ks.field("0.00000000", 8), "0")

# ---- the checksum itself -------------------------------------------------
#
# A real snapshot of BTC/USD and the checksum Kraken published with it. If the
# algorithm is wrong the bridge reports every book as broken and resynchronises
# for ever, so this vector is the thing standing between that and a release.

SNAPSHOT = {
    "bids": [
        {"price": "63447.0", "qty": "0.10808727"},
        {"price": "63445.4", "qty": "0.01653838"},
        {"price": "63445.2", "qty": "0.00005100"},
        {"price": "63444.4", "qty": "0.05205642"},
        {"price": "63444.3", "qty": "0.78809228"},
        {"price": "63442.5", "qty": "0.78811464"},
        {"price": "63442.2", "qty": "0.83410089"},
        {"price": "63442.1", "qty": "0.00005100"},
        {"price": "63441.8", "qty": "0.45514736"},
        {"price": "63440.6", "qty": "0.06960000"},
    ],
    "asks": [
        {"price": "63447.1", "qty": "0.62198531"},
        {"price": "63447.8", "qty": "0.01581778"},
        {"price": "63448.3", "qty": "0.00005100"},
        {"price": "63451.1", "qty": "0.23911593"},
        {"price": "63451.2", "qty": "0.78800739"},
        {"price": "63451.4", "qty": "0.00005100"},
        {"price": "63451.9", "qty": "0.01300000"},
        {"price": "63453.2", "qty": "0.00129070"},
        {"price": "63453.3", "qty": "0.62045344"},
        {"price": "63453.4", "qty": "0.78798103"},
    ],
}
PUBLISHED = 1274074729

check("checksum of a real snapshot", ks.book_checksum(SNAPSHOT, 1, 8), PUBLISHED)

# One wrong digit anywhere has to change it, or the check catches nothing.
tampered = {
    "bids": [dict(level) for level in SNAPSHOT["bids"]],
    "asks": [dict(level) for level in SNAPSHOT["asks"]],
}
tampered["bids"][4]["qty"] = "0.78809229"
if ks.book_checksum(tampered, 1, 8) == PUBLISHED:
    failures.append("checksum did not notice a one-digit change in a quantity")

tampered = {
    "bids": [dict(level) for level in SNAPSHOT["bids"]],
    "asks": [dict(level) for level in SNAPSHOT["asks"]],
}
del tampered["asks"][3]
if ks.book_checksum(tampered, 1, 8) == PUBLISHED:
    failures.append("checksum did not notice a missing level")

# ---- applying deltas -----------------------------------------------------

book = {"bids": [], "asks": []}
ks.apply_levels(book, "bids", [{"price": "100.0", "qty": "1"}])
ks.apply_levels(book, "bids", [{"price": "102.0", "qty": "2"}])
ks.apply_levels(book, "bids", [{"price": "101.0", "qty": "3"}])
check(
    "bids are kept descending",
    [level["price"] for level in book["bids"]],
    ["102.0", "101.0", "100.0"],
)

ks.apply_levels(book, "asks", [{"price": "110.0", "qty": "1"}])
ks.apply_levels(book, "asks", [{"price": "108.0", "qty": "1"}])
check(
    "asks are kept ascending",
    [level["price"] for level in book["asks"]],
    ["108.0", "110.0"],
)

ks.apply_levels(book, "bids", [{"price": "101.0", "qty": "0"}])
check(
    "a zero quantity removes the level",
    [level["price"] for level in book["bids"]],
    ["102.0", "100.0"],
)

ks.apply_levels(book, "bids", [{"price": "102.0", "qty": "9"}])
check("a repeated price replaces its size", book["bids"][0]["qty"], "9")
check("and does not duplicate the level", len(book["bids"]), 2)

# The checksum is over the top ten, so the book must not grow past it.
deep = {"bids": [], "asks": []}
for n in range(20):
    ks.apply_levels(deep, "bids", [{"price": f"{100 + n}.0", "qty": "1"}])
check("a side is capped at ten levels", len(deep["bids"]), 10)
check("and keeps the best ten", deep["bids"][0]["price"], "119.0")

# ---- symbols -------------------------------------------------------------

check("symbol drops the slash", ks.symbol_of("BTC/USD"), "BTCUSD")
check("symbol of a pair with no slash", ks.symbol_of("BTCUSD"), "BTCUSD")


if failures:
    print(f"{len(failures)} failure(s):\n", file=sys.stderr)
    for failure in failures:
        print(f"  - {failure}", file=sys.stderr)
    sys.exit(1)
print("kraken-stream: all checks pass")
