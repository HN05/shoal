#!/usr/bin/env python3
"""Container runtime fixture: records each `compose` call; never runs Docker or Podman."""
import json
import os
from pathlib import Path
import sys

root = Path(os.environ['HOME'])
args = sys.argv[1:]
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
if failure.exists() and failure.read_text().strip() == command:
    print(f'injected {command} failure', file=sys.stderr)
    sys.exit(1)
state_file = root / 'compose-projects.json'
projects = json.loads(state_file.read_text()) if state_file.exists() else {}
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
