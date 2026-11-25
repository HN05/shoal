# Completion and ready marks

This file says what `shoal done` and `shoal ready` do.

## Assignment completion

`shoal done [space]` records that the assignment is finished and notifies the
user. It replaces the whole recorded completion when anything it records differs, and fails when it would change nothing: the same
completion with no open thread to close and no `hooks.post_done` to rerun.
[`[cleanup.done] enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup) defaults to true; `--cleanup` requests cleanup when it is
false. It first stops the space's [demos](demos.md) unless `--keep-demos` leaves
them running until cleanup. `--integrated` records that another branch has the
work under new commits, as after a cherry-pick or a rebase that edited them: until
HEAD moves, commits no remote or default branch has no longer keep the workspace
from completion cleanup. Uncommitted files still keep it, and removal keeps its branch.
`status` shows it as integrated elsewhere. `--attach FILE`, repeated for up to
64 files, hands back files such as a report: Shoal copies each, of at most 100 MiB, into
the workspace's [attached files](files.md) as a result, the completion
lists their names in `results`, `status` names them, and the completion notice
ends with `results in shoal files <workspace>:` and the first three names, then
how many more. `--summary TEXT` states what the work found in one line of at
most 512 bytes without control characters, as for `shoal send`: the
completion records it as `summary` and as the space's
[status line](progress.md), `status` shows it, and the completion notice ends
with `; summary: TEXT`; a `done` without it records none, and `--undo`
deletes it with the completion, leaving the status line. A missing file or an
invalid summary fails before anything is stopped or recorded. The setting follows normal repository and worktree configuration
precedence. Scoped agents may mark only their own workspace or a child workspace done; completion does
not verify or assert a merge. Completion without cleanup keeps the workspace from
idle and PR cleanup, not from [merged cleanup](removal.md#merged-workspaces). Removal, except
a disposable workspace's, is reversible with [`shoal restore`](removal.md#checkpoints-and-restore); to review in a workspace,
[hold](removal.md#holds) it. `done` closes the workspace's open
[threads](threads.md), so completion and PR cleanup wait only while a hold, a
thread opened afterwards that waits or a [PR proposal](approvals.md#approval-for-agent-issues-and-prs)
remains.

Only `done` completes an assignment; issue closure and merged PRs do not. Where
it is on, [merged cleanup](removal.md#merged-workspaces) removes a merged workspace nobody
uses whether or not its agent ran `done`.

A [chat](chats.md), named or the one this runs in, records that its work is
finished at the HEAD of where it works, the worktree it runs in or its
repository's checkout, or an empty HEAD without a repository, and its agent and
session keep running until completion cleanup, requested as for a workspace,
removes the chat and the spaces below it after the grace period, stopping its
agent, whatever PRs it watches. It has no branch of its own, so `--integrated`
is refused. Otherwise `done` acts on it as on a workspace. Its
closed linked items stop being polled, as a finished workspace's do. `work
--chats` shows the chat as `finished`, `shoal inbox` and the menu list it, and
its record carries the `completion` until `shoal done --undo` withdraws it.

`shoal done --undo [space]` withdraws a recorded completion and fails when none
is recorded, leaving its results among the attached files; `--json` returns `{"withdrawn": <completion>}`. Scoped agents may mark only their own workspace or a child workspace undone.
Issue associations and PR watches remain registered. It does not keep the workspace:
cleanup proceeds as if `done` had not run.

`hooks.post_done` runs in the daemon after recording completion, before cleanup,
with the directory the space's agent works in as its working directory, no
terminal, and a 60-second limit; a chat without a repository runs the global config's hook in
its `root_dir`, with the global config's `[env]`.
It follows normal global and repository configuration precedence and receives the
[hook identity environment](setup.md) plus
`SHOAL_DONE_CHOICE=keep` or `cleanup`. It runs even when cleanup retains the
workspace. Failure records
a `hook_failed` alert without undoing completion or blocking cleanup.
Lifecycle and permit changes are rejected while it runs. The hook holds its
workspace's completion/PR gate, so it must not call Shoal commands that change
that workspace's completion or PR registrations. Each `done` runs it again; daemon
restart does not replay it.

`hooks.post_agent_exit` runs alongside agent-exit alerts of a space's agent,
including configured agents and disconnects, after acknowledging the wrapper's
exit, while the space is a ready workspace or a chat and no removal has it. It
runs where `hooks.post_done` would, under the same daemon hook rules and config
precedence, with `SHOAL_AGENT`,
`SHOAL_AGENT_EXIT_CODE` (empty on disconnect), and `SHOAL_AGENT_EXIT_COMPLETE`
(`true` when no owned processes remain, otherwise `false`). Failure only records
`hook_failed`; agent exit does not mark an assignment done. Removal uses its own
hooks, and plain commands and restart do not produce agent-exit hooks. An exit
hook may signal `done` only when no `hooks.post_done` is configured; otherwise the
completion hook would conflict with its lifecycle/permit guard.

Issue associations suppress idle cleanup, not merged cleanup.

Cleanup runs in the daemon without an idle delay; tracked agent exits trigger a
sweep after their exit hook instead of waiting for the next poll. It stops tracked
commands using normal branch retention and resource release. Without a PR registration,
files must be clean and all commits pushed as
[automatic cleanup](removal.md#automatic-cleanup) means it, unless the completion is
integrated; an existing registration keeps its merge requirements. Files and the completed HEAD
are checked again after stopping and removal hooks. Failure retains ownership;
`status` and `inspect` show the completion choice and cleanup errors. Changed HEAD
requires a new `done` signal before completion cleanup can proceed.

Keeping persists across restarts and suppresses idle and PR cleanup until a new
`done --cleanup` request or explicit removal. It leaves tracked commands running.
When cleanup removes the worktree, shell integration returns to the repository's
`<root_dir>/<repo>` directory; keeping or retained work leaves the current directory.

## Ready for review

```sh
shoal ready                   # Mark every linked item, or the workspace when none are linked
shoal ready pr 12             # Mark one linked PR; `ready issue` marks the linked issue
shoal ready --undo            # Withdraw all marks; select a kind or item like unlink
shoal ready --undo --draft    # Also return the PRs whose marks are withdrawn to draft
```

A mark says the agent needs the user before it can go on: the work for a
linked issue or PR, or for the workspace itself when nothing is linked, is
ready for review, or the agent has a decision or answer to ask them for. It means the same in a workspace and a chat.
Items are selected like `link`, and a selected item must be linked; a kind with
no linked items is an error. A linked PR is marked only while it is open and
its head on the forge is HEAD; `shoal ready` skips any other, prints each
with its reason on stderr, and fails when that PR was selected or nothing
else is left to mark. Marking again refreshes the mark, and marking linked
items replaces a mark made while nothing was linked. A mark carries no commit,
so new commits leave it as it was.
A working agent does not wait for the user, so while a marked space's agent
reports `working`, lists show it `working` and the inbox leaves its marks out;
they show again once its turn ends. Marks never notify, record completion, or
affect cleanup; use `shoal send` to
ask for attention. `status`, `inspect` and `list --json` list `review` marks
with `kind`, `url` and `created_at`.
`shoal inbox` lists them across repositories. `unready` fails when
it selects no mark. Unlinking an item withdraws its mark, and removal deletes
all marks. Each mark and withdrawal
is a [space event](log.md#space-events). Scoped callers mark only their own
workspace; `--space` selects another workspace or a chat for unscoped callers.

A [chat](chats.md) marks each linked open PR whatever its head on the forge, since
its checkout is not on the PR's branch, and its linked issue, or the chat
itself when nothing is linked. `list --chats` shows a marked chat as `ready`,
`inbox` lists its marks, and `list --json` lists them as the chat's `review`. The
[review gate](reviews.md#local-reviews) does not apply to a chat's marks; `hooks.post_ready` runs
where the chat works, its repository's checkout or the worktree it runs in, with the
repository's `[env]`, whose `{path}` is that directory and `{repo}` the repository; a
chat has no `{workspace}`, `{branch}` or ports.

`hooks.post_ready` runs in the daemon after marks are recorded, under the same
rules, precedence and limits as `hooks.post_done`, with `SHOAL_REVIEW_MARKS` holding
the JSON array of marks just recorded. Failure records `hook_failed` and keeps
the marks; every `shoal ready` runs it again.

Before the hook, `shoal ready` takes each marked open PR that is a draft out of
draft, as `shoal pr edit --no-draft` would; [`[pr] undraft_on_ready = false`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs) keeps marks
local. `shoal ready --undo --draft`, or `[pr] draft_on_unready = true`, returns the open PRs
whose marks it withdraws to draft first; `--no-draft` overrides that setting.
A failed change reports the forge's error and exits non-zero, keeping the marks
in both commands so either can be retried. Marks never change issues, and
unlinking leaves the PR as it is. `shoal ready` then adds each marked item's
label [convention warnings](issues.md#repository-conventions) to its mark as `warnings`
and prints them on stderr; they are never stored.
