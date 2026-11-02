#!/usr/bin/env python3
"""Run a command and reap the orphans it leaves, as an init process would.

A job container's first process is the runner's `tail -f /dev/null`, which
never reaps: every process a test detaches stays a zombie once it exits, and
each process inventory the tests' daemons and fixtures take reads them all.
As the subreaper of the command, this inherits those orphans instead, and
reaps each as it exits until the command does. Exits with its status.

  .forgejo/scripts/reap.py cargo test --locked
"""
import ctypes
import os
import signal
import subprocess
import sys

PR_SET_CHILD_SUBREAPER = 36


def main():
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
        error = ctypes.get_errno()
        raise OSError(error, f"prctl(PR_SET_CHILD_SUBREAPER): {os.strerror(error)}")
    command = subprocess.Popen(sys.argv[1:])
    # A cancelled job stops the command, which stops what it runs.
    for signum in (signal.SIGINT, signal.SIGTERM):
        signal.signal(signum, lambda signum, _: command.send_signal(signum))
    while True:
        pid, status = os.wait()
        if pid == command.pid:
            sys.exit(os.waitstatus_to_exitcode(status))


if __name__ == "__main__":
    main()
