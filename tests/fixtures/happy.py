#!/usr/bin/env python3
"""Record a Happy launch and wait in one process with a readable environment.

A shell waiting on sleep leaves an orphan when a test stops the shell. macOS
hides that orphan's environment, blocking concurrent tests' setup verification.
"""
import os
import sys
import time
import urllib.request
from pathlib import Path


def record_launch():
    env = os.environ
    reconnect = '|'.join(env.get('HAPPY_RECONNECT_' + key, '') for key in (
        'SESSION_ID', 'ENCRYPTION_KEY', 'ENCRYPTION_VARIANT', 'SEQ',
        'METADATA_VERSION', 'AGENT_STATE_VERSION',
    ))
    args = '\0'.join(sys.argv[1:]) + '\0'
    stdin = 'data' if sys.stdin.readline().endswith('\n') else 'eof'
    stdout = 'tty' if sys.stdout.isatty() else 'notty'
    record = Path(env['HAPPY_RECORD'])
    temporary = record.with_suffix(record.suffix + '.tmp')
    temporary.write_text(
        f'cwd={os.getcwd()}\nargs={args}\n'
        f'scope={env.get("SHOAL_SCOPE_TOKEN", "")}\n'
        f'execution={env.get("SHOAL_EXECUTION_ID", "")}\n'
        f'workspace={env.get("SHOAL_WORKSPACE", "")}\n'
        f'port={env.get("SHOAL_PORT_WEB", "")}\n'
        f'chat={env.get("SHOAL_CHAT", "")}\n'
        f'reconnect={reconnect}\nstdin={stdin}\nstdout={stdout}\n'
    )
    temporary.replace(record)


def activate_session():
    # A real session connects to Happy's server and heartbeats.
    session = os.environ.get('HAPPY_RECONNECT_SESSION_ID')
    server = os.environ.get('HAPPY_SERVER_URL')
    if session and server:
        time.sleep(1)
        request = urllib.request.Request(f'{server}/test/activate/{session}', method='POST')
        with urllib.request.urlopen(request, timeout=60) as response:
            response.read()


record_launch()
print('hello from happy', flush=True)
print('happy stderr', file=sys.stderr, flush=True)
activate_session()
time.sleep(300)
