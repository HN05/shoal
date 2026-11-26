# Completion and cleanup

This file covers how a space's work ends and how Shoal keeps it: holds,
completion, absorbing, removal, checkpoints, restores, kept branches and
automatic cleanup. [Completion](../reference/completion.md) and
[removal and cleanup](../reference/removal.md) say what the commands do.

## Principles

Removal is reversible. A workspace's removal keeps a checkpoint and any branch
with work no other branch has, a chat's keeps its record, and `shoal restore`
reopens either. Spaces are
therefore cheap to remove. The one exception is a disposable workspace, which
holds nothing worth keeping, so its removal keeps nothing.

Users should not have to decide about branches. Removal deletes a branch only
when that loses nothing, and keeps every other.

Since removal is reversible, an agent keeps a space only through an explicit
claim on it. One that needs the user asks. A hold, a thread the space opened
that waits, a pending PR proposal or push request, and an agent launch waiting
in the agent queue each keep the space from every unattended removal,
completion and PR cleanup included.

## Holds

A hold is a named claim that someone uses a space. It is independent of
completion, so a finished space stays while someone reviews it. Holds persist
across daemon restarts. Each hold records who placed it: the user, or an app
that hosts an external session, or an agent.

Releasing the last hold makes the space eligible for cleanup again. Release
stays available through failed or active lifecycle operations and hooks, so a
hold never traps a space. Explicit removal lists the holders and releases the
holds with the space's record. A hold never keeps the space's parent, nor
survives the space's removal. A deleted worktree goes even while held: the
cleanup sweep still forgets it.

