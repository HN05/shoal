#!/usr/bin/env python3
"""A scripted forge for tests: `curl`, which the scripted `curl` of
`fixtures/curl` runs for every request but those to the loopback server, or
a Forgejo API server with `serve <port-file>`, over HTTPS with
`serve <port-file> <pem>`, `pem` holding its key and certificate.

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
"GET version", so Shoal recognizes it as Forgejo. Git's own requests to a
clone URL, such as the daemon's background fetch, get 404 unlogged: they
are not forge API calls, and arrive whenever a sync runs. Pushes are the
exception: `git http-backend` serves them to a repository under
$HOME/forge/git, once the client signs in with Basic auth, and each is
logged as "PUSH owner/name" with its ref updates, each "old new ref", as
the body and its "username:password" as the authorization.
"""
import base64
import hashlib
import json
import os
import re
import subprocess
import sys

ROOT = os.path.join(os.environ["HOME"], "forge")
LOG = os.path.join(ROOT, "requests.jsonl")
GIT_ROOT = os.path.join(ROOT, "git")


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
    log(key, body, auth, status)
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


def curl(args):
    """Answer as curl does with Shoal's arguments: the body and the status
    on stdout, the head on stderr, the authorization in config on stdin."""
    config = sys.stdin.read()
    auth = re.search(r'header = "Authorization: ([^"]*)"', config)
    method, headers, body, write_out = "GET", [], None, ""
    i = 0
    while i < len(args):
        arg = args[i]
        if arg == "-X":
            method = args[i + 1]
        elif arg == "-H":
            headers.append(args[i + 1])
        elif arg == "-w":
            write_out = args[i + 1]
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
    head = f"HTTP/1.1 {status} Scripted\r\n{head}\r\n" if "-D" in args else ""
    sys.stdout.write(head)
    if isinstance(answer, str):
        sys.stdout.write(answer)
    elif answer is not None:
        sys.stdout.write(json.dumps(answer))
    sys.stdout.write(
        write_out.replace("%{size_header}", str(len(head.encode()))).replace("%{http_code}", str(status))
    )


# Git's smart HTTP requests under a repository's clone URL.
GIT_TRANSPORT = re.compile(r"^/[^?]*\.git/(info/refs|git-upload-pack|git-receive-pack)")


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


def log(key, body, auth, status):
    with open(LOG, "a") as f:
        f.write(json.dumps({"key": key, "body": body, "auth": auth, "status": status}) + "\n")


def git_push(method, path, headers, data):
    """Serve the push request `path` to a repository under GIT_ROOT through
    `git http-backend`, as status, headers and body."""
    route, _, query = path.partition("?")
    repository = route[1:].partition(".git/")[0]
    if "service=git-upload-pack" in query or route.endswith("/git-upload-pack"):
        return 404, {}, b""
    if not os.path.isdir(os.path.join(GIT_ROOT, repository + ".git")):
        return 404, {}, b""
    auth = headers.get("Authorization", "")
    if not auth.startswith("Basic "):
        return 401, {"WWW-Authenticate": 'Basic realm="forge"'}, b""
    login = base64.b64decode(auth.removeprefix("Basic ")).decode()
    env = {
        **os.environ,
        "GIT_PROJECT_ROOT": GIT_ROOT,
        "GIT_HTTP_EXPORT_ALL": "1",
        "PATH_INFO": route,
        "QUERY_STRING": query,
        "REQUEST_METHOD": method,
        "CONTENT_TYPE": headers.get("Content-Type", ""),
        "CONTENT_LENGTH": str(len(data)),
        "REMOTE_USER": login.partition(":")[0],
        "REMOTE_ADDR": "127.0.0.1",
        "GIT_PROTOCOL": headers.get("Git-Protocol", ""),
    }
    served = subprocess.run(["git", "http-backend"], input=data, env=env, capture_output=True, check=True)
    head, _, body = served.stdout.partition(b"\r\n\r\n")
    sent = dict(line.split(": ", 1) for line in head.decode().split("\r\n") if line)
    status = int(sent.pop("Status", "200").split()[0])
    if route.endswith("/git-receive-pack"):
        log("PUSH " + repository, ref_updates(data), login, status)
    return status, sent, body


def ref_updates(data):
    """The ref updates, each "old new ref", a receive-pack request names
    before its pack."""
    updates = []
    while len(data) >= 4 and (size := int(data[:4], 16)) > 0:
        updates.append(data[4:size].split(b"\0")[0].decode().strip())
        data = data[size:]
    return updates


def serve(port_file, pem=None):
    """Serve the scripted Forgejo API on a loopback port, written to
    `port_file` once it listens, over HTTPS with the key and certificate in
    `pem` when given."""
    # Only the server needs http.server, whose import takes longer than the
    # rest of a scripted curl's run.
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

    class Handler(BaseHTTPRequestHandler):
        def handle_request(self):
            length = int(self.headers.get("Content-Length") or 0)
            data = self.rfile.read(length)
            if GIT_TRANSPORT.match(self.path):
                self.send(*git_push(self.command, self.path, self.headers, data))
                return
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
            if status == 200:
                headers = {"ETag": etag(answer), **headers}
            if "Content-Type" not in headers:
                headers = {**headers, "Content-Type": "application/json"}
            self.send(status, headers, data)

        def send(self, status, headers, data):
            self.send_response(status)
            for name, value in headers.items():
                self.send_header(name, value)
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)

        do_GET = do_POST = do_PATCH = do_PUT = do_DELETE = handle_request

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    if pem is not None:
        import ssl

        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(pem)
        server.socket = context.wrap_socket(server.socket, server_side=True)
    with open(port_file + ".tmp", "w") as f:
        f.write(str(server.server_address[1]))
    os.rename(port_file + ".tmp", port_file)
    server.serve_forever()


if __name__ == "__main__":
    if sys.argv[1:2] == ["serve"]:
        serve(*sys.argv[2:4])
    else:
        curl(sys.argv[1:])
