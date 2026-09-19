---
name: shoal
description: Use Shoal to merge local or remote branches into your managed worktree, reserve ports, lease Xcode simulators, and acquire resource permits during development or testing. Applies to agents launched through Shoal or working directly in a Shoal worktree.
---

# Shoal

Use `--json`, omit targets for the current context, and request only needed resources.

`shoal skill install` refreshes user-level instructions for Codex and Claude.
Homebrew links follow upgrades; Cargo installs need refreshing. Run installation
outside a scoped execution; an optional `codex` or `claude` selects one.

## Workspace context

Inside `shoal exec`, `shoal claude`, or `shoal codex --cli`, commands inherit scope
and resolve to your own workspace. Agents launched independently in a managed
worktree can use the same commands: Shoal resolves the workspace from the current
directory, but the session itself has no execution scope or lifecycle tracking.

When unsure whether your checkout is managed, use `shoal --json list` and match
your working directory to a returned workspace `path`. In an ordinary checkout,
use ordinary Git for merges. Shoal's resources require a managed workspace;
do not select another agent's workspace or create one just to obtain a lease.
If workspace setup failed, `shoal --json setup` reruns the configured setup
command and post-setup hook for the current workspace.

## Happy sessions from a console agent

A Happy session outside any managed workspace (a console session on the user's
machine) can hand work to a new session that appears in the Happy app:

```sh
shoal --json add my-project fix-login --issue 34 --agent happy-codex
shoal --json happy codex fix-login --prompt "Fix the login bug" -- --yolo
```

Shoal returns once the launch is recorded (`execution_id`, `pid`, `log`); the
session runs detached and `shoal stop` or `shoal rm` ends it. Issue and `--prompt`
text reaches the agent as its first message (Codex through Happy's server, which
needs this machine's Happy login). Check `prompt_delivered`; when false, the text
is in `prompt_file` and the user must send it from the app. Sessions launched
through Shoal are scoped and cannot create workspaces or start sessions in other
workspaces; the human or console session does.

## PR completion

After opening a PR, run `shoal pr <url>`. Shoal checks with `gh`/`fj` and, when
merged, stops tracked agents and removes the clean workspace. Without those
tools/login, confirm the merge yourself and call `shoal pr merged` as your last
command. Never acknowledge unmerged work. `[pr_cleanup] enabled = false` disables
this; `shoal pr clear` cancels a watch. Dirty or newer work is retained.

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
A local source such as `main` is fast-forwarded from its upstream before the
merge, so `shoal merge main` alone brings in current upstream work. Pass
`--local` to merge the local branch as it is.
Agents cannot land either: when the repository has no remote, the human runs
`shoal land` to merge your branch into the default branch.

## Ports

```sh
shoal --json port
shoal --json port acquire web
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
shoal --json resource
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
shoal --json sim
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
