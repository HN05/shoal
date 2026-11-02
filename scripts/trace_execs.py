#!/usr/bin/env python3
"""Temporary (#1843): count the programs each test binary's tests execute."""
import collections, os, subprocess, sys, tempfile
sys.path.insert(0, os.path.dirname(__file__))
import profile_tests

for name, executable, cwd in profile_tests.binaries():
    if name not in sys.argv[1:]:
        continue
    log = tempfile.mktemp(prefix="exec-", dir="/tmp")
    env = {**profile_tests.cargo_env(), "CARGO_MANIFEST_DIR": cwd}
    status = subprocess.run(["strace", "-f", "-qq", "-e", "trace=execve", "-e", "signal=none",
                             "-s", "200", "-o", log, executable, "-q"], cwd=cwd, env=env,
                            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode
    programs, commands = collections.Counter(), collections.Counter()
    for line in open(log, errors="replace"):
        if "execve(" not in line or "= -1" in line:
            continue
        rest = line.split("execve(", 1)[1]
        program = os.path.basename(rest.split('"', 2)[1])
        argv = rest.split("[", 1)[1].split("]", 1)[0] if "[" in rest else ""
        words = [w.strip(' "') for w in argv.split(", ")][1:4]
        programs[program] += 1
        commands[f"{program} {' '.join(w for w in words if not w.startswith('/'))[:60]}"] += 1
    print(f"== {name}: exit {status}, {sum(programs.values())} execs")
    for program, count in programs.most_common(25):
        print(f"{count:>8}  {program}")
    print("-- by command")
    for command, count in commands.most_common(60):
        print(f"{count:>8}  {command}")
