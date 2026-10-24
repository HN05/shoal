#!/usr/bin/env python3
"""Profile a Shoal command against a daemon in a temporary home.

Builds the `profiling` cargo profile, starts a foreground daemon with
SHOAL_PROFILE=1 in a fresh home and state directory, runs each --setup command
once and the command once untimed, then times the command --runs times and
sums the timing lines the CLI and daemon print. `{repo}` in any argument names
a one-commit Git repository created for the run. --sample records the daemon
and the untimed run with samply, for `samply load`.

  python3 scripts/profile_shoal.py --setup 'repo add {repo} --name project' \\
      --setup 'add project fix-1' -- work
"""
import argparse
import contextlib
import json
import os
import re
import shlex
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import isolated_git

ROOT = Path(__file__).resolve().parent.parent
LINE = re.compile(r"^profile (\w+) ([\d.]+)ms (.*)$")
# Variables that would reach the user's agent configuration; Shoal's own and
# Git's are dropped by prefix.
LEAKING = ("CLAUDE_CONFIG_DIR", "CODEX_HOME")
OWN = ("SHOAL_STATE_DIR", "SHOAL_PROFILE")
IDENTITY = {f"GIT_{role}_{field}": value for role in ("AUTHOR", "COMMITTER")
            for field, value in (("NAME", "Shoal Profile"), ("EMAIL", "profile@example.invalid"))}
DAEMON_START_SECONDS = 60
DAEMON_LINES_SECONDS = 10
TOP_ROWS = 25


def summarize(lines, runs):
    """Rows of (kind, subject, count, per-run ms, max ms), costliest first."""
    totals = {}
    for line in lines:
        match = LINE.match(line)
        if match:
            kind, millis, subject = match.group(1), float(match.group(2)), match.group(3)
            count, total, longest = totals.get((kind, subject), (0, 0.0, 0.0))
            totals[(kind, subject)] = (count + 1, total + millis, max(longest, millis))
    rows = [(kind, subject, count, total / runs, longest)
            for (kind, subject), (count, total, longest) in totals.items()]
    return sorted(rows, key=lambda row: row[3], reverse=True)


def report(walls, rows, sandbox):
    print(f"wall ms over {len(walls)} runs: min {min(walls):.1f}, "
          f"median {statistics.median(walls):.1f}, max {max(walls):.1f}")
    if not rows:
        return
    print(f"\n{'kind':<8} {'count':>6} {'ms/run':>9} {'max ms':>8}  subject")
    for kind, subject, count, per_run, longest in rows[:TOP_ROWS]:
        subject = subject.replace(str(sandbox), "$SANDBOX")
        print(f"{kind:<8} {count:>6} {per_run:>9.1f} {longest:>8.1f}  {subject}")


def build():
    """The executable Cargo reports building, wherever its config puts it."""
    output = subprocess.run(["cargo", "build", "--profile", "profiling", "--bin", "shoal",
                             "--message-format=json-render-diagnostics"],
                            cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True).stdout
    for line in output.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            return Path(message["executable"])
    sys.exit("cargo built no shoal executable")


def environment(sandbox):
    """The caller's environment without its Git, Shoal or agent settings."""
    home = sandbox / "home"
    home.mkdir()
    env = isolated_git.environment(home, **IDENTITY, SHOAL_STATE_DIR=str(sandbox / "s"),
                                   SHOAL_PROFILE="1")
    return {name: value for name, value in env.items()
            if name not in LEAKING and (not name.startswith("SHOAL_") or name in OWN)}


def fixture_repository(sandbox, env):
    repo = sandbox / "repo"
    repo.mkdir()
    (repo / "README.md").write_text("Profiling fixture\n")
    for args in (["init", "-q", "-b", "main"], ["add", "README.md"],
                 ["commit", "-qm", "Add README"]):
        subprocess.run(["git", "-C", str(repo), *args], env=env, check=True)
    return repo


def with_repo(args, repo):
    return [part.replace("{repo}", str(repo)) for part in args]


def sampled(command, output):
    """`command` under samply when `output` names its recording."""
    if output is None:
        return command
    return ["samply", "record", "--save-only", "-o", str(output), "--", *command]


