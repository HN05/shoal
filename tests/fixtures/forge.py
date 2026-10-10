#!/usr/bin/env python3
"""A scripted forge for tests: `gh` when installed under that name, or a
Forgejo API server with `serve <port-file>`.

Responses come from $HOME/forge/responses.json. `gh api` and HTTP requests
are keyed by "METHOD endpoint"; other gh commands by the longest key their
arguments start with. A list answers successive requests in turn, repeating
its last entry. Each entry is {"status": 200, "body": ...}. Every request is
appended to $HOME/forge/requests.jsonl with its body and credentials.
"""
import json
import os
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.join(os.environ["HOME"], "forge")
LOG = os.path.join(ROOT, "requests.jsonl")


def respond(key, body, auth=None, prefix=False):
    with open(os.path.join(ROOT, "responses.json")) as f:
        responses = json.load(f)
    if prefix:
        matches = [k for k in responses if key == k or key.startswith(k + " ")]
        key = max(matches, key=len) if matches else key
    seen = 0
    if os.path.exists(LOG):
        with open(LOG) as f:
            seen = sum(1 for line in f if json.loads(line)["key"] == key)
    with open(LOG, "a") as f:
        f.write(json.dumps({"key": key, "body": body, "auth": auth}) + "\n")
    answer = responses.get(key, {"status": 404, "body": {"message": "no fixture for " + key}})
    if isinstance(answer, list):
        answer = answer[min(seen, len(answer) - 1)]
    return answer.get("status", 200), answer.get("body")


def gh(args):
    if args[:1] == ["api"]:
        index = args.index("-X")
        method, endpoint = args[index + 1], args[index + 2]
        body = json.load(sys.stdin) if "--input" in args else None
        status, answer = respond(f"{method} {endpoint}", body, os.environ.get("AGENT"))
        if answer is not None:
            print(json.dumps(answer))
        if status >= 400:
            print(f"gh: HTTP {status}", file=sys.stderr)
            sys.exit(1)
        return
    status, answer = respond(" ".join(args), None, os.environ.get("AGENT"), prefix=True)
    if answer is not None:
        print(answer if isinstance(answer, str) else json.dumps(answer))
    sys.exit(0 if status < 400 else 1)


class Handler(BaseHTTPRequestHandler):
    def handle_request(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length)) if length else None
        path = self.path.removeprefix("/api/v1/")
        status, answer = respond(
            f"{self.command} {path}", body, self.headers.get("Authorization")
        )
        data = b"" if answer is None else json.dumps(answer).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = do_POST = do_PATCH = do_PUT = do_DELETE = handle_request

    def log_message(self, *args):
        pass


if __name__ == "__main__":
    if sys.argv[1:2] == ["serve"]:
        server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        with open(sys.argv[2] + ".tmp", "w") as f:
            f.write(str(server.server_address[1]))
        os.rename(sys.argv[2] + ".tmp", sys.argv[2])
        server.serve_forever()
    else:
        gh(sys.argv[1:])
