#!/usr/bin/env python3
"""A scripted forge for tests: `gh` or `glab` when installed under that name,
or a Forgejo API server with `serve <port-file>`.

Responses come from $HOME/forge/responses.json. `gh api` and HTTP requests
are keyed by "METHOD endpoint"; other gh commands by the longest key their
arguments start with. A list answers successive requests in turn, repeating
its last entry. Each entry is {"status": 200, "body": ...}, may add
"headers" to send, and may hold the answer for "delay" seconds. The server tags
each successful body, and answers a request that names its tag in
If-None-Match with 304. Every request is appended to
$HOME/forge/requests.jsonl with its body, credentials and status; an uploaded
file's body is {"file": name, "type": content type, "bytes": size}.
The server reports a Forgejo version, unlogged, when no response names
"GET version", so Shoal recognizes it as Forgejo.
"""
import hashlib
import json
import os
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.join(os.environ["HOME"], "forge")
LOG = os.path.join(ROOT, "requests.jsonl")


def load_responses():
    with open(os.path.join(ROOT, "responses.json")) as f:
        return json.load(f)


def respond(key, body, auth=None, prefix=False, tag=None):
    responses = load_responses()
    if prefix:
        matches = [k for k in responses if key == k or key.startswith(k + " ")]
        key = max(matches, key=len) if matches else key
    seen = 0
    if os.path.exists(LOG):
        with open(LOG) as f:
            seen = sum(1 for line in f if json.loads(line)["key"] == key)
    answer = responses.get(key, {"status": 404, "body": {"message": "no fixture for " + key}})
    if isinstance(answer, list):
        answer = answer[min(seen, len(answer) - 1)]
    time.sleep(answer.get("delay", 0))
    status, headers = answer.get("status", 200), answer.get("headers", {})
    answer = answer.get("body")
    if status == 200 and tag is not None and tag == etag(answer):
        status, answer = 304, None
    with open(LOG, "a") as f:
        f.write(json.dumps({"key": key, "body": body, "auth": auth, "status": status}) + "\n")
    return status, answer, headers


def etag(answer):
    return '"' + hashlib.sha256(json.dumps(answer).encode()).hexdigest()[:16] + '"'


def gh(args):
    if args[:1] == ["api"]:
        index = args.index("-X")
        method, endpoint = args[index + 1], args[index + 2]
        source = args[args.index("--input") + 1] if "--input" in args else None
        header = args[args.index("-H") + 1] if "-H" in args else ""
        if source == "-" and "multipart/form-data" in header:
            body = form_file(sys.stdin.buffer.read())
        elif source in (None, "-"):
            body = json.load(sys.stdin) if source else None
        else:
            with open(source, "rb") as f:
                data = f.read()
            body = {
                "file": os.path.basename(source),
                "type": header.removeprefix("Content-Type: "),
                "bytes": len(data),
            }
        status, answer, _ = respond(f"{method} {endpoint}", body, os.environ.get("AGENT"))
        if "--include" in args:
            print(f"HTTP/2.0 {status} Scripted\r\nContent-Type: application/json\r\n\r")
        if answer is not None:
            print(json.dumps(answer))
        if status >= 400:
            print(f"gh: HTTP {status}", file=sys.stderr)
            sys.exit(1)
        return
    status, answer, _ = respond(" ".join(args), None, os.environ.get("AGENT"), prefix=True)
    if answer is not None:
        print(answer if isinstance(answer, str) else json.dumps(answer))
    sys.exit(0 if status < 400 else 1)


def form_file(data):
    """The one file of a multipart form, as its field, name, type and size."""
    head, _, rest = data.partition(b"\r\n\r\n")
    boundary = head.split(b"\r\n", 1)[0]
    content = rest[: rest.rindex(b"\r\n" + boundary)]
    head = head.decode()
    return {
        "field": re.search(r'name="([^"]*)"', head).group(1),
        "file": re.search(r'filename="([^"]*)"', head).group(1),
        "type": re.search(r"Content-Type: (\S+)", head).group(1),
        "bytes": len(content),
    }


class Handler(BaseHTTPRequestHandler):
    def handle_request(self):
        length = int(self.headers.get("Content-Length") or 0)
        data = self.rfile.read(length)
        if self.headers.get("Content-Type", "").startswith("multipart/form-data"):
            body = form_file(data)
        else:
            body = json.loads(data) if length else None
        path = self.path.removeprefix("/api/v1/")
        key = f"{self.command} {path}"
        if key == "GET version" and key not in load_responses():
            status, answer, headers = 200, {"version": "11.0.0+gitea-1.22.0"}, {}
        else:
            status, answer, headers = respond(
                key,
                body,
                self.headers.get("Authorization"),
                tag=self.headers.get("If-None-Match"),
            )
        data = b"" if answer is None else (answer if isinstance(answer, str) else json.dumps(answer)).encode()
        self.send_response(status)
        if status == 200:
            self.send_header("ETag", etag(answer))
        for name, value in headers.items():
            self.send_header(name, value)
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
