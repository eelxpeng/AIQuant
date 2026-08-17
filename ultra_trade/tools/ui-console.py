#!/usr/bin/env python3
"""The same view, plus the four buttons. Start this one deliberately.

    mkfifo /tmp/ctl
    cargo run -p paper -- --commands /tmp/ctl session.conf /tmp/md session.log &
    python3 tools/ui-console.py session.log --commands /tmp/ctl

Everything the viewer does, and one thing it cannot: send `halt`, `resume`,
`kill` or `flatten` to a running session.

**A command is pending until it appears in the recording** (ADR D-4). Writing
a line to the pipe says the line was written, not that the engine acted on it,
and the gap between those is where a UI colours a button green and lies. So
this reports `pending`, and the page watches the session's command count until
the engine's own record of it shows up.

**Flatten is never retried automatically** (D-5). The other three are
idempotent — halting twice is a no-op — but flatten raises a second set of
reduce-only intents against a position the first is still working through, and
the operator can end up flatter than they meant in a moment they are already
having a bad time.
"""

import argparse
import json
import pathlib
import sys
from http.server import ThreadingHTTPServer

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from ui.service import Handler, Tools  # noqa: E402

COMMANDS = ("halt", "resume", "kill", "flatten")
# The three that can be sent again safely, and the one that cannot.
IDEMPOTENT = {"halt", "resume", "kill"}


class Console(Handler):
    def do_GET(self):
        if self.path in ("/", "/index.html"):
            return self._page()
        if self.path == "/api/session":
            return self._session(with_fills=False)
        if self.path == "/api/session?fills=1":
            return self._session(with_fills=True)
        return self._json(404, {"error": f"no route {self.path}"})

    def do_POST(self):
        if self.path == "/api/sweep":
            length = int(self.headers.get("Content-Length") or 0)
            try:
                return self._sweep(json.loads(self.rfile.read(length) or b"{}"))
            except json.JSONDecodeError:
                return self._json(400, {"error": "body is not JSON"})
        if self.path != "/api/command":
            return self._json(404, {"error": f"no route {self.path}"})
        length = int(self.headers.get("Content-Length") or 0)
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            return self._json(400, {"error": "body is not JSON"})

        command = str(body.get("command", "")).strip().lower()
        if command not in COMMANDS:
            return self._json(
                400, {"error": f"{command!r} is not one of {', '.join(COMMANDS)}"}
            )

        try:
            # Line-buffered append to the pipe the session is reading.
            with open(self.console, "a") as pipe:
                pipe.write(command + "\n")
        except OSError as e:
            return self._json(502, {"error": f"cannot reach the session: {e}"})

        return self._json(
            202,
            {
                "command": command,
                # Never "done". The engine decides that, and it says so in the
                # recording (D-4).
                "status": "pending",
                "retry_safe": command in IDEMPOTENT,
                "note": (
                    "watch the recording; this is done when the command appears in it"
                    if command in IDEMPOTENT
                    else "flatten is not idempotent — check the position before resending"
                ),
            },
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("recording", help="a session root, e.g. session.log")
    parser.add_argument(
        "--commands",
        required=True,
        help="the pipe the session was started with (paper --commands)",
    )
    parser.add_argument("--port", type=int, default=8081)
    parser.add_argument(
        "--allow-sweep",
        action="store_true",
        help="permit sweeps here; off because this console is attached to a "
        "live session and a sweep can starve it",
    )
    parser.add_argument("--binaries", default=None)
    args = parser.parse_args()

    if not pathlib.Path(args.commands).exists():
        # Better to say so now than to accept a command and drop it.
        sys.exit(f"ui-console: {args.commands} does not exist; is the session running?")

    Console.tools = Tools(args.recording, args.binaries)
    Console.console = args.commands
    Console.allow_sweep = args.allow_sweep
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Console)
    print(f"console on http://127.0.0.1:{args.port}  (CAN SEND COMMANDS)")
    print(f"  reading  {args.recording}")
    print(f"  commands {args.commands}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print()


if __name__ == "__main__":
    main()
