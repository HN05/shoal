#!/usr/bin/env python3
"""Container runtime fixture: records each `compose` call and reads its running
projects' stats; never runs Docker or Podman."""
import hashlib
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['HOME'])
args = sys.argv[1:]
state_file = root / 'compose-projects.json'
projects = json.loads(state_file.read_text()) if state_file.exists() else {}
# Each running project has one container, its `db`, which reads as `STATS`
# under its ID shortened as Docker's `stats` prints it.
STATS = {'CPUPerc': '12.50%', 'MemUsage': '1.5GiB / 7.6GiB'}
if args[:4] == ['ps', '--quiet', '--no-trunc', '--filter']:
    label, _, project = args[4].rpartition('=')
    assert label == 'label=com.docker.compose.project'
    if projects.get(project, {}).get('state') == 'running':
        print(hashlib.sha256(project.encode()).hexdigest())
    sys.exit(0)
if args[:4] == ['stats', '--no-stream', '--format', 'json']:
    for container in args[4:]:
        print(json.dumps({'ID': container[:12], **STATS}))
    sys.exit(0)
assert args.pop(0) == 'compose'
assert args.pop(0) == '--project-name'
project = args.pop(0)
command = args[2] if args[:1] == ['-f'] else args[0]
ports = {name: value for name, value in os.environ.items() if name.startswith('SHOAL_PORT_')}
with (root / 'compose-events').open('a') as log:
    event = {'runtime': Path(sys.argv[0]).name, 'project': project, 'command': command,
             'args': args, 'cwd': os.getcwd(), 'ports': ports}
    log.write(json.dumps(event) + '\n')
failure = root / 'compose-fail'
failing = failure.read_text().split() if failure.exists() else []
# `<command> with-file` fails only the calls that read the Compose file.
if failing[:1] == [command] and (failing[1:] != ['with-file'] or args[:1] == ['-f']):
    print(f'injected {command} failure', file=sys.stderr)
    sys.exit(1)
if command == 'up':
    projects[project] = {'state': 'running', 'ports': ports}
elif command == 'stop' and project in projects:
    projects[project]['state'] = 'exited'
elif command == 'down':
    projects.pop(project, None)
elif command == 'ps' and project in projects:
    listed = projects[project]
    publishers = [{'URL': '0.0.0.0', 'TargetPort': 5432, 'PublishedPort': int(port),
                   'Protocol': 'tcp'} for port in listed['ports'].values()]
    print(json.dumps({'Service': 'db', 'State': listed['state'], 'Publishers': publishers}))
state_file.write_text(json.dumps(projects))
