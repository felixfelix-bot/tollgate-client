#!/usr/bin/env python3
"""A Tollgate v1 gate stand-in that speaks the *exit* advert shape.

The Android client learns a gateway from its default route, GETs `/`, and
expects a NIP-99-style event with tags: metric, step_size, price_per_step.
This server serves exactly that, and records every request with its source
address so a test can prove the request really came from the phone.

Two modes matter for the paid-exit work:

  --paid-exit           the advert names the FIPS exit and an inner IPv4 the
                        client reaches *through* the tunnel, so a purchase is
                        "get me onto the exit". It is served on :2121 because
                        that is what the Android app dials.
  POST /                a Cashu token buys a session; the server answers with
                        bytes granted and remaining quota, and counts requests
                        from that source afterwards.

Nothing here is a security boundary and nothing here is claimed to be one:
it is a test double for the phone's HTTP contract. The real enforcement is
the exit's own admission list, exercised by fips-exit's netns harness.
"""
import argparse
import json
import os
import socket
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


def log(logpath, **kv):
    kv["ts"] = round(time.time(), 3)
    with open(logpath, "a") as f:
        f.write(json.dumps(kv, sort_keys=True) + "\n")


class State:
    def __init__(self, metric, step_size, price, mint, min_steps, exit_addr):
        self.metric = metric
        self.step_size = step_size
        self.price = price
        self.mint = mint
        self.min_steps = min_steps
        self.exit_addr = exit_addr
        self.lock = threading.Lock()
        # source ip -> {"bytes": granted, "until": epoch, "spent": n}
        self.sessions = {}

    def advert(self):
        return {
            "kind": 10021,
            "tags": [
                ["metric", self.metric],
                ["step_size", str(self.step_size)],
                ["price_per_step", "cashu", str(self.price), "sat", self.mint,
                 str(self.min_steps)],
                ["exit", self.exit_addr],
            ],
        }

    def grant(self, source, steps):
        now = time.time()
        granted = self.step_size * steps
        with self.lock:
            s = self.sessions.setdefault(source, {"bytes": 0, "spent": 0})
            s["bytes"] += granted
            s["spent"] += steps
            s["until"] = now + (self.step_size / 1000.0) * steps
            return dict(s)


class Handler(BaseHTTPRequestHandler):
    state: State
    logpath: str

    def log_message(self, *a):
        pass  # our own jsonl log is the record

    def _send(self, code, obj):
        body = json.dumps(obj).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        src = self.client_address[0]
        log(self.logpath, method="GET", path=self.path, src=src)
        if self.path in ("/", "/advert", "/.well-known/tollgate"):
            self._send(200, self.state.advert())
        elif self.path == "/usage":
            with self.state.lock:
                s = dict(self.state.sessions.get(src, {"bytes": 0, "spent": 0}))
            self._send(200, {"source": src, **s,
                             "now": round(time.time(), 3)})
        elif self.path == "/healthz":
            self._send(200, {"ok": True})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        src = self.client_address[0]
        n = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(n) if n else b""
        log(self.logpath, method="POST", path=self.path, src=src, bytes=n,
            body=raw[:120].decode("utf-8", "replace"))
        if self.path != "/":
            return self._send(404, {"kind": 21023,
                                    "code": "not-found",
                                    "message": "no such endpoint"})
        # The module's POST / takes the token as the request body, and a
        # stand-in must not invent a JSON envelope: the phone sent
        # `cashuB…` raw. Accept both, because a JSON form is what a newer
        # client might send, but treat the raw body as the token itself.
        token = None
        steps = self.state.min_steps
        text = raw.decode("utf-8", "replace").strip()
        if text.startswith("{"):
            try:
                req = json.loads(text)
            except Exception:
                return self._send(400, {"kind": 21023,
                                        "code": "payment-error-invalid-token",
                                        "message": "body is not json"})
            token = req.get("token") or req.get("cashu")
            steps = int(req.get("steps") or self.state.min_steps)
        else:
            token = text or None
        if not token:
            return self._send(400, {"kind": 21023,
                                    "code": "payment-error-invalid-token",
                                    "message": "no token"})
        s = self.state.grant(src, steps)
        log(self.logpath, event="grant", src=src, steps=steps,
            granted=s["bytes"], token_prefix=str(token)[:12])
        self._send(200, {
            "kind": 1022,
            "allotment": s["bytes"],
            "metric": self.state.metric,
            "device-identifier": src,
            "expires_at": s["until"],
            "exit": self.state.exit_addr,
            "verified_by_mint": False,
        })


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, default=2121)
    p.add_argument("--bind", default="0.0.0.0")
    p.add_argument("--log", default="/tmp/tollgate-gate.jsonl")
    p.add_argument("--metric", default="milliseconds")
    p.add_argument("--step-size", type=int, default=60000)
    p.add_argument("--price", type=int, default=2)
    p.add_argument("--mint", default="https://mint.example")
    p.add_argument("--min-steps", type=int, default=1)
    p.add_argument("--exit-addr", default="fd00::1")
    a = p.parse_args()

    Handler.state = State(a.metric, a.step_size, a.price, a.mint,
                          a.min_steps, a.exit_addr)
    Handler.logpath = a.log
    srv = ThreadingHTTPServer((a.bind, a.port), Handler)
    srv.daemon_threads = True
    print(json.dumps({"listening": f"{a.bind}:{a.port}", "log": a.log,
                      "advert": Handler.state.advert()}), flush=True)
    srv.serve_forever()


if __name__ == "__main__":
    main()
