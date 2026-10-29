#!/usr/bin/env python3
"""Record a VHS tape against a development build of Shoal in a sandbox.

  python3 scripts/record.py target/rec/picker.tape
  python3 scripts/record.py target/rec/picker.tape --setup target/rec/setup.sh

The tape lives in its own directory under target/, and the sandbox goes in that
directory's `sandbox`: a home, state directory, zmx directory and Git config of
its own, with `shoal` the development build and a daemon running for it. VHS
runs from the tape's directory with the sandbox's environment, so the tape needs
no setup of its own and its Output paths are relative to that directory.

`--setup` runs a shell script first, from the sandbox's home with the same
environment and SANDBOX naming the sandbox, to create repositories and
workspaces. It must finish before the recording starts; start what the tape
needs from the tape, not in the background.

However the recording ends, the helper then stops the daemon and every process
the recording started, including detached ones still working in the tape's
directory, and reports any it could not stop. Each run starts over, first
stopping what an interrupted run left there. Nothing reaches your own daemon,
state or zmx sessions.
"""
import argparse
import shutil
import signal
import subprocess
import sys
from pathlib import Path

import processes
from sandbox import ROOT, Sandbox, build


def arguments():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("tape", type=Path, help="the VHS tape to record")
    parser.add_argument("--setup", type=Path,
                        help="a shell script that prepares the sandbox before recording")
    return parser.parse_args()


def tape_directory(tape):
    """The tape's own directory under target/, which the run owns."""
    directory = tape.resolve().parent
    target = (ROOT / "target").resolve()
    if directory == target or not directory.is_relative_to(target):
        sys.exit(f"put {tape} in its own directory under {target}")
    return directory


def stop_leftovers(leftovers, description):
    """Stop `leftovers`, naming them, and return those still running."""
    if not leftovers:
        return []
    print(f"Stopping {description}: {pids(leftovers)}", file=sys.stderr)
    survivors = processes.stop(leftovers)
    if survivors:
        print(f"Still running: {pids(survivors)}", file=sys.stderr)
    return survivors


def pids(found):
    return ", ".join(str(process.pid) for process in found)


def require_vhs():
    if shutil.which("vhs") is None:
        sys.exit("install VHS: https://github.com/charmbracelet/vhs")


def unwind_on_termination():
    """Make SIGTERM and SIGHUP exit through cleanup, as Ctrl-C does."""
    for number in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, lambda received, _frame: sys.exit(128 + received))


def prepare_sandbox(directory):
    """A fresh sandbox in the tape's directory, with SANDBOX naming it."""
    sandbox = Sandbox(directory / "sandbox")
    sandbox.prepare(build(), "shoal-rec")
    sandbox.env["SANDBOX"] = str(sandbox.path)
    return sandbox


def run_setup(sandbox, setup):
    subprocess.run(["sh", str(setup)], env=sandbox.env, cwd=sandbox.env["HOME"],
                   stdin=subprocess.DEVNULL, check=True)


def run_vhs(sandbox, tape):
    return subprocess.call(["vhs", str(tape)], env=sandbox.env, cwd=tape.parent)


def record(sandbox, tape, setup):
    sandbox.start_daemon()
    if setup is not None:
        run_setup(sandbox, setup)
    return run_vhs(sandbox, tape)


def ignore_termination():
    """Let cleanup run to the end however many termination signals arrive."""
    for number in (signal.SIGTERM, signal.SIGHUP):
        signal.signal(number, signal.SIG_IGN)


def clean_up(sandbox, directory, since):
    """Stop the daemon and what the run started, returning what survives."""
    ignore_termination()
    sandbox.stop_daemon()
    return stop_leftovers(processes.started_by_run(directory, since),
                          "processes the recording left running")


def main():
    args = arguments()
    tape = args.tape.resolve()
    directory = tape_directory(tape)
    setup = args.setup.resolve() if args.setup else None
    require_vhs()
    unwind_on_termination()
    processes.become_subreaper()
    stop_leftovers(processes.detached_in(directory), "processes an earlier run left")
    sandbox = prepare_sandbox(directory)
    since = processes.run_start()
    try:
        status = record(sandbox, tape, setup)
    finally:
        survivors = clean_up(sandbox, directory, since)
    return status or (1 if survivors else 0)


if __name__ == "__main__":
    sys.exit(main())
