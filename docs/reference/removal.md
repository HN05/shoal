# Removal and cleanup

This file says what `shoal rm`, `checkpoint`, `restore`, `absorb` and `cleanup`
do, and how holds and automatic cleanup keep or remove spaces.
[Completion and cleanup](../design/cleanup.md) gives the reasons.

## Remove a space

```sh
shoal rm 3                 # Remove space 3 and the spaces below it
shoal rm 3 5 --yes         # Remove two spaces, deleting their unsaved work
shoal rm 3 --wait          # Return once the space is removed
```

`shoal rm` removes a workspace or a chat the same way. It takes several spaces
and asks for confirmation once for all of them.

The command returns once Shoal has reserved the space and checked that it can
go. A refusal fails the command and leaves the space as it was. Shoal refuses,
for example, unsaved work it was not told to delete, a hold, or another
operation under way.

The rest runs in the background: Shoal stops the space's agent and commands,
removes the spaces below it, runs its removal hooks, takes the checkpoint and
deletes the worktree. With `--json`, `shoal rm` reports `background` with the
spaces it removes first, instead of what it did. `--wait` returns only once the
space is removed, and reports the branch it kept.

When the worktree is gone, shell integration returns to `<root_dir>/<repo>`.

### Failed removals

A failure in the background keeps the space. Shoal reports why in a
`removal_failed` alert, or, when an agent asked for the removal, in a message
to that agent. `shoal rm` tries again.

Deleting the worktree fails once it has run for ten minutes. Closing a zmx
session fails once it has run for 30 seconds.

A removal can stop before it finishes, because the daemon stopped or its work
ended early. The workspace is then `failed`, and its log says so. `shoal rm`
finishes it.

### Unsaved work

Uncommitted changes, untracked files and commits on a detached HEAD are unsaved
work. Removal deletes them only after `Are you sure? [y/N]`, or with `--yes`.
The prompt first lists the changes with Git status codes. A long list says how
many entries it leaves out; `git status` shows them all.