@contextlib.contextmanager
def daemon_running(shoal, env, log_path, recording):
    """A foreground daemon logging to `log_path`, stopped on exit."""
    with open(log_path, "w") as log:
        daemon = subprocess.Popen(sampled([str(shoal), "daemon", "run"], recording),
                                  env=env, stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        try:
            wait_for_daemon(shoal, env, daemon, log_path)
            yield
        finally:
            run(shoal, ["daemon", "stop"], env)
            daemon.wait()


def wait_for_daemon(shoal, env, daemon, log_path):
    deadline = time.monotonic() + DAEMON_START_SECONDS
    while time.monotonic() < deadline and daemon.poll() is None:
        status = run(shoal, ["--json", "daemon", "status"], env)
        if status.returncode == 0 and json.loads(status.stdout).get("running"):
            return
        time.sleep(0.1)
    daemon.kill()
    sys.exit(f"the daemon did not start; see {log_path}")


def run(shoal, args, env, check=False, recording=None):
    result = subprocess.run(sampled([str(shoal), *args], recording), env=env,
                            cwd=env["HOME"], stdin=subprocess.DEVNULL,
                            capture_output=True, text=True)
    if check and result.returncode != 0:
        sys.exit(f"shoal {shlex.join(args)} failed:\n{result.stderr}")
    return result


def measure(shoal, command, env, runs):
    """Wall milliseconds of each run and the timing lines the CLI printed."""
    walls, lines = [], []
    for _ in range(runs):
        began = time.perf_counter()
        result = run(shoal, command, env)
        walls.append((time.perf_counter() - began) * 1000)
        lines += result.stderr.splitlines()
    if result.returncode != 0:
        print(f"shoal {shlex.join(command)} exited {result.returncode}:\n"
              f"{result.stderr}", file=sys.stderr)
    return walls, lines


def daemon_lines(log_path, offset, cli_lines):
    """The daemon's log from `offset` once it has timed a request for each
    call in `cli_lines`, which it does after replying, or after a generous limit."""
    calls = sum(1 for line in cli_lines if line.startswith("profile call "))
    deadline = time.monotonic() + DAEMON_LINES_SECONDS
    while True:
        with open(log_path) as log:
            log.seek(offset)
            lines = log.read().splitlines()
        timed = sum(1 for line in lines if line.startswith("profile request "))
        if timed >= calls or time.monotonic() >= deadline:
            return lines
        time.sleep(0.05)


def profile(options, sandbox):
    shoal = build()
    env = environment(sandbox)
    repo = fixture_repository(sandbox, env)
    command = with_repo(options.command, repo)
    log_path = sandbox / "daemon.log"
    recordings = {name: sandbox / f"{name}.json.gz" if options.sample else None
                  for name in ("daemon", "cli")}
    with daemon_running(shoal, env, log_path, recordings["daemon"]):
        for setup in options.setup:
            run(shoal, with_repo(shlex.split(setup), repo), env, check=True)
        # One untimed run warms caches, and is the one samply records.
        run(shoal, command, env, recording=recordings["cli"])
        offset = log_path.stat().st_size
        walls, cli_lines = measure(shoal, command, env, options.runs)
        lines = cli_lines + daemon_lines(log_path, offset, cli_lines)
    report(walls, summarize(lines, options.runs), sandbox)


def main():
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--runs", type=int, default=5, help="timed runs (default 5)")
    parser.add_argument("--setup", action="append", default=[], metavar="ARGS",
                        help="shoal arguments to run once before timing; repeatable")
    parser.add_argument("--sample", action="store_true",
                        help="record the daemon and one command run with samply")
    parser.add_argument("--keep", action="store_true",
                        help="keep the temporary home, state and daemon log")
    parser.add_argument("command", nargs="+", help="shoal arguments to time")
    options = parser.parse_args()
    if options.runs < 1:
        parser.error("--runs must be at least 1")
    if options.sample and not shutil.which("samply"):
        parser.error("--sample needs samply: cargo install --locked samply")
    sandbox = Path(tempfile.mkdtemp(prefix="shoal-profile-")).resolve()
    try:
        profile(options, sandbox)
    finally:
        if options.keep or options.sample:
            print(f"\nkept {sandbox}")
            if options.sample:
                print(f"samply load {sandbox / 'daemon.json.gz'}\n"
                      f"samply load {sandbox / 'cli.json.gz'}")
        else:
            shutil.rmtree(sandbox, ignore_errors=True)


if __name__ == "__main__":
    main()
