#!/usr/bin/env python3
"""Fail a terminal handoff after replacing, losing or retaining its session."""
import json
import os
from pathlib import Path
import sys
import time

root = Path(os.environ.get("HANDOFF_TEST_ROOT", os.environ["HOME"]))
record = root / "handoff-session.json"
args = sys.argv[1:]
with (root / "handoff-calls.jsonl").open("a") as calls:
    calls.write(json.dumps(args) + "\n")
if args[0] == "list":
    if record.exists():
        fields = json.loads(record.read_text())
        if (root / "handoff-pause-cleanup").exists() and (root / "handoff-failed").exists():
            (root / "handoff-pause-cleanup").unlink()
            (root / "handoff-cleanup-paused").touch()
            deadline = time.monotonic() + 120
            while not (root / "handoff-cleanup-release").exists():
                if time.monotonic() >= deadline:
                    sys.exit("handoff cleanup hang guard")
                time.sleep(0.01)
        print("\t".join(f"{key}={value}" for key, value in fields.items()))
elif args[0] == "version":
    (root / "handoff-version-called").touch()
    print(f"socket_dir\t{root / 'zmx'}")
elif args[0] == "run":
    fields = {"name": args[1], "pid": "100", "created": "1"}
    if os.environ["HANDOFF_AFTER"] == "unknown":
        fields.pop("created")
    record.write_text(json.dumps(fields))
elif args[0] == os.environ.get("HANDOFF_FAIL"):
    (root / "handoff-failed").touch()
    after = os.environ["HANDOFF_AFTER"]
    if after == "gone":
        record.unlink()
    elif after in ("pid", "created"):
        fields = json.loads(record.read_text())
        fields[after] = "200" if after == "pid" else "2"
        record.write_text(json.dumps(fields))
    print("handoff failed", file=sys.stderr)
    sys.exit(1)
elif args[0] == "kill":
    record.unlink()
elif args[0] == "set":
    if not record.exists():
        sys.exit("session not found")
    fields = json.loads(record.read_text())
    for assignment in args[2:]:
        key, value = assignment.split("=", 1)
        if value:
            fields[key] = value
        else:
            fields.pop(key, None)
    record.write_text(json.dumps(fields))
else:
    sys.exit(1)