Scoped callers hold and release the spaces below their own directly, so a
coordinator keeps what its children need. Holding their own space is an
approval request, which approval carries out, since an agent keeps its own
space only through a claim another grants. Agents release the holds agents
placed, and the user's as their `user_holds` permission allows
([permissions.md](permissions.md#creating-and-removing-spaces)).

## Completion

### What completes an assignment

Only an explicit `done` completes an assignment. Closing an issue or merging a
PR does not; [merged cleanup](#merged-cleanup) removes unused merged
workspaces from their state instead. Agent instructions make `done` the last
step of every assignment.

Shoal persists a completion apart from the space's lifecycle state. The
completion binds to HEAD and notifies the space's coordinator, or else the
user. It does not claim that the work merged.

A `done` that would change nothing fails, and so does an `--undo` without a
completion. A misplaced signal is then reported instead of passing silently.

Withdrawing a completion keeps links and merge requirements but never defers
cleanup. Only the claims under [Principles](#principles) keep a space.

### Results

An assignment that ends with a finding rather than something to merge hands
back files with its completion. They are the space's results among its
attached files. Attached files outlive the space, as
[coordination.md](coordination.md#retention) describes, so the user or the coordinator's
agent reads them after the worktree is gone.

### Chats

A chat's `done` records its completion on the chat as a workspace's does, with
cleanup by the same setting. Only a workspace's completion reads a branch,
verifies a worktree or takes `--integrated`, since a chat has no branch another
could integrate. A done chat's agent keeps running until completion cleanup
removes the chat after the grace period. The chat shows as done until `--undo`
withdraws the completion.

### The completion hook

The daemon runs `hooks.post_done` after it persists the completion and before
cleanup. The hook comes from the normal config layers and receives the space's
hook identity and the keep or cleanup choice. It runs where the space's agent
works: a workspace's worktree, or the worktree or checkout a chat runs in. A
chat without a repository runs the global config's hook in `root_dir`.

The hook is bounded in time and excludes lifecycle and permit changes. A
failed hook sends an alert. It neither undoes the completion nor blocks
cleanup. Each explicit `done` runs the hook again. Shoal neither queues hook
events nor replays them on restart.

### Serialization

Completion and PR cleanup serialize per workspace through the workspace's gate.
A cleanup sweep holds a workspace's gate only while it handles that workspace,
its forge lookups and removal included, so commands for one workspace never
wait for another's.

The gate covers the completion hook, so the hook must not call gated Shoal
mutations for its own workspace. A completion without a hook takes no resource
guard, so an agent-exit hook may run `done`. A configured completion hook
cannot nest inside another guarded hook.

### Completion cleanup

`[cleanup.done] enabled` decides whether `done` requests cleanup, and
`done --cleanup` requests it regardless. A completion without cleanup keeps the
space from idle and PR cleanup until another completion requests cleanup or the
user removes the space. Merged cleanup still applies.

Without a PR registration, completion cleanup needs clean files and every
commit kept, or a completion marked integrated. The integrated mark says that
another branch has HEAD's work under commits whose patches were edited. No
check can match those commits, so only the agent that took them can say so. The
mark binds to HEAD like the completion. It keeps the branch, since no remote or
default branch has its commits.

With a PR registration, completion cleanup waits for the
[PR's merge checks](../reference/completion.md#watched-prs). A registered
workspace stays out of idle cleanup until the registration clears or the
workspace goes. `[cleanup.pr] enabled = false` pauses PR cleanup without
touching links, which serve more than cleanup.

Completion cleanup stops tracked executions through the shared removal path. It
then checks the files and HEAD again, after the removal hooks too. A changed
HEAD keeps the workspace until a new `done`. A tracked agent's exit wakes the
cleanup sweep after its exit hook, without waiting for the polling interval.

## Absorbing

`shoal absorb` is how a coordinator takes a finished space's work into its own
space, instead of merging it with Git and removing the space. It needs no
permission of its own, since it ends in removing a space below the caller,
which scoped callers may do. It takes only done work unless forced, because
absorbing ends the space.

Code goes only where a branch of its repository can take it. Absorb uses the
local merge `shoal merge` uses, generalized to a target branch in its own
worktree. That worktree need not be clean: every merge method makes its commit
without touching a checkout, then the target fast-forwards, and Git keeps the
worktree's other changes. Uncommitted files arrive as uncommitted changes for
the taking agent to review, never as a commit.

Absorb checks everything before anything moves. It withdraws what binds to the
absorbed branch, PR proposals and push requests, instead of rebinding it. The
removal path takes the space last, so its checkpoint and branch keep what was
left behind. The space's children move up one level through one operation,
which a general reparenting command can reuse.

## Removal

### The removal path

Manual and automatic cleanup share one removal path:

1. Establish ownership.
2. Stop the owned executions, saving an existing worktree's agent sessions but
   no commands.
3. Run the removal hooks.
4. Take down the owned containers and remove the owned simulators.
5. Take a checkpoint, unless the workspace is disposable.
6. Remove the worktree.
7. Release the leases with the record.

A failure keeps what a retry needs. Removal decides about the branch from its
last inspection, after the hooks, so a hook that commits keeps the branch when
only that branch has the new commits.

Manual removal asks only before it deletes uncommitted or untracked files, or
commits on a detached HEAD, since no branch keeps those.

### What Git leaves behind

Git forgets a worktree once it starts deleting the worktree's files. Removal
therefore deletes what Git could not. When that fails, or a checkout fails, a
later removal deletes what is left without Git, as it deletes a disposable
workspace.

That removal must prove the directory is the workspace's own. The `.git` link
proves it while it names a deleted admin directory of the repository. Git may
delete that link, and `git worktree prune` the admin directory, before a removal
stops. The directory's identity then proves it, which every removal records
before Git begins deleting.

Git no longer has the workspace's HEAD, index or ignore rules at that point.
For a workspace that is not disposable, removal reads the files left against
its branch's commit and takes the commit's missing files as unchanged. The
files that differ are unsaved work, which the checkpoint keeps laid over that
commit.

## Checkpoints

### What a checkpoint holds

A checkpoint is a commit of HEAD plus the unsaved files Git does not ignore.
Shoal builds it with a temporary index, keeps it under
`refs/shoal/checkpoints/<commit>` and records it in the store.

A ref is named by its commit, so it belongs to whichever records keep that
commit. Moving checkpoints to another workspace therefore changes only records.
Shoal deletes a ref that no record keeps. It writes the ref before the record,
so a record never names an unreachable commit. Writing the pair excludes expiry,
but never under the repository's Git gate, which removal holds.

A checkpoint keeps HEAD only when the unsaved files include an untracked nested
repository or a changed submodule, whose files it cannot hold. A snapshot fails
when HEAD or the branch moves while Shoal takes it, so its record's HEAD always
matches its files.

### When Shoal takes one

Shoal takes a checkpoint on request, when an agent exits or is stopped while
the workspace is ready or stopping, before a restore to a checkpoint, on
removal and when a copy starts. Each one but a requested one extends the newest
checkpoint instead when it would keep the same branch, HEAD and files. An
agent's checkpoint waits until the processes the agent left are proven stopped.

`[cleanup.restore] archive_max_mb` caps only the checkpoint of an agent's exit
or stop. That checkpoint's files stay in the worktree, so leaving them out
loses nothing. Above the cap it keeps HEAD only, unless the files are a copy's
starting ones. The cap bounds the time each agent stop spends hashing large
untracked files, and the versions a large changing file would pile up. Every
other checkpoint was asked for or may hold the files' last copy, so it keeps
them whatever their size.

### Expiry

Checkpoints expire after `retention_days`, and so do those that no workspace or
removal record owns. A copy's starting checkpoint expires only once the copy is
gone.

An expiry pass that deletes a checkpoint ref then runs `git prune` with the
repository's `gc.pruneExpire`, never a shorter window. Large loose blobs alone
never start Git's automatic gc, so Shoal prunes itself. The user's window keeps
their recovery time, and the objects that a `git add` or a snapshot under way
just wrote. Shoal does not repack, which would rewrite the user's packs, so
objects already packed stay until the user's gc.

Checkpoint objects stay in the repository's object store, since checkpoint refs
also keep a removed workspace's HEAD reachable after its branch is deleted.

### Returning to a checkpoint

Restoring a checkpoint reserves the `restoring` lifecycle state, which excludes
other lifecycle operations and holds new executions until it ends. The restore
first keeps the current state in a checkpoint. It then cleans untracked files
under the current ignore rules, resets the branch and applies the snapshot's
files without staging them. Scoped callers may restore their own workspace's
checkpoints, since the replaced state stays a checkpoint.

## Restores

### Removal records

Every removed space keeps one removal record, of either kind, in one table. Its
`chat` column tells the kinds apart once the space's own row is gone, and one
restore path reopens both.

A workspace's record holds its base, parent, links and the recovery records of
its stopped agents, with its checkpoint. A disposable workspace has none. The
record persists before removal deletes any file, so an interrupted or failed
removal stays restorable. Shoal does not offer the records of spaces that still
exist. A retried removal replaces the record with a new checkpoint, and the
earlier checkpoint stays. When the removal's checkpoint keeps HEAD only, merged
cleanup keeps the workspace instead.

A workspace's record expires with its checkpoint. A chat's record, taken before
its state is deleted, expires with its attached files.

### Reopening a space

A restored space keeps its ID and takes a new number, so its checkpoints,
events, files and proposals are its own again without rewriting. A workspace
converted into a chat leaves the chat its ID. The conversion therefore records
the workspace, with its checkpoints, under an ID of its own, and its restore
opens it under that ID beside the chat.

A restored space goes below its recorded parent while that parent takes it, and
else at the top level with a warning. It restores alone: the spaces removed
with it keep their records.

`shoal restore` reopens a workspace's branch through the existing-branch path.
It creates the branch at the removed HEAD when the branch was deleted, and takes
a branch named after the short commit when it no longer contains that HEAD. It
then applies the snapshot's files without staging them, links what the record
names and saves the agents' recovery records. `shoal resume` restores those as
a stopped space's, since reopening is a step on the way back to the work. Once
the files are back, one transaction deletes the record and what keeps its files
from expiring.

A chat restores in the place it ran. While the workspace whose worktree it ran
in is removed, its restore is refused and names the restore that opens both.

A restore first claims the record, which keeps the record, its checkpoints and
its files from expiring for an hour while the restore runs. A restore excludes
link changes, like other lifecycle operations.

A scoped caller restores a removed space only below its recorded parent, which
must exist within the caller's scope, as it creates children only there. The
daemon decides, and lists a scoped caller only what it may restore.

## Branches

### Merged branches

A branch is merged when the local default branch, or its remote-tracking
branch:

- records HEAD's tree,
- contains HEAD,
- takes HEAD's merge without changing its tree, as after a squash merge, or
- has an equivalent patch for every commit since the fork point, as after a
  rebase merge.

Merged commits count as kept. So do commits whose changes the branch the
workspace builds on has: its base workspace's branch, or else the local branch
it was created from. Those count for completion and idle cleanup, but are
neither merged nor enough to delete the branch.

### Which branches removal deletes

Every removal deletes a branch whose commits a remote-tracking branch or the
default branch all have, since deleting it loses nothing, and keeps any other.
It always keeps the default branch, and every branch while the default branch
cannot be resolved.

### Kept branches

Shoal remembers each branch a removal kept, at the commit removal left it on.
The cleanup sweep deletes a kept branch once a remote or the default branch has
all its commits, without notifying anyone.

A kept branch never expires, and the expiry of the removal's checkpoint
notifies no one of it. Its commits are nowhere else, and checkpoint expiry
prunes unreachable objects, so expiring the branch would lose that work for
good.

The sweep decides only from a default branch fetched in the last ten minutes,
and fetches it when the background sync has not. At most every ten minutes, it
asks every remote which branches it has. Only those answers count, never
remote-tracking refs, which a narrowed fetch mapping or a deleted remote branch
can leave stale.

Shoal forgets a kept branch that moved or went away, is the default branch, or
belongs to a workspace again. One that is checked out, or the start or target
of a rebase or bisect elsewhere, waits. Removal records name the branches kept
before Shoal remembered them.

`shoal cleanup` deletes the kept branches the user picks. It first checks that
each is still at the commit removal left it on and not checked out.

## Automatic cleanup

Automatic cleanup removes only clean, idle worktrees whose commits are all
kept, with no executions, directory users, leases or permits. It checks these
again immediately before deletion, without fetching. User requests and disk
pressure remove at once.

`status` reports the first reason cleanup keeps a space, from the same checks
the sweep runs, so the two cannot disagree. A sweep finds directory users with
one scan of the processes' working directories, read from the kernel, which
every workspace it checks shares. Removal scans again.

### Grace period

Every unattended removal except one for disk space first waits
`[cleanup] grace_minutes`, because removal stops an agent the user may still
want to continue. The first attempt that passes removal's initial checks
persists an end time. Each sweep pass claims that time again while some rule
still removes the workspace. The pass ends the periods no attempt claimed, so
they restart instead of carrying over to a later eligibility.

### Idle cleanup

Idle cleanup removes a workspace once its HEAD, recorded activity and file
metadata stayed unchanged for the idle period. While the timer runs, each sweep
compares HEAD and activity and checks recorded state and directory users. It
inspects Git state and walks the tree only when the timer starts, when it
expires and after the removal hooks. A file's change time only moves forward
and cannot be set, so one walk at expiry finds every change made during the
period.

Idle cleanup counts a coordinator's running agent as use of the spaces below
it that are its work, as [spaces.md](spaces.md) describes. An assigned issue
keeps a workspace from idle cleanup, but not from merged cleanup. Idle cleanup
keeps saved agents and stopped commands.

Idle cleanup keeps its timers in memory, so a daemon restart starts its timers
and grace periods over. Resuming them would need the snapshot from before the
restart to prove that nothing changed.

### Merged cleanup

Merged cleanup decides from the workspace's state instead of its assignment: a
merged branch, and nothing using the workspace for the idle period.
[Merged workspaces](../reference/removal.md#merged-workspaces) lists what keeps
one. A link, a watch or a recorded `done` does not.

Merged cleanup persists idle time, so restarts do not reset it. It walks a
worktree again only once the newest change its last walk saw is older than the
idle period, since change times only move forward.

It decides only from a default branch fetched in the last ten minutes, by the
background sync or by a sync it runs itself, so a failed fetch keeps the
workspace. It drops queued restores and keeps the agents in the removal's
record for a restore.

### Cleaning up by hand

`shoal cleanup` is the one command for what Shoal left behind: the candidates
of both rules, kept branches and shared caches. It offers the candidates
without their idle periods, and also where their rules are disabled. It lists
everything with nothing selected, since deleting a kept branch loses its
commits, and removes what the user picks. Naming a kind acts on that kind
alone. Scoped callers run it as their `admin` permission allows.

## Failures

A failed cleanup keeps the workspace's ownership, work and leases, and reports
why. An invalid record keeps its workspace without blocking cleanup of others.
A failed sweep step does not skip the others. Background loops restart after a
panic, and a failed daemon log write never stops them.

A step's failure in a workspace, including one that keeps the step from reading
the workspace's state, shows in that workspace's status. Other failures, and a
stalled loop, show in Shoal's `status` and the inbox. Each notifies once per
run of failed passes, about its workspace where it has one. The log records a
pass's failures only when they differ from the pass before.

## Lost worktrees

A moved worktree stays unresolved until restored. Any other ownership failure
is recoverable by explicit reclaim, which adopts the worktree's recorded path
again. A human requests it after checking the worktree. Reclaim re-marks a
linked worktree of the recorded repository, at the recorded path and on the
recorded branch, that no other workspace owns, then repairs it normally. It
never runs on its own, since [git.md](git.md) accepts only an ownership marker
as proof. Scoped callers request it only as their `admin` permission allows.

A deleted directory means the user removed the worktree. The cleanup sweep then
forgets the workspace through the shared removal path, releasing its leases and
keeping its branch, unless it has commands Shoal cannot verify stopped.
