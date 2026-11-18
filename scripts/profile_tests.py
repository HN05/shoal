#!/usr/bin/env python3
"""Measure the CPU time of the test suite, per binary or per test.

Builds the test binaries as `cargo test` does with Cargo's default settings,
as CI's rust job does, so they reuse a workspace's existing build. It then runs
them and reads each process's user and system CPU from wait4, which counts the
descendants it waited for, such as the daemons and CLI processes a test
spawns, but not processes left running after it.

`suite` runs each binary whole, one after another, as `cargo test` does, and
prints their totals. `tests` runs every test alone in its own process, as
`cargo nextest run` does, --jobs at a time, and lists the costliest; --csv
keeps every row.

--git also counts the Git processes the tests start, through a `git` first on
`PATH` that logs each one, and splits them into production code's, whose
commands carry `--no-optional-locks`, and the fixtures'. The logging adds a
shell to every Git process, so CPU times are higher than without it.

  python3 scripts/profile_tests.py suite
  python3 scripts/profile_tests.py tests --binary workspaces --csv target/tests.csv
  python3 scripts/profile_tests.py suite --git --binary "shoal_daemon (lib)"
"""
import argparse
import collections
import concurrent.futures
import csv
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOP_ROWS = 40
GIT_LOG = "PROFILE_TESTS_GIT_LOG"
# The global option production code's Git commands carry and fixtures' do not.
PRODUCTION_GIT = "--no-optional-locks"
# What ends each argument in the Git log, which no argument contains.
ARGUMENT_END = "\x1f"
# Git's global options that take a value as the next argument.
GIT_VALUE_OPTIONS = {"-C", "-c", "--git-dir", "--work-tree", "--namespace", "--config-env"}


def cargo_env():
    """The environment `cargo test` gives the test binaries."""
    return config_env((ROOT / ".cargo" / "config.toml").read_text(), os.environ)


def config_env(config, environ):
    """`environ` with the plain string variables of `config`'s [env] table,
    which a variable already set overrides."""
    env, section = {}, None
    for line in config.splitlines():
        line = line.strip()
        if line.startswith("["):
            section = line
        elif section == "[env]" and "=" in line and not line.startswith("#"):
            name, value = (part.strip() for part in line.split("=", 1))
            env[name] = value.strip('"')
    return {**env, **environ}


def binaries():
    """(name, executable, package directory) of each test binary."""
    output = subprocess.run(
        ["cargo", "test", "--locked", "--no-run", "--message-format=json"],
        cwd=ROOT, check=True,
        stdout=subprocess.PIPE, text=True).stdout
    return built_binaries(output.splitlines())


def built_binaries(lines):
    """The test binaries cargo's JSON `lines` report as built."""
    found = []
    for line in lines:
        message = json.loads(line)
        # A proc-macro's tests need the toolchain's libraries, which only
        # cargo puts on the library path; they have none here.
        if (message.get("reason") == "compiler-artifact" and message.get("executable")
                and message["profile"]["test"] and message["target"]["kind"] != ["proc-macro"]):
            target = message["target"]
            name = target["name"] if target["kind"] == ["test"] else f"{target['name']} ({target['kind'][0]})"
            found.append((name, message["executable"], str(Path(message["manifest_path"]).parent)))
    return found


def run(args, cwd, log, git=None):
    """Run a test process; return (exit status, wall s, user s, system s).
    With `git`, a (wrapper directory, log file) pair, its Git processes are
    logged there."""
    env = {**cargo_env(), "CARGO_MANIFEST_DIR": cwd}
    if git:
        wrapper, git_log = git
        env.update(PATH=f"{wrapper}{os.pathsep}{env.get('PATH', '')}", **{GIT_LOG: str(git_log)})
    start = time.monotonic()
    process = subprocess.Popen(args, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=log, stderr=subprocess.STDOUT)
    _, status, usage = os.wait4(process.pid, 0)
    wall = time.monotonic() - start
    process.returncode = os.waitstatus_to_exitcode(status)
    return process.returncode, wall, usage.ru_utime, usage.ru_stime


def suite(found, wrapper):
    """Run each test binary whole and print its times, its Git processes
    when `wrapper` counts them, and the total."""
    print(f"{'binary':<28} {'wall s':>8} {'user s':>8} {'sys s':>8}"
          + (f" {'git prod':>9} {'git fix':>8}" if wrapper else "") + "  status")
    rows, commands = [], collections.Counter()
    for name, executable, cwd in found:
        status, times, git = measure_binary(executable, cwd, wrapper)
        rows.append((name, status, *times, *git[:2]))
        commands += git[2]
        print_binary(rows[-1], wrapper)
    print_total(rows, wrapper)
    if wrapper:
        print_git_commands(commands)


