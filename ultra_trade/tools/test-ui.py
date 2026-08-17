#!/usr/bin/env python3
"""Tests for the UI servers, with no network and no live session.

    python3 tools/test-ui.py

The interesting cases are all refusals. A UI that quietly accepts a command it
cannot deliver, or that lets the read-only viewer write, is worse than no UI —
so those are what this checks, against a recording made on the spot.
"""

import json
import pathlib
import shutil
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request
from http.server import ThreadingHTTPServer

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
from ui.service import Tools  # noqa: E402

failures = []


def check(name, got, want):
    if got != want:
        failures.append(f"{name}\n     got {got!r}\n    want {want!r}")


def recording(into):
    """A real recording, made by the real tools."""
    conf = into / "s.conf"
    conf.write_text(
        "instrument UI tick 0.01 lot 1 min 1\n"
        "limits UI max-position 50 max-exposure 100000 max-order-notional 50000"
        " max-orders 600 rate-window 60s max-quote-age 30s\n"
        "strategy crossover UI window 5 size 2\n"
    )
    log = into / "session.log"
    subprocess.run(
        ["cargo", "run", "-q", "-p", "record", "--", str(conf), str(log), "200"],
        cwd=ROOT, check=True, capture_output=True, text=True,
    )
    return log


def serve(handler_module, **attrs):
    """Starts a server on an ephemeral port and returns its base URL."""
    handler = handler_module
    for key, value in attrs.items():
        setattr(handler, key, value)
    server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return f"http://127.0.0.1:{server.server_port}", server


def get(url):
    # Error statuses are results here, not exceptions: half these cases are
    # about the server refusing something correctly.
    try:
        with urllib.request.urlopen(url, timeout=120) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())


def post(url, body):
    request = urllib.request.Request(
        url, data=json.dumps(body).encode(),
        headers={"Content-Type": "application/json"}, method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as r:
            return r.status, json.loads(r.read())
    except urllib.error.HTTPError as e:
        return e.code, json.loads(e.read())


work = pathlib.Path(tempfile.mkdtemp(prefix="ultra_trade-ui-"))
try:
    log = recording(work)

    # ---- the read path ---------------------------------------------------
    import importlib.util

    def load(name, path):
        spec = importlib.util.spec_from_file_location(name, path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        return module

    viewer_mod = load("ui_view", HERE / "ui-view.py")
    console_mod = load("ui_console", HERE / "ui-console.py")

    base, server = serve(viewer_mod.Viewer, tools=Tools(log), console=None)
    status, body = get(base + "/api/session")
    check("viewer serves a session", status, 200)
    check("with the instrument from the config", body["instruments"][0]["symbol"], "UI")
    check("and reports it cannot control", body["can_control"], False)
    if "read_took_ms" not in body:
        failures.append("the read path must say how long it took (ADR D-7)")
    if not isinstance(body["totals"]["total"], str):
        failures.append("money must be a string, never a JSON number")
    if "ladder" not in body:
        failures.append("the read path must carry the ladder")
    else:
        for side in ("bids", "asks", "working"):
            if side not in body["ladder"][0]:
                failures.append(f"the ladder needs {side}")
    # Two keys with one name is a key silently lost, and this pair collided.
    if not isinstance(body.get("book"), dict) or "resyncs" not in body["book"]:
        failures.append("the reset counts must survive under their own name")

    # The whole reason the viewer is a separate program.
    status, body = post(base + "/api/command", {"command": "kill"})
    check("the viewer refuses to send a command", status, 405)
    if "console" not in body["error"]:
        failures.append("and should say where to go instead")

    with urllib.request.urlopen(base + "/", timeout=30) as r:
        check("the viewer serves the page", r.status, 200)
        page = r.read().decode()
    # Look for an actual external reference rather than the word "CDN", which
    # appears in the comment explaining why there is not one.
    import re as _re

    external = _re.findall(r"""(?:src|href)\s*=\s*["']([^"']+)""", page)
    remote = [u for u in external if u.startswith(("http:", "https:", "//"))]
    if remote:
        failures.append(f"the page loads something from the internet: {remote}")
    server.shutdown()

    # ---- the write path --------------------------------------------------
    pipe = work / "ctl"
    pipe.write_text("")  # a plain file stands in for the session's pipe

    base, server = serve(console_mod.Console, tools=Tools(log), console=str(pipe))
    status, body = get(base + "/api/session")
    check("the console reports it can control", body["can_control"], True)

    status, body = post(base + "/api/command", {"command": "halt"})
    check("a command is accepted", status, 202)
    check("and is never reported as done", body["status"], "pending")
    check("halt is safe to resend", body["retry_safe"], True)

    status, body = post(base + "/api/command", {"command": "flatten"})
    check("flatten is accepted", status, 202)
    check("but flagged as unsafe to resend", body["retry_safe"], False)
    if "idempotent" not in body["note"]:
        failures.append("and should say why")

    status, body = post(base + "/api/command", {"command": "sell everything"})
    check("an unknown command is refused", status, 400)

    status, body = post(base + "/api/command", {})
    check("a missing command is refused", status, 400)

    check("what reached the session", pipe.read_text(), "halt\nflatten\n")
    server.shutdown()

    # ---- the sweep, and where it is refused ------------------------------
    #
    # A sweep is N backtests and can saturate the machine. The console is
    # attached to a live session by definition, so it says no unless told
    # otherwise (ADR D-8).
    base, server = serve(
        console_mod.Console, tools=Tools(log), console=str(pipe), allow_sweep=False
    )
    status, body = post(base + "/api/sweep", {"config": "x.conf"})
    check("the console refuses a sweep by default", status, 409)
    if "--allow-sweep" not in body["error"]:
        failures.append("and should say how to permit it")
    server.shutdown()

    base, server = serve(viewer_mod.Viewer, tools=Tools(log), console=None)
    conf = work / "s.conf"
    status, body = post(
        base + "/api/sweep",
        {"config": str(conf), "vary": ["UI.window=4,6"], "split": 0.7},
    )
    check("the viewer runs one", status, 200)
    check("and returns a row per variant", len(body.get("runs", [])), 2)
    if body["runs"] and body["runs"][0]["out_total"] is None:
        failures.append("a split must produce an out-of-sample column")

    status, body = post(base + "/api/sweep", {})
    check("a sweep with no config is refused", status, 400)
    status, body = post(base + "/api/sweep", {"config": str(conf), "vary": "nope"})
    check("vary must be a list", status, 400)
    server.shutdown()

    # ---- a session that is not there -------------------------------------
    base, server = serve(viewer_mod.Viewer, tools=Tools(work / "nope.log"), console=None)
    status, body = get(base + "/api/session")
    check("a missing recording is an error, not empty data", status, 502)
    if "error" not in body:
        failures.append("and says what went wrong")
    server.shutdown()

finally:
    shutil.rmtree(work, ignore_errors=True)

if failures:
    print(f"{len(failures)} failure(s):\n", file=sys.stderr)
    for f in failures:
        print(f"  - {f}", file=sys.stderr)
    sys.exit(1)
print("ui: all checks pass")
