"""Find and stop the processes a run left behind: those it started, which may
have detached from it, and only those. A process is named by its PID and the
kernel's record of its start together, checked again just before each signal,
so a PID another process took over is never signalled."""
import ctypes
import os
import signal
import subprocess
import sys
import struct
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Optional

TERM_GRACE_SECONDS = 5
KILL_WAIT_SECONDS = 2
# prctl(2) option that makes orphaned descendants this process's children.
PR_SET_CHILD_SUBREAPER = 36
# proc_pidinfo(3) flavor reading `struct proc_bsdinfo`, its size, the offset
# of its status and of its start time, and the status of a zombie.
PROC_PIDTBSDINFO = 3
PROC_BSDINFO_SIZE = 136
PROC_BSDINFO_STATUS = 4
PROC_BSDINFO_START = 120
SZOMB = 5


@dataclass(frozen=True)
class Process:
    pid: int
    ppid: int
    # The kernel's record of when it started, which no later process with
    # the same PID shares.
    birth: tuple
    # When it started, by the clock.
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
    """When a run starting now began, as process start times tell it. Linux
    derives them from a boot time in whole seconds, which can put a process
    up to a second before the clock said, so the run counts from the second
    before."""
    return float(int(time.time()) - 1)


def snapshot():
    """This user's live processes other than this one and the `ps` listing
    them, by PID."""
    listing = subprocess.Popen(["ps", "-axo", "pid=,ppid=,uid="], stdout=subprocess.PIPE,
                               text=True)
    output, _ = listing.communicate()
    user, excluded = os.getuid(), {os.getpid(), listing.pid}
    found = {}
    for line in output.splitlines():
        pid, ppid, uid = (int(field) for field in line.split())
        recorded = birth(pid) if uid == user and pid not in excluded else None
        if recorded is not None:
            found[pid] = (ppid, recorded)
    directories = working_directories(found)
    return {pid: Process(pid, ppid, recorded, start_time(recorded), directories.get(pid))
            for pid, (ppid, recorded) in found.items()}


def birth(pid):
    """The kernel's record of when `pid` started, or None once it has exited,
    including when it only waits to be reaped."""
    if sys.platform.startswith("linux"):
        return linux_birth(pid)
    return macos_birth(pid)


def linux_birth(pid):
    try:
        stat = Path(f"/proc/{pid}/stat").read_text()
        boot = Path("/proc/sys/kernel/random/boot_id").read_text().strip()
    except OSError:
        return None
    # Fields after the parenthesized command name, which may hold spaces:
    # state is the third field of stat(5) and the start time the 22nd.
    fields = stat[stat.rindex(")") + 2:].split()
    if fields[0] in ("Z", "X"):
        return None
    return (boot, int(fields[19]))


def macos_birth(pid):
    libproc = ctypes.CDLL("/usr/lib/libproc.dylib", use_errno=True)
    info = ctypes.create_string_buffer(PROC_BSDINFO_SIZE)
    if libproc.proc_pidinfo(pid, PROC_PIDTBSDINFO, ctypes.c_uint64(0), info,
                            PROC_BSDINFO_SIZE) != PROC_BSDINFO_SIZE:
        return None
    (status,) = struct.unpack_from("I", info.raw, PROC_BSDINFO_STATUS)
    if status == SZOMB:
        return None
    return struct.unpack_from("QQ", info.raw, PROC_BSDINFO_START)


def start_time(recorded):
    """When a process with the birth `recorded` started, by the clock."""
    if sys.platform.startswith("linux"):
        boot_time = next(int(line.split()[1]) for line in Path("/proc/stat").open()
                         if line.startswith("btime "))
        return boot_time + recorded[1] / os.sysconf("SC_CLK_TCK")
    seconds, micros = recorded
    return seconds + micros / 1_000_000


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
    return birth(process.pid) == process.birth


def signal_if_alive(process, number):
    """Send `number` to `process` if its PID still names it, checked just
    before the signal."""
    if alive(process):
        try:
            os.kill(process.pid, number)
        except ProcessLookupError:
            pass


def stop(processes):
    """Stop `processes`, returning those still running afterwards."""
    for number, wait in ((signal.SIGTERM, TERM_GRACE_SECONDS),
                         (signal.SIGKILL, KILL_WAIT_SECONDS)):
        for process in processes:
            signal_if_alive(process, number)
        live = [process for process in processes if alive(process)]
        deadline = time.monotonic() + wait
        while live and time.monotonic() < deadline:
            time.sleep(0.1)
            live = [process for process in live if alive(process)]
        if not live:
            return []
    return [process for process in processes if alive(process)]
