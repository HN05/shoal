---
name: shoal
description: Use Shoal to merge local or remote branches into your managed worktree, reserve ports, lease Xcode simulators, and acquire resource permits during development or testing. Applies to agents launched through Shoal or working directly in a Shoal worktree.
---

# Shoal

Use `--json`, omit targets for the current context, and request only needed resources.

For user-level availability outside project repositories, `shoal skill install`
installs or refreshes this skill for Codex and Claude Code. An optional `codex` or
`claude` argument selects one. Run installation outside a scoped execution.

## Workspace context

Inside `shoal exec`, `shoal claude`, or `shoal codex cli`, commands inherit scope
and resolve to your own workspace. Agents launched independently in a managed
worktree can use the same commands: Shoal resolves the workspace from the current
directory, but the session itself has no execution scope or lifecycle tracking.

When unsure whether your checkout is managed, use `shoal --json list` and match
your working directory to a returned workspace `path`. In an ordinary checkout,
use ordinary Git for merges. Shoal's resources require a managed workspace;
do not select another agent's workspace or create one just to obtain a lease.

## Merge branches into your own branch

```sh
shoal --json merge main
shoal --json merge feature/api
shoal --json merge feature/api --remote origin
shoal --json merge origin/feature/api
```

Stay on your workspace's recorded branch. The argument names the source branch;
the destination is your current workspace. A local branch takes precedence.
If absent locally, Shoal finds the branch on configured remotes and fetches it,
including branches never fetched before. Multiple remote matches require
`--remote <name>`. Explicit remote sources always fetch the current remote tip;
use the unqualified branch name with `--remote`.

Merges can fast-forward or create a merge commit. They do not push, update main,
switch branches, stash edits, or reset your work. Conflicts return nonzero with
`success: false` and Git output in JSON; resolve them in your own worktree and
finish with `git add` and `git commit`, or cancel with `git merge --abort`.
Other failures may return an error before a merge starts; inspect the error.
`shoal pull` separately refreshes the repository's default branch from its upstream;
follow it with `shoal merge <default-branch>` when you also want those changes in
your own branch (for example, `main` or `develop`).

## Ports

```sh
shoal --json ports
shoal --json port reserve web
shoal port release web
```

Use configured names/defaults when available. Repeating a reservation returns the
same port. Pass `--reason "purpose"` for an ad hoc reservation.

Exit 2 with `reserved: false` means a conflict suggestion, not a reservation.
If the suggested port suits the task, accept by repeating the request with
`--port <suggested_port>`. Always use the returned `port`; don't assume the
preferred number was allocated. New reservations do not update your current
environment: pass the number to the server explicitly.

## Resource pools

```sh
shoal --json resources
shoal --json resource acquire devices --wait 60
shoal resource release devices
```

Use the returned `resource`; `--resource <name>` requests a specific member.
Standalone resources use the same commands. Each semaphore lease consumes one permit.
The same `--name` returns the same lease; use distinct names for additional
permits and pass that name on release. Busy requests exit 2. Actual use is cooperative.

For `kind: rwlock` resources, use `--mode read` for read-only access or `--mode write`
for changes (new leases default to write). Readers share; writers exclude everyone
on that resource. Check `read_available`/`write_available`, not just free slots.
Release before changing mode; there are no atomic upgrades or writer priority.

## Simulators

```sh
shoal --json sim list
shoal --json sim acquire --wait 60
shoal sim release
```

Acquisition uses configured preferences and returns an exclusive lease. Use its
`udid` explicitly with app/test tools, never simctl's ambiguous `booted` selector.
Let Shoal manage device creation, boot, shutdown, erase, and deletion. Exit 2
means capacity is busy; don't bypass allocation or retry indefinitely.

Normal reuse preserves apps, data, and settings. Only request a clean device
when the task specifically requires pristine state:

```sh
shoal --json sim acquire --clean --reason "Verify first-launch permission prompts"
```

Give the actual task-specific reason; clean requests and their outcomes are
audited. Release an existing lease before requesting it clean. Shoal chooses
the device to minimize reinstalls. Only installed runtimes are supported.

Stop servers, app automation, and debuggers before releasing their resources.
Release when finished; reservations and leases outlive the requesting command.
