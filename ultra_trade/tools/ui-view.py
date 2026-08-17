#!/usr/bin/env python3
"""Watch a session, read a recording, compare a sweep. Read-only.

    python3 tools/ui-view.py session.log
    python3 tools/ui-view.py session.log --port 8080

Opens a page at http://127.0.0.1:8080 that shows what a session did: the
totals marked to market, the fills, the refusals, the feed lag, and the
equity curve. It follows a running session by re-reading the recording, so
starting or stopping it changes nothing about the run (ADR D-6).

**This program cannot send a command.** There is no code path here that
writes anything. Halting or flattening a session is `tools/ui-console.py`,
which is a separate program you start deliberately — so the thing left open
on a second monitor all day is not the thing that can flatten a book (D-3).
"""

import argparse
import pathlib
import sys
from http.server import ThreadingHTTPServer

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
from ui.service import Handler, Tools  # noqa: E402


class Viewer(Handler):
    def do_GET(self):
        if self.path in ("/", "/index.html"):
            return self._page()
        if self.path == "/api/session":
            return self._session(with_fills=False)
        if self.path == "/api/session?fills=1":
            return self._session(with_fills=True)
        return self._json(404, {"error": f"no route {self.path}"})

    def do_POST(self):
        # Said explicitly rather than by omission: this server has no write
        # side at all, and a client that tries gets told why.
        self._json(
            405,
            {
                "error": "this is the read-only viewer; "
                "run tools/ui-console.py to send a command"
            },
        )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("recording", help="a session root, e.g. session.log")
    parser.add_argument("--port", type=int, default=8080)
    parser.add_argument(
        "--binaries", default=None, help="where the built Rust tools are"
    )
    args = parser.parse_args()

    Viewer.tools = Tools(args.recording, args.binaries)
    Viewer.console = None
    # Loopback only. There is no authentication, so this is a single-machine
    # tool until somebody decides how an operator proves who they are (D-3).
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Viewer)
    print(f"viewer on http://127.0.0.1:{args.port}  (read-only)")
    print(f"  reading {args.recording}")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print()


if __name__ == "__main__":
    main()
