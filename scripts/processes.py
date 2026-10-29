"""Find and stop the processes a run left behind: those it started, which may
have detached from it, and only those. A process is named by its PID and start
time together, and checked again before each signal, so a PID another process
took over is never signalled."""
import ctypes
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

TERM_GRACE_SECONDS = 5
KILL_WAIT_SECONDS = 2
# prctl(2) option that makes orphaned descendants this process's children.
PR_SET_CHILD_SUBREAPER = 36


@dataclass(frozen=True)
class Process:
    pid: int
    ppid: int
    started: float
    cwd: Optional[Path]


def become_subreaper():
    """On Linux, keep orphaned descendants as this process's children, so
    they stay its descendants after their parent exits. macOS has no such
    option; detached processes there are found by their directory."""
    if sys.platform.startswith("linux"):
        libc = ctypes.CDLL(None, use_errno=True)
        if libc.prctl(PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0:
            raise OSError(ctypes.get_errno(), "prctl(PR_SET_CHILD_SUBREAPER)")


def run_start():
    """When a run starting now began, as `ps` start times tell it. Linux
    derives them from a boot time in whole seconds, which can put a process
    up to a second before the clock said, so the run counts from the second
    before."""
    return float(int(time.time()) - 1)


def snapshot():
    """This user's live processes other than this one and the `ps` listing
    them, by PID."""
    listing = subprocess.Popen(["ps", "-axo", "pid=,ppid=,uid=,stat=,lstart="],
                               env={**os.environ, "LC_ALL": "C"}, stdout=subprocess.PIPE,
                               text=True)
    output, _ = listing.communicate()
    user, excluded = os.getuid(), {os.getpid(), listing.pid}
    found = {}
    for line in output.splitlines():
        pid, ppid, uid, *status = line.split()
        started = start_time(status)
        if int(uid) == user and int(pid) not in excluded and started is not None:
            found[int(pid)] = (int(ppid), started)
    directories = working_directories(found)
    return {pid: Process(pid, ppid, started, directories.get(pid))
            for pid, (ppid, started) in found.items()}


def ps(*args):
    return subprocess.run(["ps", *args], env={**os.environ, "LC_ALL": "C"},
                          stdout=subprocess.PIPE, text=True).stdout


def start_time(status):
    """When a process with `ps` columns `stat` and `lstart` started, or None
    once it has exited and only waits to be reaped."""
    state, *start = status
    if state.startswith("Z"):
        return None
    return time.mktime(time.strptime(" ".join(start), "%a %b %d %H:%M:%S %Y"))


def working_directories(pids):
    if sys.platform.startswith("linux"):
        directories = {}
        for pid in pids:
            try:
                directories[pid] = Path(os.readlink(f"/proc/{pid}/cwd"))
            except OSError:
                pass
        return directories
    output = subprocess.run(["lsof", "-n", "-P", "-a", "-u", str(os.getuid()), "-d", "cwd",
                             "-Fpn"], stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            text=True).stdout
    directories, pid = {}, None
    for line in output.splitlines():
        if line.startswith("p"):
            pid = int(line[1:])
        elif line.startswith("n") and pid in pids:
            directories[pid] = Path(line[1:])
    return directories


def descendants(processes, ancestor):
    """The processes whose parents lead to `ancestor`."""
    def descends(process):
        seen = set()
        while process.ppid not in seen:
            if process.ppid == ancestor:
                return True
            seen.add(process.ppid)
            process = processes.get(process.ppid)
            if process is None:
                return False
        return False
    return [process for process in processes.values() if descends(process)]


def inside(process, directory):
    return process.cwd is not None and process.cwd.is_relative_to(directory)


def with_descendants(processes, roots):
    """`roots` and every process descending from one of them."""
    found = {process.pid: process for process in roots}
    for root in roots:
        found.update((process.pid, process) for process in descendants(processes, root.pid))
    return list(found.values())


def started_by_run(directory, since):
    """What a run that started at `since` from `directory` left running: this
    process's descendants, and the processes started since then that work in
    `directory`, which a detached one does even after its parent exited, with
    their own descendants wherever they work."""
    directory = directory.resolve()
    processes = snapshot()
    detached = [process for process in processes.values()
                if process.started >= since and inside(process, directory)]
    return with_descendants(processes, descendants(processes, os.getpid()) + detached)


def detached_in(directory):
    """Processes init or launchd adopted that work in `directory`, with their
    descendants: what an interrupted run left there, which no live process
    outside it keeps."""
    directory = directory.resolve()
    processes = snapshot()
    return with_descendants(processes, [process for process in processes.values()
                                        if process.ppid == 1 and inside(process, directory)])


def alive(process):
    status = ps("-o", "stat=,lstart=", "-p", str(process.pid)).split()
    return bool(status) and start_time(status) == process.started


def stop(processes):
    """Stop `processes`, returning those still running afterwards."""
    for sig, wait in ((signal.SIGTERM, TERM_GRACE_SECONDS),
                      (signal.SIGKILL, KILL_WAIT_SECONDS)):
        live = [process for process in processes if alive(process)]
        for process in live:
            try:
                os.kill(process.pid, sig)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + wait
        while live and time.monotonic() < deadline:
            time.sleep(0.1)
            live = [process for process in live if alive(process)]
        if not live:
            return []
    return [process for process in processes if alive(process)]
