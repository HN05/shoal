#!/usr/bin/env python3
"""Try a development build of Shoal against sample data.

Builds Shoal, starts its daemon with a fresh state directory and home under
target/demo, fills it with a repository, workspaces, a question, an approval
request, a ready mark and a notification, then opens a shell where `shoal` is
that build. Nothing reaches your own daemon, state or zmx sessions. Leaving the
shell stops the daemon; the next run starts over.

  python3 scripts/demo.py
  shoal demo start shoal -- python3 scripts/demo.py   # Offer it as a demo
"""
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

import isolated_git

ROOT = Path(__file__).resolve().parent.parent
SANDBOX = ROOT / "target" / "demo"
IDENTITY = {f"GIT_{role}_{field}": value for role in ("AUTHOR", "COMMITTER")
            for field, value in (("NAME", "Shoal Demo"), ("EMAIL", "demo@example.invalid"))}
# Variables that would reach the user's agents, terminal hosts or daemon.
LEAKING = ("CLAUDE_CONFIG_DIR", "CODEX_HOME", "SHOAL_", "HERDR_", "ZMX_", "HAPPY_")
DAEMON_START_SECONDS = 60
REPOSITORY = "acme-app"
WORKSPACES = ("fix-login", "new-onboarding", "refactor-api")


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


def run(args, env, check=True):
    result = subprocess.run(["shoal", *args], env=env, cwd=env["HOME"],
                            stdin=subprocess.DEVNULL, capture_output=True, text=True)
    if check and result.returncode != 0:
        sys.exit(f"shoal {' '.join(args)} failed:\n{result.stderr}")
    return result


def in_workspace(workspace, args, env, check=True):
    """Run `shoal args` as a process of `workspace`, as its agent would."""
    return run(["run", "--workspace", workspace, "--", "shoal", *args], env, check)


def start_daemon(env):
    log = open(SANDBOX / "daemon.log", "w")
    daemon = subprocess.Popen(["shoal", "daemon", "run"], env=env, stdin=subprocess.DEVNULL,
                              stdout=log, stderr=log)
    deadline = time.monotonic() + DAEMON_START_SECONDS
    while time.monotonic() < deadline and daemon.poll() is None:
        status = run(["--json", "daemon", "status"], env, check=False)
        if status.returncode == 0 and json.loads(status.stdout).get("running"):
            return daemon
        time.sleep(0.1)
    daemon.kill()
    sys.exit(f"the daemon did not start; see {SANDBOX / 'daemon.log'}")


def repository(env):
    repo = SANDBOX / REPOSITORY
    repo.mkdir()
    (repo / "README.md").write_text("Sample repository for the Shoal demo\n")
    # Agents need approval for this port, so the inbox has a request to decide.
    (repo / ".shoal.toml").write_text("[ports.web]\nrequires_approval = true\n")
    for args in (["init", "-q", "-b", "main"], ["add", "."], ["commit", "-qm", "Start"]):
        subprocess.run(["git", "-C", str(repo), *args], env=env, check=True)
    return repo


def fill(env):
    """A repository with workspaces, and something in the inbox of each kind."""
    repo = repository(env)
    run(["repo", "add", str(repo)], env)
    for workspace in WORKSPACES:
        run(["add", str(repo), workspace], env)
    in_workspace("fix-login", ["questions", "ask", "Which database should the session cache use?",
                               "--recommend", "SQLite", "--reason", "No server to run",
                               "--choice", "SQLite", "--choice", "Postgres"], env)
    in_workspace("fix-login", ["alerts", "send", "Login fix pushed; checks are running"], env)
    # Pending approval exits 2 by design.
    in_workspace("new-onboarding", ["acquire", "port", "web", "--reason",
                                    "Preview the onboarding flow"], env, check=False)
    in_workspace("refactor-api", ["ready"], env)


def shell(env):
    print("Shoal development build with sample data. Try `shoal`, `shoal inbox`,\n"
          "`shoal work` and `shoal questions`. Exit the shell to stop.", flush=True)
    program = os.environ.get("SHELL") or "/bin/sh"
    return subprocess.call([program, "-i"], env=env, cwd=env["HOME"])


def main():
    shoal = build()
    if SANDBOX.exists():
        # A daemon left by an interrupted run still owns the old state.
        bin_dir = SANDBOX / "bin"
        if (bin_dir / "shoal").exists():
            run(["daemon", "stop"], environment(SANDBOX, bin_dir), check=False)
        shutil.rmtree(SANDBOX)
    bin_dir = SANDBOX / "bin"
    bin_dir.mkdir(parents=True)
    home = SANDBOX / "home"
    home.mkdir()
    # An empty home would start zsh's first-run wizard instead of a prompt.
    (home / ".zshrc").write_text("PROMPT='shoal-demo %1~ %# '\n")
    (home / ".bashrc").write_text("PS1='shoal-demo \\W \\$ '\n")
    (bin_dir / "shoal").symlink_to(shoal)
    env = environment(SANDBOX, bin_dir)
    daemon = start_daemon(env)
    try:
        fill(env)
        return shell(env)
    finally:
        run(["daemon", "stop"], env, check=False)
        daemon.wait()


if __name__ == "__main__":
    sys.exit(main())
