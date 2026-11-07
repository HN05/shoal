#!/usr/bin/env python3
"""Measure the CPU time of the test suite, per binary or per test.

Builds the test binaries as `cargo test` does with CI's settings, then runs
them and reads each process's user and system CPU from wait4, which counts the
descendants it waited for, such as the daemons and CLI processes a test
spawns, but not processes left running after it.

`suite` runs each binary whole, one after another, as `cargo test` does, and
prints their totals. `tests` runs every test alone in its own process, --jobs
at a time, and lists the costliest; --csv keeps every row.

  python3 scripts/profile_tests.py suite
  python3 scripts/profile_tests.py tests --binary workspaces --csv target/tests.csv
"""
import argparse
import concurrent.futures
import csv
import json
import os
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
# CI's build settings (.forgejo/workflows/ci.yml).
CI_ENV = {"CARGO_INCREMENTAL": "0", "CARGO_PROFILE_DEV_DEBUG": "0",
          "CARGO_PROFILE_TEST_DEBUG": "0"}
TOP_ROWS = 40


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
        cwd=ROOT, env={**os.environ, **CI_ENV}, check=True,
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


def run(args, cwd, log):
    """Run a test process; return (exit status, wall s, user s, system s)."""
    env = {**cargo_env(), "CARGO_MANIFEST_DIR": cwd}
    start = time.monotonic()
    process = subprocess.Popen(args, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                               stdout=log, stderr=subprocess.STDOUT)
    _, status, usage = os.wait4(process.pid, 0)
    wall = time.monotonic() - start
    process.returncode = os.waitstatus_to_exitcode(status)
    return process.returncode, wall, usage.ru_utime, usage.ru_stime


def suite(found):
    """Run each test binary whole and print its times and the total."""
    print(f"{'binary':<28} {'wall s':>8} {'user s':>8} {'sys s':>8}  status")
    rows = []
    for name, executable, cwd in found:
        rows.append((name, *measure_binary(executable, cwd)))
        print_binary(rows[-1])
    print_total(rows)


def measure_binary(executable, cwd):
    """(exit status, wall s, user s, system s) of a whole test binary; a
    failing one's output ends up on stdout."""
    with tempfile.TemporaryFile() as log:
        status, *times = run([executable], cwd, log)
        if status != 0:
            log.seek(0)
            sys.stdout.write(log.read().decode(errors="replace")[-4000:])
    return (status, *times)


def print_binary(row):
    name, status, wall, user, system = row
    print(f"{name:<28} {wall:>8.1f} {user:>8.1f} {system:>8.1f}  {status}", flush=True)


def print_total(rows):
    wall, user, system = (sum(row[column] for row in rows) for column in (2, 3, 4))
    print(f"{'total':<28} {wall:>8.1f} {user:>8.1f} {system:>8.1f}")


def tests(found, jobs, csv_path):
    """Run every test alone, `jobs` at a time, and report the costliest."""
    work = listed_tests(found)
    print(f"{len(work)} tests, {jobs} at a time", file=sys.stderr)
    rows = measure_tests(work, jobs)
    if csv_path:
        write_csv(rows, csv_path)
    print_binary_totals(rows)
    print_failures(rows)
    print_costliest(rows)


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


def measure_tests(work, jobs):
    """(binary name, test, exit status, wall s, user s, system s) of each
    test in `work`, in its order."""
    rows = []
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        for done, row in enumerate(pool.map(measure_test, work), 1):
            rows.append(row)
            if done % 100 == 0:
                print(f"{done}/{len(work)}", file=sys.stderr, flush=True)
    return rows


def measure_test(item):
    name, executable, cwd, test = item
    with tempfile.TemporaryFile() as log:
        status, wall, user, system = run(
            [executable, "--exact", test, "--test-threads=1", "-q"], cwd, log)
    return name, test, status, wall, user, system


def write_csv(rows, path):
    with open(path, "w", newline="") as file:
        writer = csv.writer(file)
        writer.writerow(["binary", "test", "status", "wall", "user", "system"])
        writer.writerows(rows)


def binary_totals(rows):
    """(binary name, tests, user s, system s) per binary, in first-seen order."""
    totals = {}
    for name, _, _, _, user, system in rows:
        count, user_total, system_total = totals.get(name, (0, 0.0, 0.0))
        totals[name] = (count + 1, user_total + user, system_total + system)
    return [(name, *total) for name, total in totals.items()]


def print_binary_totals(rows):
    print(f"{'binary':<16} {'tests':>6} {'user s':>9} {'sys s':>9}")
    for name, count, user, system in binary_totals(rows):
        print(f"{name:<16} {count:>6} {user:>9.1f} {system:>9.1f}")


def print_failures(rows):
    failed = [f"{name}::{test}" for name, test, status, *_ in rows if status != 0]
    if failed:
        print(f"\nfailed: {', '.join(failed)}")


def print_costliest(rows):
    print(f"\n{'sys s':>7} {'user s':>7} {'wall s':>7}  test")
    for name, test, _, wall, user, system in sorted(rows, key=lambda r: r[5], reverse=True)[:TOP_ROWS]:
        print(f"{system:>7.2f} {user:>7.2f} {wall:>7.2f}  {name}::{test}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=["suite", "tests"])
    parser.add_argument("--binary", action="append",
                        help="only this binary, such as `workspaces` or `shoal (bin)`; repeatable")
    parser.add_argument("--jobs", type=int, default=os.cpu_count(),
                        help="tests run at once in `tests` mode (default: CPU count)")
    parser.add_argument("--csv", help="write every test's row to this file")
    args = parser.parse_args()
    found = [b for b in binaries() if not args.binary or b[0] in args.binary]
    if args.mode == "suite":
        suite(found)
    else:
        tests(found, args.jobs, args.csv)


if __name__ == "__main__":
    main()
