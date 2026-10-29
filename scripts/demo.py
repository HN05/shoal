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
import os
import subprocess
import sys

from sandbox import ROOT, Sandbox, build

SANDBOX = ROOT / "target" / "demo"
REPOSITORY = "acme-app"
WORKSPACES = ("fix-login", "new-onboarding", "refactor-api")


def in_workspace(sandbox, workspace, args, check=True):
    """Run `shoal args` as a process of `workspace`, as its agent would."""
    return sandbox.run(["run", "--workspace", workspace, "--", "shoal", *args], check)


def repository(env):
    repo = SANDBOX / REPOSITORY
    repo.mkdir()
    (repo / "README.md").write_text("Sample repository for the Shoal demo\n")
    # Agents need approval for this port, so the inbox has a request to decide.
    (repo / ".shoal.toml").write_text("[ports.web]\nrequires_approval = true\n")
    for args in (["init", "-q", "-b", "main"], ["add", "."], ["commit", "-qm", "Start"]):
        subprocess.run(["git", "-C", str(repo), *args], env=env, check=True)
    return repo


def fill(sandbox):
    """A repository with workspaces, and something in the inbox of each kind."""
    repo = repository(sandbox.env)
    sandbox.run(["repo", "add", str(repo)])
    for workspace in WORKSPACES:
        sandbox.run(["add", str(repo), workspace])
    in_workspace(sandbox, "fix-login",
                 ["questions", "ask", "Which database should the session cache use?",
                  "--recommend", "SQLite", "--reason", "No server to run",
                  "--choice", "SQLite", "--choice", "Postgres"])
    in_workspace(sandbox, "fix-login", ["alerts", "send", "Login fix pushed; checks are running"])
    # Pending approval exits 2 by design.
    in_workspace(sandbox, "new-onboarding",
                 ["acquire", "port", "web", "--reason", "Preview the onboarding flow"],
                 check=False)
    in_workspace(sandbox, "refactor-api", ["ready"])


def shell(env):
    print("Shoal development build with sample data. Try `shoal`, `shoal inbox`,\n"
          "`shoal work` and `shoal questions`. Exit the shell to stop.", flush=True)
    program = os.environ.get("SHELL") or "/bin/sh"
    return subprocess.call([program, "-i"], env=env, cwd=env["HOME"])


def main():
    sandbox = Sandbox(SANDBOX)
    sandbox.prepare(build(), "shoal-demo")
    sandbox.start_daemon()
    try:
        fill(sandbox)
        return shell(sandbox.env)
    finally:
        sandbox.stop_daemon()


if __name__ == "__main__":
    sys.exit(main())
