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
    """The [env] table of .cargo/config.toml, which `cargo test` gives the
    test binaries; a variable already set wins."""
    env, section = {}, None
    for line in (ROOT / ".cargo" / "config.toml").read_text().splitlines():
        line = line.strip()
        if line.startswith("["):
            section = line
        elif section == "[env]" and "=" in line and not line.startswith("#"):
            name, value = (part.strip() for part in line.split("=", 1))
            env[name] = value.strip('"')
    return {**env, **os.environ}


def binaries():
    """(name, executable, package directory) of each test binary."""
    output = subprocess.run(
        ["cargo", "test", "--locked", "--no-run", "--message-format=json"],
        cwd=ROOT, env={**os.environ, **CI_ENV}, check=True,
        stdout=subprocess.PIPE, text=True).stdout
    found = []
    for line in output.splitlines():
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
    print(f"{'binary':<28} {'wall s':>8} {'user s':>8} {'sys s':>8}  status")
    totals = [0.0, 0.0, 0.0]
    for name, executable, cwd in found:
        with tempfile.TemporaryFile() as log:
            status, *times = run([executable], cwd, log)
            if status != 0:
                log.seek(0)
                sys.stdout.write(log.read().decode(errors="replace")[-4000:])
        totals = [total + value for total, value in zip(totals, times)]
        print(f"{name:<28} {times[0]:>8.1f} {times[1]:>8.1f} {times[2]:>8.1f}  {status}", flush=True)
    print(f"{'total':<28} {totals[0]:>8.1f} {totals[1]:>8.1f} {totals[2]:>8.1f}")


def tests(found, jobs, csv_path):
    work = []
    for name, executable, cwd in found:
        listing = subprocess.run([executable, "--list", "--format", "terse"], cwd=cwd,
                                 check=True, stdout=subprocess.PIPE, text=True).stdout
        work += [(name, executable, cwd, line.removesuffix(": test"))
                 for line in listing.splitlines() if line.endswith(": test")]
    print(f"{len(work)} tests, {jobs} at a time", file=sys.stderr)

    def one(item):
        name, executable, cwd, test = item
        with tempfile.TemporaryFile() as log:
            status, wall, user, system = run(
                [executable, "--exact", test, "--test-threads=1", "-q"], cwd, log)
        return name, test, status, wall, user, system

    rows = []
    with concurrent.futures.ThreadPoolExecutor(jobs) as pool:
        for done, row in enumerate(pool.map(one, work), 1):
            rows.append(row)
            if done % 100 == 0:
                print(f"{done}/{len(work)}", file=sys.stderr, flush=True)
    if csv_path:
        with open(csv_path, "w", newline="") as file:
            writer = csv.writer(file)
            writer.writerow(["binary", "test", "status", "wall", "user", "system"])
            writer.writerows(rows)

    print(f"{'binary':<16} {'tests':>6} {'user s':>9} {'sys s':>9}")
    for name in dict.fromkeys(row[0] for row in rows):
        own = [row for row in rows if row[0] == name]
        print(f"{name:<16} {len(own):>6} {sum(r[4] for r in own):>9.1f} {sum(r[5] for r in own):>9.1f}")
    failed = [row for row in rows if row[2] != 0]
    if failed:
        print(f"\nfailed: {', '.join(f'{r[0]}::{r[1]}' for r in failed)}")
    print(f"\n{'sys s':>7} {'user s':>7} {'wall s':>7}  test")
    for name, test, _, wall, user, system in sorted(rows, key=lambda r: r[5], reverse=True)[:TOP_ROWS]:
        print(f"{system:>7.2f} {user:>7.2f} {wall:>7.2f}  {name}::{test}")


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("mode", choices=["suite", "tests"])
    parser.add_argument("--binary", action="append",
                        help="only this binary, such as `workspaces` or `shoal (lib)`; repeatable")
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
