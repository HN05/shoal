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
pub const PASSTHROUGH_ZMX: &str = include_str!("../fixtures/zmx.py");

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
