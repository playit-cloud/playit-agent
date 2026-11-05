"""A stand-in for the playit API's claim endpoints, so the setup screen can be captured
without creating real claims. Pending claims stay pending until APPROVE_AFTER seconds
have passed since the code was first seen.

usage: api_stub.py <port> [approve_after_secs]
"""

import http.server
import json
import sys
import time

APPROVE_AFTER = float(sys.argv[2]) if len(sys.argv) > 2 else 1e9
first_seen = {}


class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        length = int(self.headers.get("content-length", 0))
        request = json.loads(self.rfile.read(length) or b"{}")
        code = request.get("code", "")
        age = time.time() - first_seen.setdefault(code, time.time())
        if self.path == "/claim/setup":
            if age < APPROVE_AFTER / 2:
                state = "WaitingForUserVisit"
            elif age < APPROVE_AFTER:
                state = "WaitingForUser"
            else:
                state = "UserAccepted"
            body = {"status": "success", "data": state}
        elif age >= APPROVE_AFTER:
            body = {"status": "success", "data": {"secret_key": "mock-secret"}}
        else:
            body = {"status": "fail", "data": "NotAccepted"}
        data = json.dumps(body).encode()
        self.send_response(200)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *args):
        pass


http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
