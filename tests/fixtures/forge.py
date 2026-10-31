#!/usr/bin/env python3
"""A scripted forge for tests: `curl` when installed under that name, which
leaves requests to the loopback server to the real curl, or a Forgejo API
server with `serve <port-file>`.

Responses come from $HOME/forge/responses.json, keyed by "METHOD endpoint",
the endpoint relative to the forge's API root, or the whole URL outside one.
A string body is sent as text, anything else as JSON. A list answers
successive requests in turn, repeating its last entry. Each entry is
{"status": 200, "body": ...}, and may add "headers" to send, which may
replace its JSON Content-Type. Both tag each
successful body, and answer a request that names its tag in If-None-Match
with 304. Every request is appended to $HOME/forge/requests.jsonl with its
body, its Authorization value and status; an uploaded file's body is
{"file": name, "type": content type, "bytes": size}, with its "field" in a
form. The server reports a Forgejo version, unlogged, when no response names
"GET version", so Shoal recognizes it as Forgejo.
"""
import hashlib
import json
import os
import re
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = os.path.join(os.environ["HOME"], "forge")
LOG = os.path.join(ROOT, "requests.jsonl")


def load_responses():
    with open(os.path.join(ROOT, "responses.json")) as f:
        return json.load(f)


def respond(key, body, auth=None, tag=None):
    responses = load_responses()
    seen = 0
    if os.path.exists(LOG):
        with open(LOG) as f:
            seen = sum(1 for line in f if json.loads(line)["key"] == key)
    answer = responses.get(key, {"status": 404, "body": {"message": "no fixture for " + key}})
    if isinstance(answer, list):
        answer = answer[min(seen, len(answer) - 1)]
    status, headers = answer.get("status", 200), answer.get("headers", {})
    answer = answer.get("body")
    if status == 200 and tag is not None and tag == etag(answer):
        status, answer = 304, None
    with open(LOG, "a") as f:
        f.write(json.dumps({"key": key, "body": body, "auth": auth, "status": status}) + "\n")
    return status, answer, headers


def etag(answer):
    return '"' + hashlib.sha256(json.dumps(answer).encode()).hexdigest()[:16] + '"'


# API roots whose endpoints the responses name, longest first.
API_ROOTS = [
    re.compile(r"^https://api\.github\.com/"),
    re.compile(r"^https://api\.bitbucket\.org/2\.0/"),
    re.compile(r"^https?://[^/]+/api/v[134]/"),
    re.compile(r"^https?://[^/]+/api/(?=graphql$)"),
]


def endpoint(url):
    for root in API_ROOTS:
        if root.match(url):
            return root.sub("", url, count=1)
    return url


def real_curl(args):
    """Run the next curl on PATH, for the scripted Forgejo server."""
    here = os.path.dirname(os.path.abspath(sys.argv[0]))
    for directory in os.environ["PATH"].split(os.pathsep):
        program = os.path.join(directory, "curl")
        if os.path.abspath(directory) != here and os.access(program, os.X_OK):
            os.execv(program, [program] + args)
    sys.exit("no curl on PATH")


def curl(args):
    """Answer as curl does with Shoal's arguments: the body and the status
    on stdout, the head on stderr, the authorization in config on stdin.
    Requests to the loopback server go to the real curl, so no other
    request leaves the machine."""
    if args[-1].startswith("http://127.0.0.1:"):
        real_curl(args)
    config = sys.stdin.read()
    auth = re.search(r'header = "Authorization: ([^"]*)"', config)
    method, headers, body = "GET", [], None
    i = 0
    while i < len(args):
        arg = args[i]
        if arg == "-X":
            method = args[i + 1]
        elif arg == "-H":
            headers.append(args[i + 1])
        elif arg == "--data-binary":
            data = args[i + 1]
            if data.startswith("@"):
                with open(data[1:], "rb") as f:
                    size = len(f.read())
                kind = next((h for h in headers if h.startswith("Content-Type: ")), "")
                body = {
                    "file": os.path.basename(data[1:]),
                    "type": kind.removeprefix("Content-Type: "),
                    "bytes": size,
                }
            else:
                body = json.loads(data)
        elif arg == "-F":
            field, _, rest = args[i + 1].partition("=@")
            path, _, kind = rest.rpartition(";type=")
            path = path.strip('"').replace('\\"', '"').replace("\\\\", "\\")
            with open(path, "rb") as f:
                size = len(f.read())
            body = {"field": field, "file": os.path.basename(path), "type": kind, "bytes": size}
        if arg in ("-X", "-H", "--data-binary", "-F", "-w", "-D", "-K", "--max-time", "--max-redirs"):
            i += 2
        else:
            i += 1
    url = args[-1]
    tag = next((h.removeprefix("If-None-Match: ") for h in headers if h.startswith("If-None-Match: ")), None)
    status, answer, sent = respond(
        f"{method} {endpoint(url)}", body, auth.group(1) if auth else None, tag=tag
    )
    if status == 200:
        sent = {"ETag": etag(answer), **sent}
    if "Content-Type" not in sent:
        sent = {"Content-Type": "application/json", **sent}
    head = "".join(f"{name}: {value}\r\n" for name, value in sent.items())
    sys.stderr.write(f"HTTP/1.1 {status} Scripted\r\n{head}\r\n")
    if isinstance(answer, str):
        sys.stdout.write(answer)
    elif answer is not None:
        sys.stdout.write(json.dumps(answer))
    sys.stdout.write(f"\n{status}")


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
        if "Content-Type" not in headers:
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
        curl(sys.argv[1:])
