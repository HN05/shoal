"""A development build of Shoal in a sandbox directory: its own home, state
directory, zmx directory and Git config, without the caller's Shoal, Git, host
or agent settings, so nothing reaches the user's own daemon or sessions."""
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
LEAKING = ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "SHOAL_", "HERDR_", "ZMX_", "HAPPY_", "REX_")
DAEMON_START_SECONDS = 60


def build():
    """The executable Cargo reports building, wherever its config puts it."""
    output = subprocess.run(["cargo", "build", "--bin", "shoal",
                             "--message-format=json-render-diagnostics"],
                            cwd=ROOT, check=True, stdout=subprocess.PIPE, text=True).stdout
    for line in output.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("executable"):
            return Path(message["executable"])
    sys.exit("cargo built no shoal executable")


def environment(sandbox, bin_dir):
    """The caller's environment without its Git, Shoal, host or agent settings,
    with the sandbox's home, state and zmx directory and `bin_dir` first on PATH."""
    env = isolated_git.environment(sandbox / "home", **IDENTITY)
    env = {name: value for name, value in env.items() if not name.startswith(LEAKING)}
    env.update(SHOAL_STATE_DIR=str(sandbox / "state"), ZMX_DIR=str(sandbox / "zmx"),
               PATH=os.pathsep.join([str(bin_dir), env.get("PATH", "")]))
    return env


def prepare_zmx_lock(sandbox):
    """Create the ownership lock beside the sandbox's zmx directory. Debug builds
    only open an existing lock there, leaving the user's own to the installed Shoal."""
    (sandbox / "zmx.shoal-ownership.lock").touch()


class Sandbox:
    def __init__(self, path):
        self.path = path
        self.bin_dir = path / "bin"
        self.env = environment(path, self.bin_dir)
        self.daemon = None

    def run(self, args, check=True):
        result = subprocess.run(["shoal", *args], env=self.env, cwd=self.env["HOME"],
                                stdin=subprocess.DEVNULL, capture_output=True, text=True)
        if check and result.returncode != 0:
            sys.exit(f"shoal {' '.join(args)} failed:\n{result.stderr}")
        return result

    def clear(self):
        """Remove the last run's sandbox, stopping a daemon an interrupted run left."""
        if not self.path.exists():
            return
        if (self.bin_dir / "shoal").exists():
            self.run(["daemon", "stop"], check=False)
        shutil.rmtree(self.path)

    def prepare(self, shoal, prompt):
        """Start over with `shoal` as the sandbox's build and `prompt` naming
        its shells."""
        self.clear()
        self.bin_dir.mkdir(parents=True)
        home = self.path / "home"
        home.mkdir()
        # An empty home would start zsh's first-run wizard instead of a prompt.
        (home / ".zshrc").write_text(f"PROMPT='{prompt} %1~ %# '\n")
        (home / ".bashrc").write_text(f"PS1='{prompt} \\W \\$ '\n")
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