def measure_binary(executable, cwd, wrapper):
    """(exit status, (wall s, user s, system s), Git counts) of a whole test
    binary; a failing one's output ends up on stdout."""
    with tempfile.TemporaryFile() as log, tempfile.TemporaryDirectory() as logs:
        git_log = Path(logs) / "git"
        status, *times = run([executable], cwd, log, wrapper and (wrapper, git_log))
        if status != 0:
            log.seek(0)
            sys.stdout.write(log.read().decode(errors="replace")[-4000:])
        return status, times, git_counts(git_log)


def print_binary(row, git):
    name, status, wall, user, system, production, fixture = row
    counts = f" {production:>9} {fixture:>8}" if git else ""
    print(f"{name:<28} {wall:>8.1f} {user:>8.1f} {system:>8.1f}{counts}  {status}", flush=True)


def print_total(rows, git):
    wall, user, system = (sum(row[column] for row in rows) for column in (2, 3, 4))
    production, fixture = (sum(row[column] for row in rows) for column in (5, 6))
    counts = f" {production:>9} {fixture:>8}" if git else ""
    print(f"{'total':<28} {wall:>8.1f} {user:>8.1f} {system:>8.1f}{counts}")


def print_git_commands(commands):
    """The Git commands production code ran most often, with their counts."""
    print(f"\n{'git prod':>9}  command")
    for command, count in commands.most_common(TOP_ROWS):
        print(f"{count:>9}  {command}")


def git_wrapper(directory, developer):
    """Write a `git` into `directory` that logs its arguments to the file
    `$PROFILE_TESTS_GIT_LOG` names, then runs the real Git, and return
    `directory`. With `developer`, the macOS developer directory, whose Git
    test fixtures run by its path, an `xcode-select` there names a developer
    directory of its own whose Git is the wrapper too."""
    directory = Path(directory)
    directory.mkdir(exist_ok=True)
    real = developer / "usr" / "bin" / "git" if developer else shutil.which("git")
    wrapper = directory / "git"
    # A record is the arguments, each ending in a unit separator, then a NUL,
    # appended in one write so that concurrent Git processes' never mix.
    wrapper.write_text(
        "#!/bin/sh\n"
        f'if [ -n "${GIT_LOG}" ]; then\n'
        '  record=\n'
        f'  for arg; do record="$record$arg{ARGUMENT_END}"; done\n'
        f'  printf \'%s\\0\' "$record" >> "${GIT_LOG}"\n'
        'fi\n'
        f'exec "{real}" "$@"\n')
    wrapper.chmod(0o755)
    if developer:
        tools = directory / "developer" / "usr" / "bin"
        tools.mkdir(parents=True)
        (tools / "git").symlink_to(wrapper)
        select = directory / "xcode-select"
        select.write_text(
            "#!/bin/sh\n"
            f'[ "$1" = -p ] && [ $# -eq 1 ] && echo "{directory / "developer"}" && exit 0\n'
            'exec /usr/bin/xcode-select "$@"\n')
        select.chmod(0o755)
    return directory


def developer_directory():
    """The macOS developer directory, when it has a Git: the test binaries
    run that Git, by its path, rather than `/usr/bin`'s shim."""
    if sys.platform != "darwin":
        return None
    developer = subprocess.run(["xcode-select", "-p"], stdout=subprocess.PIPE,
                               stderr=subprocess.DEVNULL, text=True).stdout.strip()
    has_git = developer and (Path(developer) / "usr" / "bin" / "git").exists()
    return Path(developer) if has_git else None


def git_counts(log):
    """(production processes, fixture processes, a Counter of production
    commands) the Git log at `log` records; zeros when it does not exist."""
    try:
        records = Path(log).read_bytes().decode(errors="replace").split("\0")[:-1]
    except FileNotFoundError:
        return 0, 0, collections.Counter()
    runs = [record.split(ARGUMENT_END)[:-1] for record in records]
    production = [args for args in runs if args[:1] == [PRODUCTION_GIT]]
    commands = collections.Counter(git_command(args) for args in production)
    return len(production), len(runs) - len(production), commands


def git_command(args):
    """The Git command `args` runs, such as `rev-parse`, after Git's own
    options and their values."""
    remaining = iter(args)
    for arg in remaining:
        if arg in GIT_VALUE_OPTIONS:
            next(remaining, None)
        elif not arg.startswith("-"):
            return arg
    return ""