Every removal of an existing worktree first keeps the unsaved work, with HEAD,
in a [checkpoint](#checkpoints). A disposable workspace's removal keeps none.

### Branches

Removal deletes the workspace's branch when a remote-tracking branch or the
default branch has every one of its commits. That includes squash and rebase
merges the default branch's remote-tracking branch knows about. Removal never
asks about the branch.

Removal keeps these branches:

- a branch with commits only it has,
- the default branch,
- a branch another workspace builds on,
- every branch while the default branch is unknown.

Shoal deletes a [kept branch](#kept-branches) later, once a remote or the
default branch has all its commits. It leaves the branch when it moved, is
checked out, belongs to a workspace again or a workspace builds on it.

A [workspace at a commit](create.md) has no branch to delete or keep. The
commit it started at counts as kept, so a fork PR's head or a local commit it
was created at never keeps it from cleanup. Commits made on top of it, and
uncommitted changes, are unsaved work, as on a detached HEAD. `shoal restore`
reopens it without a branch at the checkpoint's HEAD.

### Disposable workspaces

`--disposable` on `shoal start` and `shoal create` creates a workspace that
holds nothing worth keeping, such as one a CI job runs in. It takes only a new
branch: an existing one is refused, since removal would then delete it.

Every removal deletes a disposable workspace without asking or inspecting its
unsaved work: `shoal rm`, its parent's removal, and idle or other cleanup. The
removal keeps no checkpoint. It deletes the branch, whatever commits only it
has, while the worktree is still on it.

Holds, waiting threads and running commands keep a disposable workspace from
unattended removal, as they keep any workspace. `status` shows
`Disposable: yes`, and `--json` `disposable`.

Removal deletes what Git could not. When that deletion or Git's own removal is
cut short, or a failed checkout leaves its directory, the next removal deletes
the rest and keeps the branch.

### Processes

Removal stops the space's tracked commands. It then stops its
[untracked processes](daemon.md#untracked-processes) as
`shoal stop --processes` does, including those that hooks and
`shoal internal env` exports started. Automatic cleanup leaves out leftovers
that work in the worktree: they count as activity and keep the workspace.

### Deleting the worktree

Removal runs its hooks, then checks ownership and removal policy again. The
branch decision uses that last inspection.

Removal deletes ignored files and keeps shared caches. It disables Git hooks.
Git protects other checkouts. A failed ancestry or exact-ref check stops
removal with Git's diagnostic, as it stops branch selection and refresh. A
missing ref counts as a negative answer.

A time limit or a signal can stop `git worktree remove` partway. Shoal then
checks out again at once the tracked files it deleted. When the daemon's stop
cut it short, or that checkout fails, the workspace stays `failed`. The files
come back when the daemon next starts or at the next removal. The next removal
continues with the ignored files left.

Once Git has deleted the worktree's admin directory, Git no longer knows the
worktree. The next removal deletes what is left as it deletes a disposable
workspace's, and `--json` reports `disowned`. It still asks first, and it keeps
the branch. It counts the files left that differ from the branch's commit as
uncommitted, and keeps them laid over that commit in the checkpoint.

## Checkpoints and restore

```sh
shoal checkpoint --name "before migration"  # Keep the current workspace's state
shoal restore                     # Pick a removed space to open again
shoal restore <id>                # Open the removed space with that ID again
shoal restore 7 --checkpoint      # Pick one of workspace 7's checkpoints
shoal restore 7 --checkpoint 3    # Return workspace 7 to checkpoint 3
```

### Checkpoints

A checkpoint is a commit of HEAD plus the uncommitted and untracked files,
without ignored ones. It lives under `refs/shoal/checkpoints/<commit>` in the
repository. Taking one leaves the worktree and its index untouched.

`shoal checkpoint [workspace]` takes one. Shoal takes one on its own:

- when an agent exits or is stopped,
- before a restore to a checkpoint,
- when a [copy](create.md#copies) starts,
- on every removal of an existing worktree, except a disposable workspace's.

An automatic checkpoint that would keep what the newest one keeps extends that
one instead.

A checkpoint keeps HEAD only when the unsaved files include an untracked nested
repository or a changed submodule, whose files it cannot hold. The checkpoint
of an agent's exit or stop also keeps HEAD only when the unsaved files exceed
`archive_max_mb`, unless they are a copy's starting files. Every other
checkpoint keeps the files whatever their size.

Checkpoints expire after `retention_days`. A copy's starting checkpoint expires
only once the copy is gone. Shoal never reuses a checkpoint's number, so a
number names one checkpoint for good.

An expiry pass that deletes a checkpoint then runs `git prune` with the
repository's `gc.pruneExpire`, two weeks unless set. It runs even where
automatic gc is off. It deletes every unreachable loose object older than that,
the expired checkpoints' and any other, as Git's own gc would. Objects already
packed stay until that gc.

[`[cleanup.restore]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
sets `retention_days` and `archive_max_mb`.

### Return to a checkpoint

`shoal restore [workspace] --checkpoint <number>` first keeps the current state
in a new checkpoint. It then moves the workspace's branch to the checkpoint's
HEAD and puts the checkpoint's files back as uncommitted changes. It deletes the
tracked changes and untracked files the checkpoint lacks.

It refuses:

- while an execution other than the caller's own runs in the workspace,
- during a Git operation,
- on another branch than the checkpoint's,
- when a checkpoint could not keep the unsaved files,
- when the checkpoint has a path at, inside or around an ignored file. The
  restore would delete or overwrite that file, and no checkpoint keeps it.

`--checkpoint` without a number opens a picker of the workspace's checkpoints
and restores the one picked. Without a terminal, or with `--json`, it lists
them instead. It takes one workspace, and none of the flags that resume agents.

Workspace processes list and restore their own workspace's checkpoints.

### Removal records

Removal stops agents as `shoal stop` does and keeps their recovery records in
the removal record. It saves no interrupted commands.

A workspace's removal records its branch, base, parent, links, stopped agents
and last sessions with its checkpoint. The record lasts until the checkpoint
expires.

Every removal of a chat, by cleanup too, records its name, parent, links, mode,
the permissions given to its agent, where it ran, its session, and its stopped
agent and last session. The record lasts `retention_days` after the removal, as
the chat's attached files do.

Links cannot change once removal starts. The record therefore holds the final
ones.

### Restore a removed space

`shoal restore` without a space offers the removed spaces, workspaces then
chats. Without a terminal, or with `--json`, it lists them instead. Each entry
starts with the ID that selects it, and a workspace's also gives the number of
its removal's checkpoint. A space that is not removed is refused, naming the
[`shoal resume`](daemon.md#recovery) that brings its agent back.

A restored space keeps its ID and takes a new number. It goes below the space
it was below while that space exists. Otherwise it opens at the top level, with
a warning naming that space. It restores alone: the spaces removed with it keep
their records.

The restore resumes the space's saved agents and last sessions as
`shoal resume` resumes a stopped space's, in the same run. They are queued
unless `--now` or `--focus` is given, and run under `--permission`. An agent it
cannot resume is reported and stays saved, and the space stays open.
`--no-resume` keeps them saved for a later `shoal resume`, which keeps a
workspace from idle cleanup as stopped work does.

An agent whose session another space here holds, running or stopped, starts
from a fork of that session, as a [copy](create.md#copies)'s does. A warning
names that space.

### Restore a removed workspace

`shoal restore <id>` of a removed workspace opens it again:

- on its branch, recreated at the removed HEAD when it was deleted,
- on its recorded base, which restores its stack link while the base workspace
  exists,
- with its unsaved files back as uncommitted changes,
- linked to its issue, its opened and related items and its PRs,
- with its checkpoints, log and attached files.

It saves the agents stopped by the removal or before it, with their last
sessions, and runs setup. A link or save that fails is a warning. Once the
post-setup hook succeeds, it resumes those agents.

A branch that no longer contains the removed HEAD, as after a removal during a
rebase, leaves the workspace to reopen on a branch named after the short
commit, with a warning. The restore refuses, naming the commit to recover by
hand:

- when another workspace uses the branch,
- when neither branch contains the removed HEAD,
- when the branch moved past HEAD while unsaved files would have to be applied
  to it.

A workspace converted into a chat leaves the chat its ID. Its removal record
takes an ID of its own, with the checkpoints. `shoal restore` lists it under
that ID beside the chat and opens it under it.

### Restore a removed chat

`shoal restore <id>` of a removed chat records it again in the place it ran:

- the checkout,
- the `root_dir` it ran in, which must still exist,
- or the worktree of its workspace, which must exist and be ready.

While that workspace is removed, the restore refuses and names
`shoal restore <workspace id> <chat id>`, which opens the workspace first.

The restore links the chat's items and PRs again and gives back its attached
files. It restores its agent as a stopped chat's, under the same flags. A chat
that saved nothing of its agent starts a new one, with a warning.

### Who restores

A scoped process restores a removed space, of either kind, when its recorded
parent exists and the process's scope reaches that parent. The space always
goes below that parent. A chat also needs a place where the process could
start one: a worktree its scope reaches, with permissions its own space holds.
Any other removed space is the user's to restore, and the refusal says why. A
scoped process's restore list and picker offer only the spaces it may restore.

## Absorb a space

`shoal absorb <space> [--into <space>]` folds a space into another and removes
it.

- Your own terminal absorbs any space, by default into its parent. A top-level
  space needs `--into`.
- An agent absorbs a space below its own, by default into its own space.

`--into` names any space your scope reaches, except the absorbed space, a space
below it, a chat in its worktree or a space below such a chat.

### Done spaces only

Absorb refuses a space whose completion is not recorded at its current HEAD. It
also refuses one with a chat in its worktree that is not done. `--force` takes
such a space and stops those agents first. A done space's running agent stops
with its removal.

### Code

Where both spaces are workspaces of one repository, the absorbed space's branch
merges into the other's branch, in its worktree. The merge uses `-X` and
`--message` as [a local merge](branches.md#local-merges)
takes them, else `[merge] method`. A merge commit, or a squash without
`--message`, gets a message naming the absorbed space and its branch. A squash
or rebase records the absorbed space's completion as integrated.

The absorbed space's uncommitted and untracked files then carry over into that
worktree as uncommitted changes, never as a commit. `--discard-uncommitted`
leaves them behind.

The worktree that takes them need not be clean. Absorb refuses, changing
nothing:

- when the merge would not apply cleanly,
- when the merge or the carried files would touch a file with uncommitted
  changes there,
- when the carried files would replace a file it ignores,
- while another lifecycle operation, such as a stop or a removal, runs in
  either workspace.

Absorb neither fetches nor pushes the default branch. `[merge] enabled`, the
`merge` permission and review gates do not apply.

A chat has no code to merge. A space without a branch in the absorbed space's
repository takes no code either. Absorb then refuses a workspace with
uncommitted files or commits the default branch lacks, naming
`shoal convert workspace`. `--force` absorbs it and leaves its code behind.

### What moves

The space that takes the work also takes the absorbed space's:

- links, with their relations and watches,
- attached files,
- open threads: those it opened, under the new space's numbers, and those it
  held, now held by the new space.

Absorb withdraws the absorbed space's pending PR proposals and push requests.
Its issue proposals stay with the repository.

The spaces directly below the absorbed space move up one level, below its
parent, and each logs its parent change. A workspace built on the absorbed
branch then builds on the branch that took the merge. When that branch builds
on the absorbed one, it builds instead on what the absorbed branch built on.
Its PRs are retargeted to the new base, and its agent is told the rebase
command.

Both spaces' logs record the absorb. When you are not the agent of the space
that took the work, that agent gets a message saying what it took over.

### Removal

The absorbed space then goes as `shoal rm` removes it: in the background,
keeping a checkpoint and its branch. A hold of the user's refuses an agent as
its `user_holds` permission says. `shoal restore` reopens the space's files,
without what moved. `--json` reports the merge, the carried files and
everything that moved.

## Holds

```sh
shoal acquire hold --name review --reason "Review the UI"  # Keep this space
shoal leases hold                                         # List the holds
shoal release hold --name review
```

A hold is a named claim that someone uses a space, a workspace or a chat. It is
independent of completion. `--space` names the space, the current one by
default.

### What a hold keeps

A hold blocks every unattended removal. Waiting threads, pending PR proposals
and push requests, and queued agent launches block it the same way. A hold
persists across command exits and daemon restarts, and it stays with its space
when the space converts to the other kind.

Releasing the last hold lets cleanup check its usual conditions again. Idle
cleanup restarts its timer after the release.

Removal releases a space's holds, and a parent's removal releases those below
it too. A hold never keeps the space's parent. `shoal rm` lists the holders
before it asks, and its JSON result includes them.

### Placing and releasing

Acquiring a name again returns the original hold, with its reason. A workspace
takes a hold only while it is ready, with a verified worktree, and while no
resource hook runs. No space takes one while a removal is under way. Release
stays available while the space exists.

Each hold records who placed it in `placed_by`: `user` for the user or an app
that hosts an external session, `agent` for an agent. `status` and
`list --json` show holds on the space's record.

Scoped processes hold and release the spaces below their own directly. Holding
their own space is an approval request with the target `hold/<name>`, which
needs `--reason`. It goes to the nearest space above with an agent, as other
[approval requests](approvals.md#resource-approvals) do, and else to the user.
That space needs no grant to decide it. Approval places the hold, and acquiring
again returns it.

Scoped processes release the holds agents placed, on their own space or below
it. They release the user's as their space's
[`user_holds` permission](modes.md#permitted-actions) allows.

## Automatic cleanup

Shoal removes idle, clean, fully pushed worktrees once they have been idle for
[`[cleanup.auto] idle_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup),
or sooner when [disk space](load.md) runs low.

Shoal forgets a workspace whose directory was deleted, even with automatic
cleanup disabled. It releases the workspace's resources and keeps its branch.
A moved worktree, or one with recorded commands, needs recovery by hand.

### The idle timer

HEAD and commands reset the timer. File changes, ignored files included,
restart it when it expires. Shoal checks Git state, such as unpushed commits,
when the timer starts and when it expires.

Sweeps run about every 30 seconds. A failed step does not skip the others.
Timers reset when the daemon restarts.

### What keeps a workspace

These keep a workspace from idle cleanup:

- running or unknown commands,
- saved stopped work,
- a chat running in the worktree or saved there,
- directory users, shells included,
- dirty or unpushed work,
- simulator leases,
- permits,
- holds,
- open [threads](threads.md),
- pending PR proposals and push requests,
- the CI runs of a merge the space follows,
- failed checks.

The running agent of the nearest space above with one also keeps it, in any
turn state, while no agent of the space's own works there. That holds when no
agent has ever started in the workspace, as in a child an agent created for its
subagents, or when the chat's agent stopped. Once that agent above stops, the
timer starts over.

Pushed means one of these:

- reachable from the locally known remote branches,
- merged into the default branch, as [removal](#branches) decides,
- with its changes on the branch the workspace builds on: its base workspace's
  branch, else the local branch it was created from.

Cleanup fetches nothing to decide it.

A waiting restore of an idle agent keeps nothing when no
[timer](sleep.md#sleep-timer) or [turn retry](agents.md) will wake the agent.
Removal cancels it and keeps the agent stopped, so `shoal restore` brings the
agent back with its space.

### Chats

[Chats](chats.md) follow the same rules, with their repository's settings and
none of the conditions on a worktree. A chat goes, with the spaces below it,
once its agent has stopped. These keep it:

- a restore that waits to run it,
- a hold,
- a waiting thread it opened.

These also keep it from idle cleanup:

- a session Shoal saved, as saved agents keep a workspace,
- a completion,
- a linked issue,
- a watched PR.

Merged cleanup removes a chat whose agent has been stopped for its
`idle_minutes` and that has no space below it. Cleanup after `shoal done`
removes a done chat and stops its agent.

### Grace period

Every automatic removal except one for low disk space first waits
[`grace_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
from the first sweep that would remove the workspace. That covers idle, merged
and PR cleanup, and cleanup after `shoal done`. Its agent keeps running
meanwhile.

A workspace that would not pass removal's checks starts no grace period, such
as one with unsaved work after `done`. `shoal status` shows when the period
ends. A sweep that no longer removes the workspace ends it, as after
`done --undo`, a new commit or new activity. A later attempt starts a new one.

The end time survives restarts, except that idle cleanup starts over with its
timers. `shoal rm`, `shoal cleanup` and disk-space removal do not wait.

### Merged workspaces

Merged cleanup is off unless
[`[cleanup.merged] enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
turns it on. It removes a workspace whose branch merged into the default branch
once nothing has used it for `[cleanup.merged] idle_minutes`. It does so
whether or not the agent ran `shoal done`, and whatever issues and PRs the
workspace links.

Merged means what it means for [removal](#branches). Cleanup checks it against
the default branch's remote-tracking branch as the
[sync](repo.md#default-branch-sync) last fetched it. When no sync has fetched for ten minutes, cleanup fetches again. Until a
fetch succeeds, it keeps the repository's workspaces.

Uncommitted and untracked files go into the
[removal's checkpoint](#checkpoints). They keep the workspace when the
checkpoint cannot keep them all, or when the branch has no commits of its own.

These also keep it:

- a merge, rebase or other Git operation under way,
- open [threads](threads.md),
- running or unknown commands,
- a running chat in it,
- directory users,
- leases and permits,
- holds,
- workspaces stacked on it, and any space below it, here or on another
  machine.

PR cleanup moves stacked workspaces once the base's PR merges.

Saved agents and stopped commands do not keep a merged workspace. Removal drops
queued restores and keeps the agents for `shoal resume`.

The idle period runs from the latest of these:

- each command start and exit, and each lease and hold change, whose times
  Shoal records,
- the newest change time of the worktree's files, ignored ones included,
- the newest change time of its HEAD, index and reflog.

The period survives restarts. Removals record the `merged` event cause.

## Clean up by hand

```sh
shoal cleanup                        # Pick what to remove; Tab selects several
shoal cleanup --dry-run              # List what is left behind, and why other spaces stay
shoal cleanup spaces                 # Remove every idle and merged space now
shoal cleanup spaces 3               # Remove one, or report why it stays
shoal cleanup caches --repo project  # Empty one repository's shared caches
```

`shoal cleanup` lists what Shoal left behind:

- `removed`: the idle and merged spaces it would remove now, without their idle
  periods and also where those rules are disabled. `removed_ids` gives their
  IDs in the same order.
- `branches`: [kept branches](#kept-branches).
- `caches`: [shared caches](setup.md#shared-caches), each repository's with the
  `outcome` `emptied`, `busy` or `failed`.
- `kept`: by `space`, the reason `status` gives for keeping each other space,
  or `agent_running` for one whose agent runs.

Without a terminal, or with `--dry-run`, it stops there. On a terminal, a
picker offers the spaces, kept branches and caches nothing is using, with
nothing selected. Tab selects several. Deleting kept branches asks first,
unless `--yes` is given.

`shoal cleanup spaces`, `branches` and `caches` each act on one kind alone.
Only `branches` opens a picker, and only without names or `--all`.
`spaces` takes spaces by number or ID. It reports those that stay in `kept`,
and any it retained in `retained`. `--repo` limits each kind to one
repository.

Removals record the `manual` event cause. Scoped processes run
`shoal cleanup` as their space's
[`admin` permission](modes.md#permitted-actions) allows.

### Kept branches

```sh
shoal cleanup branches --dry-run              # List kept branches and their unpushed commits
shoal cleanup branches                        # Pick kept branches to delete; Tab selects several, Ctrl-A all
shoal cleanup branches fix-login --yes        # Delete one kept branch and its unpushed commits
shoal cleanup branches --all --repo project   # Delete every kept branch of a repository
```

A kept branch is one that removal kept and Shoal has not deleted yet.
`shoal cleanup branches --dry-run` lists them in `branches`. Each entry gives
its last commit and how many of its commits neither a remote-tracking ref nor
the default branch has.

`shoal cleanup branches` deletes them, with those commits, after
`Are you sure? [y/N]`, or with `--yes`. It checks every branch first. It
deletes none when one has moved since it was listed, is checked out or is no
longer kept, or when its repository's default branch is unknown. When a
branch is kept in several repositories, `--repo` picks one. The removal's checkpoint still
holds the commits until it expires.

Scoped processes list and delete kept branches as their space's
[`admin` permission](modes.md#permitted-actions) allows.
