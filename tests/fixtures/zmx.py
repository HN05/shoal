#!/bin/sh
''':'
# Version checks and session listings, the calls daemons make most, answer
# in sh without starting Python, which runs everything else: this file is
# both, and Python reads these lines as a string.
root=${ZMX_TEST_ROOT:-$HOME}
case $1/$# in
version/1)
  : >"$root/zmx-version-called"
  version=1.0.1
  if [ -e "$root/zmx-version" ]; then version=$(cat "$root/zmx-version"); fi
  printf 'zmx\t\t%s\nsocket_dir\t%s\n' "$version" "$root/zmx"
  exit
  ;;
list/1)
  if [ ! -e "$root/zmx-pause-owner-check" ]; then
    case $(ps -o args= -p "$PPID") in
    *' place-watch '*) log=zmx-watch-calls ;;
    *) log=zmx-calls ;;
    esac
    echo '["list"]' >>"$root/$log"
    if [ -e "$root/zmx-sessions" ]; then
      cat "$root/zmx-sessions"
      echo
    else
      echo 'no sessions found'
    fi
    exit
  fi
  ;;
esac
exec python3 "$0" "$@"
'''
import fcntl, json, os, subprocess, sys, time
from pathlib import Path
root = Path(os.environ.get('ZMX_TEST_ROOT', os.environ['HOME']))
args = sys.argv[1:]
def rewrite_sessions(lines):
    # Replace the listing whole, as zmx's server answers: a list run
    # meanwhile must never find it truncated, which reads as no sessions.
    sessions = root / 'zmx-sessions'
    staged = sessions.with_name(f'zmx-sessions.{os.getpid()}')
    staged.write_text(''.join(line + '\n' for line in lines))
    os.replace(staged, sessions)
if args[:1] == ['worker']:
    # Keep a readable parent for macOS's hidden shell environment, so other
    # fixtures can attribute it while verifying their setup processes.
    try:
        sys.exit(subprocess.run(['sh', '-c', args[1]]).returncode)
    finally:
        (root / f"zmx-{os.environ['ZMX_SESSION']}.running").unlink(missing_ok=True)
parent = subprocess.run(['ps', '-o', 'args=', '-p', str(os.getppid())],
    capture_output=True, text=True).stdout
if args[:1] == ['version']:
    (root / 'zmx-version-called').touch()
    version = root / 'zmx-version'
    print(f"zmx\t\t{version.read_text().strip() if version.exists() else '1.0.1'}")
    print(f'socket_dir\t{root / "zmx"}')
    sys.exit()
log = 'zmx-watch-calls' if ' place-watch ' in parent else 'zmx-calls'
with (root / log).open('a') as calls:
    calls.write(json.dumps(args) + '\n')
if args[:1] == ['set'] and (root / 'zmx-label-fails').exists():
    sys.exit('ownership update failed')
if args[:1] == ['set'] and (root / 'zmx-keep-sessions').exists():
    sessions = root / 'zmx-sessions'
    lines = []
    found = False
    for line in sessions.read_text().splitlines():
        fields = dict(field.split('=', 1) for field in line.strip().split('\t'))
        if fields.get('name') == args[1]:
            found = True
            for assignment in args[2:]:
                key, value = assignment.split('=', 1)
                if value:
                    fields[key] = value
                else:
                    fields.pop(key, None)
            line = '\t'.join(f'{key}={value}' for key, value in fields.items())
        lines.append(line)
    if not found:
        sys.exit('session not found')
    rewrite_sessions(lines)
elif args[:1] == ['kill'] and (root / 'zmx-close-removes-session').exists():
    rewrite_sessions(line for line in (root / 'zmx-sessions').read_text().splitlines()
        if not any(field == f'name={args[1]}' for field in line.strip().split('\t')))
elif args[:1] in (['set'], ['kill']):
    pass
elif args[:1] == ['run']:
    (root / f'zmx-{args[1]}.env').write_text(json.dumps(dict(os.environ)))
    if (root / 'zmx-keep-sessions').exists():
        with (root / 'zmx-sessions').open('a') as sessions:
            sessions.write(f'  name={args[1]}\tpid=1\n')
elif args[:1] == ['send'] and not (root / f'zmx-{args[1]}.env').exists():
    # An attached session's agent reads what is typed; record that it was.
    (root / 'zmx-sent').touch()
elif args[:1] == ['send']:
    env = json.loads((root / f'zmx-{args[1]}.env').read_text())
    env['ZMX_SESSION'] = args[1]
    with (root / f'zmx-{args[1]}.log').open('a') as log:
        # Native send returns after writing input, before the worker labels
        # its session. Waiting here would deadlock on the handoff's lock.
        (root / f'zmx-{args[1]}.running').touch()
        subprocess.Popen([sys.executable, __file__, 'worker', args[2].rstrip('\r')], env=env,
            start_new_session=True,
            stdin=subprocess.DEVNULL, stdout=log, stderr=log)
elif args[:1] == ['history']:
    output = root / f'zmx-{args[1]}.log'
    print(output.read_text() if output.exists() else '', end='')
elif args[:1] == ['list']:
    sessions = root / 'zmx-sessions'
    listed = sessions.read_text() if sessions.exists() else 'no sessions found'
    if (root / 'zmx-pause-owner-check').exists():
        with (root / 'zmx.shoal-ownership.lock').open() as lock:
            try:
                fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                (root / 'zmx-pause-owner-check').unlink()
                (root / 'zmx-owner-check-paused').touch()
                deadline = time.monotonic() + 120
                while not (root / 'zmx-owner-check-release').exists():
                    if time.monotonic() >= deadline:
                        sys.exit('ownership check hang guard')
                    time.sleep(0.01)
    print(listed)
elif args[:1] == ['attach']:
    args = args[1:]
    if args[:1] == ['--labels']:
        args = args[2:]
    if (root / 'zmx-attach-ends-session').exists():
        rewrite_sessions(line for line in (root / 'zmx-sessions').read_text().splitlines()
            if not any(field == f'name={args[0]}' for field in line.strip().split('\t')))
    if len(args) > 1 and (root / 'zmx-attach-detaches').exists():
        with (root / 'zmx-sessions').open('a') as sessions:
            sessions.write(f'  name={args[0]}\tpid=1\n')
    if len(args) > 1:
        os.environ['ZMX_SESSION'] = args[0]
        if os.environ.get('ZMX_TEST_LEAK_INPUT_MODE'):
            import subprocess
            status = subprocess.run(args[1:]).returncode
            with open('/dev/tty', 'wb', buffering=0) as terminal:
                terminal.write(b'\x1b[?1000h')
            sys.exit(status)
        os.execvp(args[1], args[1:])
else:
    sys.exit(1)
