#!/usr/bin/env python3
import json
import os
import shlex
import sys
from pathlib import Path

root = Path(os.environ['HOME'])
args = sys.argv[1:]
with (root / 'herdr-calls').open('a') as calls:
    calls.write(json.dumps(args) + '\n')
if args[:2] == ['tab', 'create']:
    print(json.dumps({'result': {'tab': {'tab_id': 'w1:t9'}, 'root_pane': {'pane_id': 'w1:p9'}}}))
elif args[:2] == ['pane', 'run']:
    failure = root / 'herdr-run-fails'
    if failure.exists():
        after = failure.read_text()
        if after == 'replacement':
            (root / 'herdr-terminal').write_text('replacement-terminal')
        if after == 'unreachable':
            (root / 'herdr-unreachable').write_text('')
        sys.exit('handoff failed')
    (root / 'herdr-worker').write_text(json.dumps(shlex.split(args[3])))
    print('{}')
elif args[:2] == ['pane', 'get']:
    if (root / 'herdr-unreachable').exists():
        print(json.dumps({'error': {'code': 'server_not_running'}}), file=sys.stderr)
        sys.exit(1)
    if (root / 'herdr-gone').exists():
        print(json.dumps({'error': {'code': 'pane_not_found'}}), file=sys.stderr)
        sys.exit(1)
    identity = root / 'herdr-terminal'
    terminal = identity.read_text() if identity.exists() else 'terminal-original'
    print(json.dumps({'result': {'pane': {'pane_id': args[2], 'terminal_id': terminal}}}))
elif args[:2] == ['pane', 'close']:
    if (root / 'herdr-close-fails').exists():
        sys.exit('close failed')
    with (root / 'herdr-closed').open('a') as closed:
        closed.write(json.dumps([os.environ.get('HERDR_SOCKET_PATH'), args[2]]) + '\n')
    print('{}')
else:
    print('{}')
