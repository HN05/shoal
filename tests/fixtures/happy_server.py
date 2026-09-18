#!/usr/bin/env python3
"""A stand-in for Happy's server: the routes Shoal uses to seed a session
and deliver a prompt, plus a test hook that marks a session alive the way a
connected happy-cli heartbeat would. Prints its port on the first stdout line
and mirrors every request into the JSON file named by argv[1]."""
import json
import sys
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

STATE = {"sessions": {}, "messages": {}, "requests": []}
LOCK = threading.Lock()
RECORD = sys.argv[1]


def persist():
    with open(RECORD, "w") as handle:
        json.dump(STATE, handle)


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def body(self):
        length = int(self.headers.get("Content-Length") or 0)
        return json.loads(self.rfile.read(length) or b"null")

    def reply(self, status, payload):
        data = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def handle_request(self, method):
        with LOCK:
            body = self.body() if method == "POST" else None
            STATE["requests"].append({
                "method": method,
                "path": self.path,
                "authorization": self.headers.get("Authorization"),
                "client": self.headers.get("X-Happy-Client"),
                "body": body,
            })
            if method == "POST" and self.path.startswith("/test/activate/"):
                session_id = self.path.rsplit("/", 1)[1]
                if session_id in STATE["sessions"]:
                    STATE["sessions"][session_id]["active"] = True
                    self.reply(200, {"ok": True})
                else:
                    self.reply(404, {"error": "unknown session"})
            elif self.headers.get("Authorization") != "Bearer test-token":
                self.reply(401, {"error": "unauthorized"})
            elif method == "POST" and self.path == "/v1/sessions":
                session = {
                    "id": "session-" + uuid.uuid4().hex[:8],
                    "seq": 0,
                    "createdAt": 1,
                    "updatedAt": 1,
                    "active": False,
                    "activeAt": 0,
                    "metadata": body["metadata"],
                    "metadataVersion": 1,
                    "agentState": body.get("agentState"),
                    "agentStateVersion": 1,
                    "dataEncryptionKey": body.get("dataEncryptionKey"),
                    "tag": body["tag"],
                }
                STATE["sessions"][session["id"]] = session
                self.reply(200, {"session": session})
            elif method == "GET" and self.path == "/v2/sessions/active":
                active = [s for s in STATE["sessions"].values() if s["active"]]
                self.reply(200, {"sessions": active})
            elif method == "POST" and self.path.startswith("/v3/sessions/") and self.path.endswith("/messages"):
                session_id = self.path.split("/")[3]
                if session_id not in STATE["sessions"]:
                    self.reply(404, {"error": "Session not found"})
                else:
                    stored = STATE["messages"].setdefault(session_id, [])
                    for index, message in enumerate(body["messages"]):
                        stored.append({"seq": len(stored) + 1, "localId": message["localId"], "content": message["content"]})
                    self.reply(200, {"messages": stored})
            else:
                self.reply(404, {"error": "no route"})
            persist()

    def do_GET(self):
        self.handle_request("GET")

    def do_POST(self):
        self.handle_request("POST")


server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
persist()
print(server.server_address[1], flush=True)
server.serve_forever()
