#!/usr/bin/env python3
"""Temporary (#1843): compare a ps inventory with reading /proc, among zombies."""
import os, resource, subprocess, sys, time

def cost(label, fn, n=50):
    r0, t0 = resource.getrusage(resource.RUSAGE_SELF), resource.getrusage(resource.RUSAGE_CHILDREN)
    w = time.monotonic()
    for _ in range(n):
        fn()
    w = time.monotonic() - w
    r1, t1 = resource.getrusage(resource.RUSAGE_SELF), resource.getrusage(resource.RUSAGE_CHILDREN)
    user = (r1.ru_utime - r0.ru_utime + t1.ru_utime - t0.ru_utime) / n * 1000
    sys_ = (r1.ru_stime - r0.ru_stime + t1.ru_stime - t0.ru_stime) / n * 1000
    print(f"{label:<40} user {user:6.2f} ms  sys {sys_:6.2f} ms  wall {w / n * 1000:6.2f} ms", flush=True)

def ps():
    subprocess.run(["ps", "-ax", "-o", "pid=,uid=,ruid="], stdout=subprocess.DEVNULL, env={"LC_ALL": "C", "PATH": os.environ["PATH"]})

def proc():
    for name in os.listdir("/proc"):
        if name.isdigit():
            try:
                with open(f"/proc/{name}/stat", "rb") as f:
                    f.read()
                with open(f"/proc/{name}/status", "rb") as f:
                    f.read()
            except OSError:
                pass

def zombies(count):
    # Children of a process that exits become orphans of PID 1, which never reaps here.
    for _ in range(count // 100):
        subprocess.run(["sh", "-c", "for i in $(seq 100); do true & done"])
    time.sleep(1)

for count in (0, 2700):
    if count:
        zombies(count)
    states = subprocess.run(["ps", "-ax", "-o", "stat="], capture_output=True, text=True).stdout.split()
    print(f"-- processes {len(states)}, zombies {sum(s.startswith('Z') for s in states)}")
    cost("ps -ax -o pid=,uid=,ruid=", ps)
    cost("read /proc/*/stat and status", proc)
