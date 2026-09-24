#!/usr/bin/env python3
"""Isolated simctl fixture; never invokes Xcode or touches real simulator storage."""
import json
import os
import plistlib
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
overrides = root / 'sim-state-overrides.json'
states = json.loads(overrides.read_text()) if overrides.exists() else {}
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
    if command == 'listapps':
        if device['state'] != 'Booted':
            sys.exit(149)
        apps = {'com.apple.SystemApp': {'ApplicationType': 'System'}}
        apps.update({f'app.{i}': {'ApplicationType': 'User'} for i in range(device.get('user_apps', 0))})
        print(plistlib.dumps(apps).decode())
        sys.exit(0)
    elif command == 'bootstatus':
        device['state'] = states.get(command, 'Booted')
    elif command == 'shutdown':
        device['state'] = states.get(command, 'Shutdown')
    elif command == 'erase':
        assert device['state'] == 'Shutdown'
        device['user_apps'] = 0
    elif command == 'delete':
        assert device['state'] == 'Shutdown'
        devices.remove(device)
    else:
        raise AssertionError('unexpected simctl command: ' + command)
state.write_text(json.dumps(devices))

if command == 'create' and (root / 'sim-lost-create-response').exists():
    print('injected lost create response', file=sys.stderr)
    sys.exit(1)
