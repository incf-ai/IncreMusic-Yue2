#!/usr/bin/env python3
"""Stub audiocpp_server for launcher tests.

Answers /health on --port. Ignores SIGTERM like a server busy with a GPU job: after a
SIGTERM it keeps its port open for STUB_EXIT_DELAY seconds (default 3), then exits.
"""
import http.server
import os
import signal
import sys
import threading
import time

port = int(sys.argv[sys.argv.index("--port") + 1])
delay = float(os.environ.get("STUB_EXIT_DELAY", "3"))


def on_term(signum, frame):
    print(f"stub: got signal {signum}; finishing the current job first", flush=True)
    threading.Thread(target=lambda: (time.sleep(delay), os._exit(0)), daemon=True).start()


signal.signal(signal.SIGTERM, on_term)


class H(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = b'{"status":"ok","backend":"stub","models":0,"ui":true,"ui_management":true}'
        if self.path == "/v1/models?include_session_options=true":
            body = b'{"object":"list","data":[]}'
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def log_message(self, *a):
        pass


print(f"stub: listening on {port} with args {sys.argv[1:]}", flush=True)
http.server.ThreadingHTTPServer(("127.0.0.1", port), H).serve_forever()
