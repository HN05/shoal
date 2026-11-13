//! Fake `zmx` programs that run a session's command in place, for tests
//! that start agents in sessions without a real zmx.

/// Runs a session's command in place of a zmx session, recording each call
/// and serving the sessions listed in `$HOME/zmx-sessions`. A detached
/// session keeps the environment it was created with and runs sent text as
/// its shell would, logging the output to `$HOME/zmx-<name>.log`, which
/// `history` prints, and stays listed, with the labels set on it, once
/// `$HOME/zmx-keep-sessions` exists.
/// `$HOME/zmx-<name>.running` exists while the sent text runs.
/// An attached session ends once `$HOME/zmx-attach-ends-session` exists, and
/// one an attach creates stays listed after its command, as after a detach,
/// once `$HOME/zmx-attach-detaches` exists.
/// Calls from a place watcher go to `$HOME/zmx-watch-calls`, out of the
/// recorded actions, as do version checks. It reports itself as zmx 1.0.1,
/// or the version in `$HOME/zmx-version`.
pub const PASSTHROUGH_ZMX: &str = r#"#!/usr/bin/env python3
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
"#;

/// A [`PASSTHROUGH_ZMX`] whose sessions run their command on a terminal of
/// their own, as a chat needs. It waits for the command rather than for the
/// terminal to close, which `pty.spawn` waits for and macOS may never
/// report, leaving the session behind.
pub fn terminal_zmx() -> String {
    // The command runs inside the worker's `try`, which removes its
    // running marker however it ends.
    let zmx = PASSTHROUGH_ZMX.replace(
        "        sys.exit(subprocess.run(['sh', '-c', args[1]]).returncode)",
        "        import pty, select\n\
         \x20       pid, terminal = pty.fork()\n\
         \x20       if pid == 0:\n\
         \x20           os.execvp('sh', ['sh', '-c', args[1]])\n\
         \x20       while not (status := os.waitpid(pid, os.WNOHANG))[0]:\n\
         \x20           if select.select([terminal], [], [], 0.1)[0]:\n\
         \x20               try:\n\
         \x20                   os.read(terminal, 4096)\n\
         \x20               except OSError:\n\
         \x20                   pass\n\
         \x20       sys.exit(os.waitstatus_to_exitcode(status[1]))",
    );
    assert_ne!(zmx, PASSTHROUGH_ZMX);
    zmx
}
