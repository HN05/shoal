---
name: shoal-worker
description: Use inside a Shoal workspace to finish assignments with `shoal done`, notify the user, watch and wait on PRs, sync the default branch from its remote, reserve ports, lease Xcode simulators, and acquire resource permits during development or testing. Applies to agents launched through Shoal or working directly in a Shoal worktree.
---

# Shoal worker

Use `--json`, omit targets for the current context, and request only needed resources.
When acquisition returns `approval_pending`, give the user the request ID and wait
for an unscoped `shoal access approve <id>` or `deny <id>`. Supply `--reason` for
protected access; inspect your requests with `shoal access`. Retry after approval,
and stop on `approval_denied`. Never remove scope to approve your own request.

## Completion

Shoal keeps your workspace until you run `shoal done`; issue closure and merged
PRs do not end the assignment unless `[done] automatic` is enabled (see below). Run `shoal done` as your last command once the
assignment is finished: every watched PR merged, or no PR needed. Never end a
session without it.

Register each PR with `shoal link pr <number-or-url>`. Watches accumulate per
workspace; after `done`, cleanup waits until all have merged and the merged set
contains current HEAD. `shoal unlink pr <number-or-url>` unlinks one PR;
omit its number or URL to unlink all PRs. Closed, unmerged PRs keep waiting, so cancel their
watches before finishing without a merge.

While watched PRs are open, run `shoal --json watch pr` from the workspace. Handle
the returned `updates` by inspecting their PR URLs, then wait again. Comments
and reviews wake the wait, as does each completed CI check or a merge conflict;
respond to available review findings while other checks run. The first wait
includes existing activity; later waits share a persistent cursor per workspace.
`--timeout <seconds>` bounds the wait (default 3600, maximum 3600); `timed_out`
with empty `updates` means no update. A `lookup_failed` entry names a failed
activity lookup; correct its cause before relying on that source. Waiting also
reports PR closure or merging and grants no permission to merge.

`shoal done` defaults to cleanup, which may stop your execution. Use
`shoal done --keep` when the user wants to review in this workspace; `--cleanup`
overrides a `[done] cleanup = false` default. Dirty or newer work is retained;
completion without a PR watch also requires every commit pushed or on the local
default branch. Completion does not claim a merge occurred. Inspect
`completion.error` and `pr_cleanup.error` for cleanup or lookup failures.

To get the user's attention without finishing, such as when a PR is ready to
merge or you need a decision, run `shoal notify "<one-line message>"`. It records
a notification for your workspace and does not affect completion or cleanup.

`[done] automatic = true` also marks the assignment done when the associated
issue closes or every watched PR merges. With it, call `shoal continue` before
closing the issue or merging a watched PR when you receive more work; it defers
issue, PR and idle cleanup until you explicitly call `shoal done`.

## Workspace context

Inside Shoal's tracked execution wrapper, commands inherit scope
and resolve to your own workspace. Agents launched independently in a managed
worktree can use the same commands: Shoal resolves the workspace from the current
directory, but the session itself has no execution scope or lifecycle tracking.

When unsure whether your checkout is managed, use `shoal --json ls` and match
your working directory to a returned workspace `path`. Shoal's resources
require a managed workspace; do not select another agent's workspace or create
one just to obtain a lease.
If workspace setup failed, `shoal --json setup` reruns the configured setup
command and post-setup hook for the current workspace.

## Update from the default branch

```sh
shoal --json sync
git rebase main            # or git merge main; use your repo's branch name
```

`shoal sync` fetches your repository's remote and fast-forwards the local
default branch, advancing the registered checkout when it has that branch
checked out, where `git fetch origin main:main` is refused. It never pushes or changes your branch. Rebase or merge
with plain Git afterwards; other pushed branches are current as
`origin/<branch>`. After rebasing a pushed branch, push it with
`git push --force-with-lease`.

Branches of other workspaces are shared refs, so `git merge feature/api`
brings in their latest commits; use `origin/feature/api` for pushed work after
`shoal sync`. Stay on your workspace's recorded branch.
Agents cannot land: when the repository has no remote, the human runs
`shoal land` to merge your branch into the default branch.

## Ports

```sh
shoal --json port
shoal --json acquire port web
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
shoal --json acquire resource devices --wait 60
shoal resource release devices
```

Use the returned `resource`; `--member <name>` requests a specific member.
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
shoal --json acquire sim --wait 60
shoal sim release
```

Acquisition uses configured preferences and returns an exclusive lease. Use its
`udid` explicitly with app/test tools, never simctl's ambiguous `booted` selector.
Let Shoal manage device creation, boot, shutdown, erase, and deletion. Exit 2
means capacity is busy; don't bypass allocation or retry indefinitely.

Normal reuse preserves apps, data, and settings. Only request a clean device
when the task specifically requires pristine state:

```sh
shoal --json acquire sim --clean --reason "Verify first-launch permission prompts"
```

Give the actual task-specific reason; clean requests and their outcomes are
audited. Release an existing lease before requesting it clean. Shoal chooses
the device to minimize reinstalls. Only installed runtimes are supported.

Stop servers, app automation, and debuggers before releasing their resources.
Release when finished; reservations and leases outlive the requesting command.