def tests(found, jobs, csv_path, wrapper):
    """Run every test alone, `jobs` at a time, and report the costliest."""
    work = listed_tests(found)
    print(f"{len(work)} tests, {jobs} at a time", file=sys.stderr)
    rows = measure_tests(work, jobs, wrapper)
    if csv_path:
        write_csv(rows, csv_path)
    print_binary_totals(rows, wrapper)
    print_failures(rows)
    print_costliest(rows, wrapper)


def listed_tests(found):
    """(binary name, executable, package directory, test) of every test."""
    work = []
    for name, executable, cwd in found:
        listing = subprocess.run([executable, "--list", "--format", "terse"], cwd=cwd,
                                 check=True, stdout=subprocess.PIPE, text=True).stdout
        work += [(name, executable, cwd, test) for test in names_in_listing(listing)]
    return work


def names_in_listing(listing):
    """The tests in a test binary's terse `--list` output, leaving out benchmarks."""
    return [line.removesuffix(": test") for line in listing.splitlines()
            if line.endswith(": test")]


def measure_tests(work, jobs, wrapper):
    """(binary name, test, exit status, wall s, user s, system s, production
    Git processes, fixture Git processes) of each test in `work`, in its
    order; the counts are zero unless `wrapper` counts them."""
    rows = []
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        measured = pool.map(lambda item: measure_test(item, wrapper), work)
        for done, row in enumerate(measured, 1):
            rows.append(row)
            if done % 100 == 0:
                print(f"{done}/{len(work)}", file=sys.stderr, flush=True)
    return rows


def measure_test(item, wrapper):
    name, executable, cwd, test = item
    with tempfile.TemporaryFile() as log, tempfile.TemporaryDirectory() as logs:
        git_log = Path(logs) / "git"
        status, wall, user, system = run(
            [executable, "--exact", test, "--test-threads=1", "-q"], cwd, log,
            wrapper and (wrapper, git_log))
        production, fixture, _ = git_counts(git_log)
    return name, test, status, wall, user, system, production, fixture


def write_csv(rows, path):
    with open(path, "w", newline="") as file:
        writer = csv.writer(file)
        writer.writerow(["binary", "test", "status", "wall", "user", "system",
                         "git_production", "git_fixture"])
        writer.writerows(rows)


def binary_totals(rows):
    """(binary name, tests, user s, system s, production Git processes,
    fixture Git processes) per binary, in first-seen order."""
    totals = {}
    for name, _, _, _, *measured in rows:
        count, *sums = totals.get(name, (0, 0.0, 0.0, 0, 0))
        totals[name] = (count + 1, *(total + value for total, value in zip(sums, measured)))
    return [(name, *total) for name, total in totals.items()]


def print_binary_totals(rows, git):
    print(f"{'binary':<16} {'tests':>6} {'user s':>9} {'sys s':>9}"
          + (f" {'git prod':>9} {'git fix':>8}" if git else ""))
    for name, count, user, system, production, fixture in binary_totals(rows):
        counts = f" {production:>9} {fixture:>8}" if git else ""
        print(f"{name:<16} {count:>6} {user:>9.1f} {system:>9.1f}{counts}")


def print_failures(rows):
    failed = [f"{name}::{test}" for name, test, status, *_ in rows if status != 0]
    if failed:
        print(f"\nfailed: {', '.join(failed)}")


def print_costliest(rows, git):
    """The tests with the most system time, or with `git` the most
    production Git processes."""
    column = 6 if git else 5
    print(f"\n{'sys s':>7} {'user s':>7} {'wall s':>7}"
          + (f" {'git prod':>9} {'git fix':>8}" if git else "") + "  test")
    for name, test, _, wall, user, system, production, fixture in sorted(
            rows, key=lambda r: r[column], reverse=True)[:TOP_ROWS]:
        counts = f" {production:>9} {fixture:>8}" if git else ""
        print(f"{system:>7.2f} {user:>7.2f} {wall:>7.2f}{counts}  {name}::{test}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=["suite", "tests"])
    parser.add_argument("--binary", action="append",
                        help="only this binary, such as `workspaces` or `shoal (bin)`; repeatable")
    parser.add_argument("--jobs", type=int, default=os.cpu_count(),
                        help="tests run at once in `tests` mode (default: CPU count)")
    parser.add_argument("--csv", help="write every test's row to this file")
    parser.add_argument("--git", action="store_true",
                        help="count Git processes, production code's and the fixtures'")
    args = parser.parse_args()
    found = [b for b in binaries() if not args.binary or b[0] in args.binary]
    with tempfile.TemporaryDirectory() as directory:
        wrapper = args.git and git_wrapper(directory, developer_directory())
        if args.mode == "suite":
            suite(found, wrapper)
        else:
            tests(found, args.jobs, args.csv, wrapper)


if __name__ == "__main__":
    main()
