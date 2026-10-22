#!/usr/bin/env python3
"""A scripted forge for tests: `gh` when installed under that name, or a
Forgejo API server with `serve <port-file>`.

Responses come from $HOME/forge/responses.json. `gh api` and HTTP requests
are keyed by "METHOD endpoint"; other gh commands by the longest key their
arguments start with. A list answers successive requests in turn, repeating
its last entry. Each entry is {"status": 200, "body": ...}. Every request is
appended to $HOME/forge/requests.jsonl with its body and credentials; an
uploaded file's body is {"file": name, "type": content type, "bytes": size}.
"""
import json
import os
import re
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
        source = args[args.index("--input") + 1] if "--input" in args else None
        if source in (None, "-"):
            body = json.load(sys.stdin) if source else None
        else:
            header = args[args.index("-H") + 1]
            with open(source, "rb") as f:
                data = f.read()
            body = {
                "file": os.path.basename(source),
                "type": header.removeprefix("Content-Type: "),
                "bytes": len(data),
            }
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
        status, answer = respond(
            f"{self.command} {path}", body, self.headers.get("Authorization")
        )
        data = b"" if answer is None else (answer if isinstance(answer, str) else json.dumps(answer)).encode()
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
