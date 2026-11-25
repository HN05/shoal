# Removal and cleanup

This file says what `shoal rm`, `checkpoint`, `restore`, `absorb` and `cleanup`
do, and how holds and automatic cleanup keep or remove spaces.

`shoal rm` removes a workspace or a chat the same way. It takes several
spaces and asks for confirmation once for all of them. It returns once Shoal has
reserved the space and checked that it can go: a refusal, such as unsaved work it
was not told to delete, a hold or another operation under way, fails the command
and leaves the space as it was. Stopping its agent and commands, removing the
spaces below it, its hooks, the checkpoint and deleting the worktree then go on
in the background, and `--json` reports `background` with the spaces it takes
first instead of what it did. A failure from then on keeps the space and
arrives as a `removal_failed` alert, or for an agent's removal as a message
to that agent, saying why; `shoal rm` tries again. Deleting the worktree fails
once it has run for ten minutes, and closing a zmx session once it has run for
30 seconds. A removal that stopped before it finished, because the daemon
stopped or its work ended early, leaves the workspace `failed`, its log saying
so, and `shoal rm` finishes it. `--wait`
returns only once the space is removed and reports the branch it kept.

Removal deletes the workspace's branch when a remote-tracking branch or the
default branch has every one of its commits, including squash and rebase merges
the default branch's remote-tracking branch knows about. A branch with commits
only it has stays, and so do the default branch, a branch another workspace
builds on, and every branch while the default branch is unknown. Removal never
asks about the branch. Shoal deletes a
kept branch later, once a remote or the default branch has all its commits, unless
it moved, is checked out, belongs to a workspace again or a workspace builds on
it.
Uncommitted changes, untracked files and commits on a detached HEAD are deleted
only after `Are you sure? [y/N]`, which lists the changes with Git status codes
first, or with `--yes`. Large previews show an omitted-entry count; use
`git status` for the full list. Every removal of an existing worktree, unless the
workspace is disposable, first keeps them, with HEAD, in a
[checkpoint](#checkpoints-and-restore).

A [workspace at a commit](create.md) has no branch to delete or keep. The
commit it started at counts as kept, so a fork PR's head or a local commit it was
created at never keeps it from cleanup; commits made on top of it and uncommitted
changes are unsaved work as on a detached HEAD. `shoal restore` reopens it
without a branch at the checkpoint's HEAD.

`--disposable` on `shoal start` and `shoal create` creates a workspace that holds
nothing worth keeping, such as one a CI job runs in: every removal, `shoal rm`,
its parent's removal and idle or other cleanup, deletes it without inspecting its
unsaved work or asking, keeps no checkpoint, and deletes its branch whatever
commits only it has while the worktree is still on it. Removal deletes what Git
could not, and when that or Git's own removal is cut short, or a checkout that
failed leaves its directory, the next removal deletes the rest and keeps the
branch. It applies only to a new
branch, never to an existing one that removal would then delete. Holds, waiting threads and running commands keep it from
unattended removal as they keep any workspace. `status` shows `Disposable: yes`
and `--json` `disposable`.

Removal stops tracked commands, then the space's
[untracked processes](daemon.md#untracked-processes) as `shoal stop --processes` does,
including those hooks and `shoal internal env` exports started. Automatic cleanup
leaves out leftovers working in the worktree, which keep the workspace as
activity instead. Ownership and removal policy are checked again after removal hooks,
and the branch decision uses that last inspection. Ignored files go; shared caches
stay; Git hooks are disabled. When its time limit or a signal stops a
`git worktree remove` partway, Shoal checks the tracked files it deleted out
again at once. When the daemon's stop cut it short, or that fails, the workspace
stays failed, and they come back when the daemon next starts or at the next
removal. The next removal continues with the ignored files left. Once Git
deleted the worktree's admin directory, Git no longer knows the worktree, and
the next removal deletes what is left of it as it does a disposable
workspace's, `--json` reporting `disowned`: it asks first, counts the files
left that differ from its branch's commit as uncommitted, keeps them laid over
that commit in the checkpoint, and keeps the branch.
Git protects other checkouts. Failed ancestry or exact-ref checks stop removal
with the Git diagnostic, as they do branch selection and refresh; a missing ref
is a negative answer. Shell integration returns to `<root_dir>/<repo>`.

## Kept branches

```sh
shoal cleanup branches --dry-run              # List kept branches and their unpushed commits
shoal cleanup branches                        # Pick kept branches to delete; Tab selects several, Ctrl-A all
shoal cleanup branches fix-login --yes        # Delete one kept branch and its unpushed commits
shoal cleanup branches --all --repo project   # Delete every kept branch of a repository
```

`shoal cleanup branches --dry-run` lists the branches removal kept and Shoal has
not deleted yet, in `branches`, with how many of their commits no remote-tracking
ref or the default branch has, and their last commit.
`shoal cleanup branches` deletes them with those commits after `Are you sure? [y/N]`, or
with `--yes`; it checks every branch first and deletes none when one has moved
since it was listed, is checked out or is no longer kept, or when its
repository's default branch is unknown. Name a branch kept in several repositories
with `--repo`. The removal's checkpoint still holds the commits until it expires.
Scoped processes list and delete kept branches as their space's
[`admin` permission](modes.md#permitted-actions) allows.

## Checkpoints and restore

```sh
shoal checkpoint --name "before migration"  # keep the current workspace's state
shoal restore                     # pick a removed space to open again
shoal restore <id>                # open the removed space with that ID again
shoal restore 7 --checkpoint      # pick one of workspace 7's checkpoints
shoal restore 7 --checkpoint 3    # return workspace 7 to checkpoint 3
```

A checkpoint is a commit of HEAD plus the uncommitted and untracked files, without
ignored ones, under `refs/shoal/checkpoints/<commit>` in the
repository; taking one leaves the worktree and its index untouched.
`shoal checkpoint [workspace]` takes one, and Shoal takes one when an agent
exits or is stopped, before a restore to a checkpoint, when a [copy](create.md#copies)
starts, and on every removal of an existing worktree except a disposable
workspace's. An automatic checkpoint that would keep what the newest one keeps
extends that one instead. When the unsaved files include an untracked nested
repository or a changed submodule, whose files it cannot hold, the checkpoint
keeps HEAD only. The checkpoint of an agent's exit or stop also keeps HEAD only
when the unsaved files exceed `archive_max_mb`, unless they are a copy's
starting files; every other checkpoint keeps them whatever their size. Checkpoints expire after
`retention_days`, a copy's starting checkpoint only once the copy is gone; their
numbers are never reused, so a number names one checkpoint for good. An expiry
pass that deletes a checkpoint then runs `git prune` with the repository's
`gc.pruneExpire` (two weeks unless set), even where automatic gc is off: it
deletes every unreachable loose object older than that, the expired
checkpoints' and any other, as Git's own gc would; objects already packed stay
until that gc.

`shoal restore [workspace] --checkpoint <number>` keeps the current state in a new checkpoint,
then moves the workspace's branch to the checkpoint's HEAD and puts its files back
as uncommitted changes, deleting tracked changes and untracked files it lacks.
It refuses while an execution other than the caller's own runs in the workspace,
during a Git operation, on another branch than the checkpoint's, when a
checkpoint could not keep the unsaved files, or when the checkpoint has a path at,
inside or around an ignored file, which it would delete or overwrite and no
checkpoint keeps. Workspace processes may list and
restore their own workspace's checkpoints.

`--checkpoint` without a number opens a picker of the workspace's checkpoints
and restores the one picked; without a terminal, or with `--json`, it lists them
instead. It takes one workspace, and none of the flags that resume agents.
`shoal restore` without a space offers the removed spaces, workspaces then
chats; without a terminal, or with `--json`, it lists them instead, each led by
the ID that selects it, a workspace with the number of its removal's
checkpoint. A space that is not removed is refused, naming the [`shoal
resume`](daemon.md#recovery) that brings its agent back.
`shoal restore <id>` of a removed workspace opens that removed workspace again under its ID, with a new number, on its branch and recorded base, which restores its stack link while the base
workspace exists, recreating the branch at the removed HEAD when it was deleted,
puts its unsaved files back as uncommitted changes, links its issue and its
opened and related items, saves the agents stopped by the removal or before it
and the last sessions, runs setup, links its PRs, and keeps its checkpoints, log and attached
files; a link or save that fails is a warning. It goes below the space it was
below while that exists and takes it as a child; otherwise it opens at the top
level with a warning naming that space. It restores alone: the spaces removed
with it keep their records. The record of a workspace converted into a chat, which
keeps the workspace's ID, takes an ID of its own with the checkpoints, which
`shoal restore` lists and opens it under beside that chat. Once its post-setup hook succeeds, it resumes those agents and last
sessions as `shoal resume` resumes a stopped space's, in the same run, queued unless `--now` or `--focus`, under
`--permission`; one it cannot resume is reported and stays saved, and the workspace
stays open. `--no-resume` keeps them saved for a later `shoal resume`, keeping the
workspace from idle cleanup as stopped work does.
An agent whose session another space here holds, running or stopped, starts
from a fork of it, as a [copy](create.md#copies)'s does, with a warning naming that
space; this holds for a chat's restore too. A branch that no longer contains
the removed HEAD, as after a removal during a rebase, leaves the workspace to
reopen, with a warning, on a branch named after the short commit. It refuses
when another workspace uses the branch, when neither branch contains the
removed HEAD, or when the branch moved past HEAD while unsaved files would have
to be applied to it; the error names the commit to recover by hand. A scoped process restores a
removed space, of either kind, whose recorded parent exists and its scope
reaches, always below that parent, and a chat only where it could start one:
in a worktree its scope reaches, with permissions its own space holds; any other
removed space is the user's to restore, and the refusal says why. Its restore list and picker offer only those
it may restore.

Removal records the branch, base, parent, links, stopped agents and last sessions with its checkpoint, and
keeps them until the checkpoint expires.

Every removal of a chat, by cleanup too, records its name, parent, links, mode,
the permissions given to its agent, where it ran, its session, and its stopped
agent and last session, until `retention_days` after the removal, as its
attached files stay. `shoal restore <id>` of a removed chat records it again
under its ID, with a new number, below its parent as a workspace's restore
places one, in the place it ran: the checkout, the `root_dir` it ran in, which
must still exist, or the worktree of its workspace, which must exist and be
ready; while that workspace is removed, the restore refuses and names `shoal
restore <workspace id> <chat id>`, which opens the workspace first. It links
its items and PRs again, gives back its attached files, and restores its agent
as a stopped chat's, under the same flags; a chat that saved nothing of its
agent starts a new one, with a warning. Links cannot change once removal
starts, so the record holds the final ones. Removal stops agents as `shoal stop`
does, keeping their recovery records in the removal record; interrupted commands
are not saved. When the removal's checkpoint keeps HEAD only, merged cleanup keeps the workspace instead.
[`[cleanup.restore]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup) sets `retention_days` and `archive_max_mb`.

## Absorb a space

`shoal absorb <space> [--into <space>]` folds a space into another and
removes it. Your own terminal absorbs any space, by default into its parent,
and a top-level space only with `--into`; an agent absorbs a space below its
own, by default into its own space. `--into` names any space your scope
reaches, other than the absorbed space, a space below it, or a chat in its
worktree or a space below one. A space whose completion is not recorded at its current HEAD, or
with a chat in its worktree that is not done, is refused unless `--force`,
which stops their agents first; a done space's running agent stops with its
removal.

Where both are workspaces of one repository, the absorbed space's branch
merges into the other's branch in its worktree, by `-X` and `--message` as
[a local merge](branches.md#local-merges) takes them, else by `[merge]
method`; a merge commit or a squash without `--message` gets a message naming
the absorbed space and its branch. The absorbed space's uncommitted and
untracked files then carry over into that worktree as uncommitted changes,
never as a commit; `--discard-uncommitted` leaves them behind. The worktree
that takes them need not be clean: absorbing refuses, changing nothing, when
the merge would not apply cleanly, the merge or the carried files would
touch a file it has uncommitted changes to, or the carried files would replace
a file it ignores, or while another lifecycle operation, such as a stop or a
removal, runs in either workspace. A squash or rebase records the
absorbed space's completion as integrated. The default branch is neither
fetched nor pushed, and `[merge] enabled`, the `merge` permission and review
gates do not apply.

A chat has no code to merge, and a space that takes the work without a branch
in its repository takes none: a workspace with commits the default branch lacks
or uncommitted files is then refused, naming `shoal convert workspace`, unless
`--force` leaves its code behind.

The space that takes the work also takes the absorbed space's links, with their
relations and watches, its attached files and its open threads, those it
opened under its own numbers and those it held held by it. The absorbed space's pending PR proposals and push requests are
withdrawn; its issue proposals stay with the repository. The spaces directly
below it move up one level, below its parent, and each logs its parent change;
a workspace built on its branch then builds on the branch that took the merge,
unless that branch builds on it, and else on what the absorbed branch built on, its PRs are retargeted there and its agent is
told the rebase command. Both spaces' logs record the absorb, and when you are
not the agent of the space that took the work, that agent gets a message
saying what it took over. The absorbed space then goes as `shoal rm` removes
it, in the background, keeping a checkpoint and its branch; a hold of the user's
refuses an agent as its `user_holds` permission says. `shoal restore` reopens
its files, without what moved. `--json` reports the merge, the carried files and
everything that moved.

## Holds

Holds are caller-named claims of a space's use, a workspace's or a chat's,
independent of assignment completion; `--space` names the space, and the
current one by default. They persist across command exits and daemon restarts
and block every unattended removal, as waiting threads, pending PR proposals and
push requests, and queued agent launches do; releasing the last hold lets
cleanup recheck its usual conditions. Idle cleanup restarts its timer after
release. Removal releases holds, a parent's removal those below it too; a hold
never keeps the space's parent. A hold stays with its space when the space
converts to the other kind.

Reacquiring a name returns the original hold, including its reason. A
workspace takes one only while it is ready, with a verified worktree, and no
resource hook runs; no space takes one while a removal is under way.
Release remains available while the space exists. Each hold records
in `placed_by` whether the user or an app hosting an external session placed it
(`user`), or an agent (`agent`). Scoped processes hold and release the
spaces below their own directly. Holding their own space is an approval
request with the target `hold/<name>`, which needs `--reason` and goes to the
nearest space above with an agent, as other [approval requests](approvals.md#resource-approvals)
do: a workspace or a chat, which needs no grant to decide it, else the user;
approval places the hold,
and acquiring again returns it.
Scoped processes release the holds agents placed, on their own space
or below it, and the user's as their space's [`user_holds` permission](modes.md#permitted-actions) allows. `status` and `list --json` show holds on the space's record. `rm` lists holders before
confirmation and includes them in its JSON result; `--yes` skips confirmation.

## Automatic cleanup

Idle, clean, fully pushed worktrees are removed after
[`[cleanup.auto] idle_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup), 10 by default, or sooner when [disk space](load.md) runs low.
Deleted directories are forgotten even when disabled, releasing resources and
retaining branches; moved worktrees or recorded commands need manual recovery.
HEAD and commands reset the timer; file changes, including ignored files, restart
it when it expires, and Git state such as unpushed commits is checked when it
starts and when it expires.
Running/unknown commands, saved stopped work, a chat running in the worktree or
saved there, directory users (including shells), dirty/unpushed work,
simulator leases, permits, holds, open [threads](threads.md), pending PR proposals and push requests, the CI runs of a merge the space follows, and failed checks block cleanup.
So does the running agent of the nearest space above with one, in any turn
state, while no agent of the space's own works there: none has ever started in
the workspace, as in a child it created for its subagents, or the chat's agent
stopped; once that agent stops, the timer starts over.
Pushed means reachable from locally known remote branches, merged into the
default branch as removal decides, or with its changes on the branch the
workspace builds on: its base workspace's branch, otherwise the local branch
it was created from; no fetch.
A restore of an idle agent waiting for an update, without a
[timer](sleep.md#sleep-timer) or a [turn retry](agents.md) that will wake it, blocks nothing: removal cancels
it and keeps the agent stopped, so `shoal restore` brings it back with its
space.
Sweeps run about every 30 seconds; a failed step does not skip the others, and
timers reset on restart.

[Chats](chats.md) follow the same rules, with their repository's settings and
none of the conditions on a worktree: a chat goes once its agent stopped, with
the spaces below it, unless a restore waits to run it, a hold, a waiting
thread it opened, or for idle cleanup a session Shoal saved, as saved agents keep a
workspace, a completion, a linked issue or a watched PR keeps it. Merged cleanup removes one whose agent has been stopped for its
`idle_minutes` and that has no space below it, and cleanup after `shoal done` removes a done chat, stopping its
agent.

### Grace period

Every automatic removal except for low disk space waits [`grace_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup) from the
first sweep that would remove the workspace, so its agent keeps running while you
decide to take the work back: idle, merged and PR cleanup, and cleanup after
`shoal done`. A workspace that would not pass removal's checks, such as one with
unsaved work after `done`, does not start it. `shoal status` shows when the grace
period ends; a sweep that no longer removes the workspace, as after `done --undo`, a
new commit or new activity, ends it, and a later attempt starts a new one. The end
time survives restarts, except that idle cleanup starts over with its timers. `shoal rm`, `shoal cleanup` and disk-space removal do not wait.

### Merged workspaces

Where [`[cleanup.merged] enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup)
turns it on, a workspace whose branch merged into the default branch is removed once nothing
has used it for [`[cleanup.merged] idle_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#cleanup), whether or not its agent ran `shoal done` or it
has linked issues and PRs. Merged has the meaning [removal](removal.md)
gives it, against the default branch's remote-tracking branch as the
[sync](repo.md) last fetched it, fetching it again when no sync has for ten
minutes; until a fetch succeeds, the repository's workspaces are kept. Uncommitted and untracked files go into the
[removal's checkpoint](#checkpoints-and-restore); they keep the workspace when the checkpoint cannot keep
them all or the branch has no commits of its own. A merge, rebase or other
Git operation under way, open [threads](threads.md), running/unknown commands,
a running chat in it, directory users, leases, permits, holds,
workspaces stacked on it and any space below it, here or on another machine,
keep it too; PR cleanup moves stacked workspaces once the base's PR merges. Shoal records the
time of each command start and exit, lease and hold change, and takes the newest
change time of the worktree's files, including ignored ones, and of its HEAD,
index and reflog; the period runs from the latest of those and survives restarts.
Saved agents and stopped commands do not keep a merged workspace; removal drops
queued restores and keeps the agents for `shoal resume`.
Removals record the `merged` event cause.

```sh
shoal cleanup                        # Pick what to remove; Tab selects several
shoal cleanup --dry-run              # List what is left behind, and why other spaces stay
shoal cleanup spaces                 # Remove every idle and merged space now
shoal cleanup spaces 3               # Remove one, or report why it stays
shoal cleanup caches --repo project  # Empty one repository's shared caches
```

`shoal cleanup` lists what Shoal left behind: in `removed`, the idle and merged
spaces it would remove now, without their idle periods and also where those
rules are disabled, with their IDs in the same order in `removed_ids`; in `branches`, [kept branches](#kept-branches); and in `caches`,
[shared caches](setup.md#shared-caches), each repository's with the `outcome` `emptied`,
`busy` or `failed`. `kept` gives, by `space`, the reason `status` gives for keeping each other
space, `agent_running` for one whose agent runs. Without a terminal, or with `--dry-run`, it stops there. On a terminal,
a picker offers the spaces, kept branches and caches nothing is using, with
nothing selected; Tab selects several, and deleting kept branches asks first unless
`--yes` is given. `shoal cleanup spaces`, `branches` and `caches` act on one
kind alone, where only `branches` without names or `--all` opens a picker; `spaces` takes spaces by number or ID, and those that stay are reported in `kept`, and
any it retained in `retained`. `--repo` limits each kind to one repository.
Removals record the `manual` event cause. Scoped processes run it as their space's
[`admin` permission](modes.md#permitted-actions) allows.
