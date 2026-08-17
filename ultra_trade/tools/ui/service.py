"""What both UI servers share: running the Rust tools and serving the page.

The UI never decodes a recording itself — it calls `journal`, which re-derives
everything through `report` and `oms` (ADR, operator UI contract, D-2). The
log format has a version, a CRC, a segment chain and a torn-tail rule, and it
has been bumped three times; a second decoder here would go quietly wrong the
first time it moved again.

Standard library only. A web server is not a reason for this repository to
acquire its first dependency (D-1).
"""

import json
import pathlib
import subprocess
import time
from http.server import BaseHTTPRequestHandler

HERE = pathlib.Path(__file__).resolve().parent
ROOT = HERE.parent.parent  # the cargo workspace


class Tools:
    """Runs the Rust tools and hands back what they said."""

    def __init__(self, recording, binaries=None):
        self.recording = str(recording)
        # Prefer built binaries; fall back to `cargo run` so this works in a
        # fresh checkout without a separate build step.
        self.binaries = binaries or (ROOT / "target" / "debug")

    def _run(self, tool, args, timeout):
        exe = pathlib.Path(self.binaries) / tool
        cmd = (
            [str(exe), *args]
            if exe.exists()
            else ["cargo", "run", "-q", "-p", tool, "--", *args]
        )
        done = subprocess.run(
            cmd, cwd=ROOT, capture_output=True, text=True, timeout=timeout
        )
        if done.returncode != 0:
            raise RuntimeError(
                (done.stderr or done.stdout or "no output").strip().splitlines()[-1]
            )
        return done.stdout

    def session(self, with_fills):
        args = [self.recording, "--json"]
        if with_fills:
            args.append("--fills")
        return json.loads(self._run("journal", args, timeout=60))

    def sweep(self, config, vary, split):
        args = [self.recording, config, "--json"]
        for axis in vary:
            args += ["--vary", axis]
        if split:
            args += ["--split", str(split)]
        # A sweep is many backtests. It gets a long timeout and is still the
        # one call that can take a while, which the page says while it waits.
        return json.loads(self._run("sweep", args, timeout=900))


class Handler(BaseHTTPRequestHandler):
    """Common plumbing. Each server supplies its own routes."""

    tools = None
    console = None  # set only by the console server (D-3)
    allow_sweep = True

    def log_message(self, *_):
        pass  # the access log is noise; failures are reported in the response

    def _send(self, code, body, kind="application/json"):
        payload = body.encode() if isinstance(body, str) else body
        self.send_response(code)
        self.send_header("Content-Type", kind)
        self.send_header("Content-Length", str(len(payload)))
        # No caching: the whole point is that the numbers move.
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(payload)

    def _json(self, code, obj):
        self._send(code, json.dumps(obj))

    def _page(self):
        self._send(200, (HERE / "app.html").read_bytes(), "text/html; charset=utf-8")

    def _sweep(self, body):
        """Runs a grid of backtests and hands back the ranked rows.

        A sweep is N backtests and can saturate the machine. The console is by
        definition attached to a **running session**, so it refuses unless
        started with `--allow-sweep`: starving a live engine to answer a
        research question is not a trade anyone would make deliberately.
        """
        if not self.allow_sweep:
            return self._json(
                409,
                {
                    "error": "this server is attached to a live session and will not "
                    "run a sweep; use the viewer, or start the console with "
                    "--allow-sweep"
                },
            )
        config = str(body.get("config", "")).strip()
        if not config:
            return self._json(400, {"error": "no config given"})
        vary = body.get("vary") or []
        if not isinstance(vary, list) or any(not isinstance(v, str) for v in vary):
            return self._json(400, {"error": "vary must be a list of strings"})
        split = body.get("split")
        try:
            return self._json(200, self.tools.sweep(config, vary, split))
        except subprocess.TimeoutExpired:
            return self._json(504, {"error": "the sweep did not finish in time"})
        except RuntimeError as e:
            return self._json(502, {"error": str(e)})

    def _session(self, with_fills):
        started = time.time()
        try:
            data = self.tools.session(with_fills)
        except subprocess.TimeoutExpired:
            return self._json(504, {"error": "journal did not finish in time"})
        except RuntimeError as e:
            return self._json(502, {"error": str(e)})
        except FileNotFoundError:
            return self._json(500, {"error": "cannot find the journal binary"})
        # How stale this answer is by the time it is read. A live view that
        # silently shows a two-second-old position looks the same as a current
        # one, and the difference matters when somebody is deciding whether to
        # flatten (D-7).
        data["read_took_ms"] = round((time.time() - started) * 1000)
        data["read_at"] = time.time()
        data["can_control"] = self.console is not None
        return self._json(200, data)
