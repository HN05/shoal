#!/usr/bin/env python3
"""Fail a terminal handoff after replacing, losing or retaining its session."""
import json
import os
from pathlib import Path
import sys

root = Path(os.environ["HOME"])
record = root / "handoff-session.json"
args = sys.argv[1:]
with (root / "handoff-calls.jsonl").open("a") as calls:
    calls.write(json.dumps(args) + "\n")
if args[0] == "list":
    if record.exists():
        print("\t".join(f"{key}={value}" for key, value in json.loads(record.read_text()).items()))
elif args[0] == "run":
    fields = {"name": args[1], "pid": "100", "created": "1"}
    if os.environ["HANDOFF_AFTER"] == "unknown":
        fields.pop("created")
    record.write_text(json.dumps(fields))
elif args[0] == os.environ.get("HANDOFF_FAIL"):
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
elif args[0] != "set":
    sys.exit(1)
