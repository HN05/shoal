"""A build of Shoal in a sandbox directory: its own home, state
directory, zmx, tmux and Zellij directories and Git config, without the
caller's Shoal, Git, host or agent settings, so nothing reaches the user's own
daemon or sessions. The home and the directories that hold sockets lie in a
short directory under /tmp named after the sandbox, outside any checkout."""
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import isolated_git

ROOT = Path(__file__).resolve().parent.parent
IDENTITY = {f"GIT_{role}_{field}": value for role in ("AUTHOR", "COMMITTER")
            for field, value in (("NAME", "Shoal Demo"), ("EMAIL", "demo@example.invalid"))}
# Variables that would reach the user's agents, terminal hosts or daemon.
LEAKING = ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "SHOAL_", "HERDR_", "ZMX_", "HAPPY_", "REX_",
           "TMUX", "ZELLIJ")
DAEMON_START_SECONDS = 60
# Not TMPDIR, which can itself be deep or inside a checkout.
OUTSIDE_ROOT = Path("/tmp")


def build(profile="dev"):
    """The executable Cargo reports building with `profile`, wherever its
    config puts it."""
    output = subprocess.run(["cargo", "build", "--profile", profile, "--bin", "shoal",
                             "--message-format=json-render-diagnostics"],
                            cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True).stdout
    for line in output.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            return Path(message["executable"])
    sys.exit("cargo built no shoal executable")


def outside_directory(sandbox):
    """The short directory for what the sandbox cannot hold itself, named after
    it so a later run finds it: its state, zmx, tmux and Zellij directories,
    which hold sockets, and its home. Socket paths hold about 100 bytes and zmx
    fits session names into what its directory leaves, so a sandbox under a
    long path cannot hold them, and Shoal refuses workspaces inside a Git
    checkout, so a sandbox under target/ cannot hold the home they go in."""
    digest = hashlib.sha256(str(sandbox).encode()).hexdigest()[:16]
    return OUTSIDE_ROOT / f"shoal-sandbox-{digest}"


def home(sandbox):
    return outside_directory(sandbox) / "home"


def isolated(sandbox, identity):
    """The caller's environment without its Git, Shoal, host or agent settings,
    with the sandbox's home, state, zmx, tmux and Zellij directories and Git
    commits made as `identity`."""
    env = isolated_git.environment(home(sandbox), **identity)
    env = {name: value for name, value in env.items() if not name.startswith(LEAKING)}
    outside = outside_directory(sandbox)
    env.update(SHOAL_STATE_DIR=str(outside / "state"), ZMX_DIR=str(outside / "zmx"),
               TMUX_TMPDIR=str(outside / "tmux"), ZELLIJ_SOCKET_DIR=str(outside / "zellij"))
    return env


def environment(sandbox, bin_dir):
    """The isolated environment of a sandbox with `bin_dir` first on PATH."""
    env = isolated(sandbox, IDENTITY)
    env["PATH"] = os.pathsep.join([str(bin_dir), env.get("PATH", "")])
    return env


def prepare_directories(sandbox):
    """Create the home and tmux directories `isolated` names, which their
    programs expect to exist."""
    outside = outside_directory(sandbox)
    outside.mkdir(mode=0o700)
    home(sandbox).mkdir()
    (outside / "tmux").mkdir()


def prepare_zmx_lock(sandbox):
    """Create the ownership lock beside the sandbox's zmx directory. Debug builds
    only open an existing lock there, leaving the user's own to the installed Shoal."""
    (outside_directory(sandbox) / "zmx.shoal-ownership.lock").touch()


def remove(sandbox):
    """Remove the sandbox and its outside directory."""
    shutil.rmtree(sandbox, ignore_errors=True)
    shutil.rmtree(outside_directory(sandbox), ignore_errors=True)


class Sandbox:
    def __init__(self, path):
        self.path = path
        self.bin_dir = path / "bin"
        self.home = home(path)
        self.env = environment(path, self.bin_dir)
        self.daemon = None

    def run(self, args, check=True, cwd=None):
        result = subprocess.run(["shoal", *args], env=self.env, cwd=cwd or self.home,
                                stdin=subprocess.DEVNULL, capture_output=True, text=True)
        if check and result.returncode != 0:
            sys.exit(f"shoal {' '.join(args)} failed:\n{result.stderr}")
        return result

    def clear(self):
        """Remove the last run's sandbox, stopping a daemon an interrupted run left.
        The stop runs from the sandbox, since /tmp cleaners can have removed the
        home in the outside directory."""
        if (self.bin_dir / "shoal").exists():
            self.run(["daemon", "stop"], check=False, cwd=self.path)
        remove(self.path)

    def prepare(self, shoal, prompt):
        """Start over with `shoal` as the sandbox's build and `prompt` naming
        its shells."""
        self.clear()
        self.bin_dir.mkdir(parents=True)
        prepare_directories(self.path)
        # An empty home would start zsh's first-run wizard instead of a prompt.
        (self.home / ".zshrc").write_text(f"PROMPT='{prompt} %1~ %# '\n")
        (self.home / ".bashrc").write_text(f"PS1='{prompt} \\W \\$ '\n")
        # Nor Zellij's first-run tips over the recording.
        zellij = self.home / ".config" / "zellij"
        zellij.mkdir(parents=True)
        (zellij / "config.kdl").write_text("show_startup_tips false\nshow_release_notes false\n")
        (self.bin_dir / "shoal").symlink_to(shoal)
        prepare_zmx_lock(self.path)

    def start_daemon(self):
        log_path = self.path / "daemon.log"
        log = open(log_path, "w")
        self.daemon = subprocess.Popen(["shoal", "daemon", "run"], env=self.env,
                                       stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        deadline = time.monotonic() + DAEMON_START_SECONDS
        while time.monotonic() < deadline and self.daemon.poll() is None:
            status = self.run(["--json", "daemon", "status"], check=False)
            if status.returncode == 0 and json.loads(status.stdout).get("running"):
                return
            time.sleep(0.1)
        self.daemon.kill()
        sys.exit(f"the daemon did not start; see {log_path}")

    def stop_daemon(self):
        if self.daemon is None:
            return
        self.run(["daemon", "stop"], check=False)
        self.daemon.wait()
        self.daemon = None
