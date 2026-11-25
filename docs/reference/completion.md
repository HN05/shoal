# Completion and ready marks

This file says what `shoal done` and `shoal ready` do.
[Completion and cleanup](../design/cleanup.md) gives the reasons.

## Assignment completion

`shoal done [space]` records that the space's work is finished at its HEAD. It
notifies the space's [coordinating agent](create.md), or else the user.
Completion does not check or claim that the work merged.

Only `done` completes an assignment. Closing an issue or merging a PR does
not. Where it is on, [merged cleanup](removal.md#merged-workspaces) removes a
merged workspace nobody uses, whether or not its agent ran `done`.

A scoped agent marks only its own space or a space below it done.

### What `done` does

`done` first stops the space's [demos](demos.md). `--keep-demos` leaves them
running until cleanup removes the space.

It then records the completion and closes the space's open
[threads](threads.md).

Running `done` again replaces the recorded completion when anything it records
differs. It fails when it would change nothing: the same completion, no open
thread to close and no `hooks.post_done` to run again.

A missing `--attach` file or an invalid `--summary` fails before `done` stops
or records anything.

### Hand back results

`--attach FILE` hands back a file, such as a report. It takes up to 64 files of
at most 100 MiB each. Shoal copies each into the space's
[attached files](files.md) as a result:

- the completion lists their names in `results`, and `status` names them;
- the completion notice ends with `results in shoal files <space>:`, the first
  three names and how many more there are.

`--summary TEXT` states what the work found in one line. The line holds at most
512 bytes without control characters, as for `shoal send`. The completion
records it as `summary`, and it becomes the space's
[status line](progress.md). `status` shows it, and the completion notice ends
with `; summary: TEXT`. A `done` without `--summary` records none.

### Work integrated elsewhere

`--integrated` records that another branch has the work under new commits, as
after a cherry-pick or a rebase that edited them. Until HEAD moves, commits
that no remote or default branch has no longer keep the workspace from
completion cleanup. Uncommitted files still keep it, and removal keeps its
branch. `status` shows the workspace as integrated elsewhere.

### Done chats

A [chat](chats.md) records that its work is finished at the HEAD of where it
works: the worktree it runs in, its repository's checkout, or an empty HEAD
without a repository. Its agent keeps running until completion cleanup removes
the chat. A chat has no branch of its own, so `--integrated` is refused.
Otherwise `done` acts on a chat as on a workspace.

`list --chats` shows a done chat as `finished`, and `shoal inbox` and the menu
list it. Its record carries the `completion` until `shoal done --undo`
withdraws it. Its closed linked items stop being polled, as a done workspace's
do.

### Cleanup after `done`

[`[cleanup.done] enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
decides whether `done` requests cleanup. `--cleanup` requests it when the
setting is off.

Completion cleanup waits no idle period, only the
[grace period](removal.md#grace-period). It then removes the space and the spaces below it, stopping their
agents and tracked commands. It keeps branches and releases resources as every
removal does. A tracked agent's exit starts a cleanup sweep right after its
exit hook, without waiting for the next poll.

A workspace goes once its work is kept:

- With a watched PR, it goes once the PR's merge requirements are met.
- Without one, its files must be clean and every commit pushed, as
  [automatic cleanup](removal.md#automatic-cleanup) means it, unless the
  completion is integrated.

Shoal checks the files and the done HEAD again after it stops the agent and
runs the removal hooks. When HEAD changed, cleanup waits for a new `done`.
A chat goes whatever PRs it watches.

A hold, a thread the space opened after `done` that waits, or a pending
[PR proposal](approvals.md#approval-for-agent-issues-and-prs) keeps the space.
A failed cleanup keeps the space and its ownership, and `status` shows the
error with the completion's choice. To review the work in a space,
[hold](removal.md#holds) it. Removal, except of a disposable workspace, can
be undone with [`shoal restore`](removal.md#checkpoints-and-restore).

### Without cleanup

A completion without cleanup keeps the space from idle and PR cleanup, across
restarts, until a `done --cleanup` or an explicit removal. It leaves tracked
commands running. [Merged cleanup](removal.md#merged-workspaces) still applies.

A linked issue likewise keeps a workspace from idle cleanup, but not from
merged cleanup.

### Withdraw a completion

`shoal done --undo [space]` withdraws a recorded completion. It fails when
none is recorded. A scoped agent withdraws only its own space's completion or
that of a space below it.

The results stay among the attached files. The summary goes with the
completion, but the status line stays. Linked issues and watched PRs stay.
`--undo` does not keep the space: cleanup goes on as if `done` had not run.

With `--json`, it returns `{"withdrawn": <completion>}`.

### The completion hook

`hooks.post_done` runs in the daemon after Shoal records the completion, and
before cleanup. It runs even when the space is kept. Each `done` runs it again,
and a daemon restart does not replay it.

The hook runs without a terminal, with a 60-second limit. Its working
directory is the one the space's agent works in. A chat without a repository
runs the global config's hook in its `root_dir`, with the global config's
`[env]`. The hook receives the [hook environment](setup.md) and
`SHOAL_DONE_CHOICE`, `keep` or `cleanup`.

A failed hook records a `hook_failed` alert. It neither undoes the completion
nor blocks cleanup.

While the hook runs, Shoal refuses lifecycle and permit changes. The hook
holds its space's completion and PR lock, so it must not run Shoal commands
that change that space's completion or the PRs it watches.

### The agent-exit hook

`hooks.post_agent_exit` runs when the agent of a ready workspace or a chat
exits, beside the exit's alert. It also runs for configured agents and
disconnects. It runs after Shoal acknowledges the wrapper's exit, and not while
a removal has the space. Plain commands and restarts run no exit hook, and
removal runs its own hooks.

It runs where `hooks.post_done` runs, under the same rules, with these
variables:

- `SHOAL_AGENT`: the agent.
- `SHOAL_AGENT_EXIT_CODE`: the exit code, empty on a disconnect.
- `SHOAL_AGENT_EXIT_COMPLETE`: `true` when none of the agent's processes
  remain, otherwise `false`.

A failed hook only records `hook_failed`. An agent's exit does not complete its
assignment. The exit hook may run `shoal done` only when no `hooks.post_done`
is configured.

## Ready for review

```sh
shoal ready                   # Mark every linked item, or the space when none are linked
shoal ready pr 12             # Mark one linked PR; `ready issue` marks the linked issue
shoal ready --undo            # Withdraw every mark; select a kind or item as for unlink
shoal ready --undo --draft    # Also return the PRs whose marks it withdraws to draft
```

A ready mark says that the agent needs the user before it can go on. Either
its work is ready for review, or it has a decision or answer to ask them for.
The mark is on a linked issue or PR, or on the space itself when nothing is
linked. It means the same in a workspace and a chat.

Marks never notify, record completion or affect cleanup. To ask for attention,
use `shoal send`.

### Mark items

`shoal ready` selects items as `shoal link` does, and each selected item must be
linked. Selecting a kind with no linked items is an error.

A linked PR takes a mark only while it is open and its head on the forge is
HEAD. `shoal ready` skips any other PR and prints each with its reason on
stderr. It fails when it skipped a PR that was selected, or when nothing else
is left to mark.

Marking again refreshes a mark. Marking linked items replaces the mark made
while nothing was linked. A mark carries no commit, so new commits leave it as
it was.

A scoped agent marks only its own space. For the user, `--space` selects
another workspace or a chat.

### Where marks show

`status` and `list --json` list marks as `review`, each with `kind`, `url` and
`created_at`. `shoal inbox` lists them across repositories.

While a marked space's agent reports `working`, lists show the space `working`
and the inbox leaves its marks out. They show again once the agent's turn ends.

Each mark and withdrawal is a [space event](log.md#space-events).

### Withdraw marks

`shoal ready --undo` withdraws marks. It fails when it selects none.

Unlinking an item withdraws its mark, and removing the space deletes all its
marks.

### Draft PRs

Before the hook runs, `shoal ready` takes each marked open draft PR out of
draft, as `shoal pr edit --no-draft` would.
[`[pr] undraft_on_ready = false`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs)
keeps the PR a draft and the mark local.

`shoal ready --undo --draft` first returns the open PRs whose marks it
withdraws to draft. `[pr] draft_on_unready = true` does so by default, and
`--no-draft` overrides it.

When the forge refuses the change, the command prints the forge's error and
exits non-zero. Both commands keep the marks, so either can be retried.

Marks never change issues. Unlinking a PR leaves its draft state as it is.

### Convention warnings

`shoal ready` then adds each marked item's label
[convention warnings](issues.md#repository-conventions) to its mark as
`warnings`, and prints them on stderr. Shoal never stores them.

### The ready hook

`hooks.post_ready` runs in the daemon after Shoal records the marks, under the
rules of [the completion hook](#the-completion-hook).
`SHOAL_REVIEW_MARKS` holds the JSON array of the marks just recorded. A failed
hook records `hook_failed` and keeps the marks. Every `shoal ready` runs it
again.

### Marks in chats

A [chat](chats.md)'s checkout is not on a PR's branch. So a chat marks each
linked open PR whatever its head on the forge. It also marks its linked issue,
or the chat itself when nothing is linked.

`list --chats` shows a marked chat as `ready`. `inbox` lists its marks, and
`list --json` lists them as the chat's `review`.

The [review gate](reviews.md#local-reviews) does not apply to a chat's marks.

A chat's `hooks.post_ready` runs where the chat works: its repository's
checkout or the worktree it runs in. It gets the repository's `[env]`, whose
`{path}` is that directory and `{repo}` the repository. A chat has no
`{workspace}`, `{branch}` or ports.
