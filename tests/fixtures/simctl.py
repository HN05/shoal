#!/usr/bin/env python3
"""Isolated simctl fixture; never invokes Xcode or touches real simulator storage."""
import json
import os
from pathlib import Path
import sys
import uuid

root = Path(os.environ['HOME'])
args = sys.argv[1:]
assert args.pop(0) == 'simctl'
command = args.pop(0)
with (root / 'sim-events').open('a') as log:
    log.write(json.dumps([command] + args) + '\n')
failure = root / 'sim-fail'
if failure.exists() and failure.read_text().strip() == command:
    print('injected simulator failure', file=sys.stderr)
    sys.exit(1)
state = root / 'sim-devices.json'
devices = json.loads(state.read_text()) if state.exists() else []
runtime = 'com.apple.CoreSimulator.SimRuntime.iOS-Test'
if command == 'list':
    print(json.dumps({
        'devicetypes': [{'name': n, 'identifier': 'type.' + n} for n in ['Phone', 'Tablet', 'Watch']],
        'runtimes': [{'name': 'iOS Test', 'identifier': runtime, 'isAvailable': True}],
        'devices': {runtime: devices},
    }))
    sys.exit(0)
if command == 'create':
    udid = str(uuid.uuid4()).upper()
    devices.append({'name': args[0], 'udid': udid, 'state': 'Shutdown', 'isAvailable': True})
    print(udid)
else:
    device = next(d for d in devices if d['udid'] == args[0])
    assert device['name'].startswith('shoal-'), 'Shoal must never mutate external devices'
    if command == 'bootstatus':
        device['state'] = 'Booted'
    elif command == 'shutdown':
        device['state'] = 'Shutdown'
    elif command == 'erase':
        assert device['state'] == 'Shutdown'
    elif command == 'delete':
        assert device['state'] == 'Shutdown'
        devices.remove(device)
    else:
        raise AssertionError('unexpected simctl command: ' + command)
state.write_text(json.dumps(devices))

if command == 'create' and (root / 'sim-lost-create-response').exists():
    print('injected lost create response', file=sys.stderr)
    sys.exit(1)
