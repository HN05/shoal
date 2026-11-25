# Completion and cleanup

This file covers holds, absorbing, completion, removal, checkpoints, restores,
kept branches and automatic cleanup.

Idle cleanup requires unchanged HEAD, activity and file metadata; while the
timer runs, sweeps compare HEAD and activity and check recorded state and
directory users, and inspect Git state and walk the tree only when the timer
starts, when it expires and after removal hooks, since change times only move
forward and cannot be set. For the same reason, merged cleanup walks a worktree
again only once the newest change its last walk saw is older than the idle
period. A sweep lists directory users with one scan of the processes' working
directories, read from the kernel, shared by every workspace it checks; removal
scans again. Status reports the first reason cleanup keeps a space from the
checks the sweep runs, so it cannot disagree with the sweep. A HEAD changed by
removal hooks keeps its branch when only that branch has the new commits.

External sessions acquire caller-named workspace holds independently of assignment
completion. Holds are idempotent by name and persist across restarts. They block
automatic removal while the worktree exists; releasing the last hold restores
normal cleanup eligibility. Explicit
removal lists holders and releases holds with the workspace record; deleted-worktree
cleanup still forgets missing worktrees. Release remains available through failed or
active lifecycle operations and hooks. Each hold records who placed it: the user
or an app hosting an external session, or an agent. Scoped callers hold and
release the workspaces below their own directly; holding their own is an approval
request, which approval carries out, so a coordinator keeps what its children
need. Agents release only the holds agents placed; the user's and host apps'
stay theirs. A hold never keeps a parent or survives its removal. Removal of a
workspace that is not disposable is reversible through checkpoints, so an agent otherwise has no way to keep a
workspace: one that needs the user asks, and a thread it opened that waits, like a hold, keeps the
workspace from every unattended removal, completion and PR cleanup included. So
does an agent launch waiting in the agent queue to start in the workspace.

Decision: `shoal absorb` is how a coordinator takes a finished space's work
back into its own instead of merging it with Git and removing the space. It
needs no permission of its own, since it ends in removing a space below the
caller, which scoped callers may. Only done work is absorbed unless forced,
because absorbing ends the space. Code goes only where a branch of its
repository can take it, through the local merge `shoal merge` uses, generalized
to a target branch in its worktree, which need not be clean: every method makes
its commit without touching a checkout and the target fast-forwards, Git
keeping that worktree's other changes. Uncommitted files arrive as uncommitted
changes for the taking agent to review, never as a commit. Everything that is
checked is checked before anything moves; what binds to the absorbed branch,
PR proposals and push requests, is withdrawn rather than rebound, and the
removal path takes the space last, so its checkpoint and branch keep what was
left behind. Its children move up one level through one operation, which a
general reparenting command can reuse.

Workspace completion uses `[cleanup.done] enabled` (default true), resolved
through the normal configuration layers, and an explicit cleanup override. Only
an explicit done signal completes an assignment; issue closure and merges do
not, and merged cleanup removes unused merged workspaces from their state
instead. Agent instructions make `done` the last step of every assignment.
Completion is persisted separately from lifecycle readiness, binds to HEAD, and
notifies the user; it does not assert that work was merged. A `done` that would
change nothing and an `undone` without a completion fail, so a misplaced signal
is reported instead of passing silently. An assignment that ends with a finding
rather than something to merge hands back files with its completion. They are
the workspace's results among its attached files, which, like every attached
file, stay for `cleanup.restore.retention_days` after cleanup removes it, so the
user or the parent's agent can read them after the worktree is gone. Files an
agent sends the user with an alert or a post stay the same way, since the user
may read the alert after the workspace is gone, and outlive the alert or post as
a message's files outlive the message: clearing or withdrawing an alert and
resolving a thread leave them among the space's files, which the user already
sees and Quay may show, until `shoal files rm` or the retention removes them.
The alert or post names them by their records rather than by path, so a file
`shoal files rm` deleted drops out and a moved repository directory moves them,
and a coordinating agent that receives the alert or post instead receives their
paths as the message's attached files. A chat's `done` records its completion on
the chat as a workspace's does, with cleanup by the same setting and override,
but no branch another could integrate and no parent to tell, so it notifies the
user and its agent keeps running until completion cleanup removes the chat after
the grace period, with the done state shown until `--undo` withdraws it.

Own-workspace withdrawal removes a recorded completion and preserves
associations and merge requirements, but never defers cleanup: only holds,
threads it opened that wait, a pending PR proposal or push request and queued
agent launches keep a workspace. The daemon runs `hooks.post_done` after
persisting completion and before cleanup, using normal config precedence and the
space's hook identity plus the keep/cleanup choice, where its agent works: a
workspace's worktree, the worktree or checkout a chat runs in, or a chat without
a repository's `root_dir` with the global config's hook. Only a workspace's
completion reads a branch, verifies a worktree or takes `--integrated`. It is
bounded, excludes lifecycle and permit changes, and reports failure through an
alert without undoing completion or blocking cleanup. Explicit signals rerun it;
events are not queued or replayed on restart. Completion/PR serialization is per
workspace: cleanup sweeps hold a workspace's gate only while they handle it,
including its forge lookups and removal, so commands for one workspace never
wait for another's. It covers the hook, which must not call gated Shoal
mutations for its workspace. Completion without a hook takes no resource guard,
so an agent-exit hook may signal it; configured completion hooks cannot nest
inside another guarded hook. A completion without cleanup suppresses idle and PR
cleanup until another completion requests cleanup or the user removes it; merged
cleanup still applies. Completion cleanup without a PR registration requires
clean files and every commit kept, or a completion marked integrated: another
branch has HEAD's work under commits whose patches were edited, which no check
can match, so only the agent that took them can say so. The mark binds to HEAD
like the completion and keeps the branch, since no remote or default branch has
its commits. An existing PR registration keeps its merge checks. The daemon
stops tracked executions through shared removal and rechecks files and HEAD,
including after hooks. Tracked agent exits wake the cleanup sweep after their
exit hook, without waiting for the polling interval. Changed HEAD retains the
workspace until a new completion signal; failed cleanup retains ownership and
reports why.

Manual and automatic cleanup share one path: establish ownership, stop owned
executions, saving an existing worktree's agent sessions but no commands, run
removal hooks, take down owned containers and remove owned simulators, take a
checkpoint unless the workspace is disposable, remove the worktree, and release
leases with the record. Failures retain what is needed to retry. Git forgets a
worktree once it starts deleting its files, so removal deletes what Git could
not, and a later removal deletes what such a failure, or a checkout that failed,
left of a disposable workspace without Git, proven its own by the `.git` link
naming a deleted admin directory of its repository, or, once a removal stopped
after Git deleted that link and a `git worktree prune` deleted the admin
directory, by the directory's identity, which a removal records before Git
begins deleting it. Git no longer has its HEAD, index or ignore rules, so the
files left of a workspace that is not disposable are read against its branch's
commit, with the commit's missing files taken as unchanged; those that differ
are unsaved work, kept laid over that commit in the checkpoint. Workspaces are
cheap to remove because removal is reversible, except that of a disposable
workspace, which holds nothing worth keeping. A checkpoint is a commit of HEAD
plus the unsaved files Git does not ignore, built with a temporary index and
kept under `refs/shoal/checkpoints/<commit>`, with a record in the store. A ref
is named by its commit, so it belongs to whichever records keep that commit and
moving checkpoints to another workspace changes only records; a ref no record
keeps is deleted. The ref is written before its record, so a record never names
an unreachable commit, and the pair excludes expiry, never under the
repository's Git gate, which removal holds. An agent's checkpoint is taken only
once the processes it left are proven stopped, and a restore excludes link
changes like other lifecycle operations. Checkpoints are taken on request, when
an agent exits or is stopped (while the workspace is ready or stopping), before
a restore to a checkpoint, and by removal, and when a copy starts; all but
requested ones extend the newest checkpoint instead when they would keep the
same branch, HEAD and files. A checkpoint keeps HEAD only when the unsaved files
include an untracked nested repository or a changed submodule, whose files it
cannot hold. `[cleanup.restore] archive_max_mb` caps only the checkpoint of an
agent's exit or stop, whose files stay in the worktree, so leaving them out
loses nothing: above it, that checkpoint keeps HEAD only, unless the files are a
copy's starting ones, which bounds the time each agent stop spends hashing large
untracked files and the versions a large changing file would pile up. Any other
checkpoint is asked for or may hold their last copy, so it keeps them whatever
their size. A snapshot fails when HEAD or the branch moves while it is taken, so
its record's HEAD always matches its files. Checkpoints expire after
`retention_days`, a copy's starting checkpoint only once the copy is gone, and
so do those no workspace or removal record owns. An expiry pass that deletes a
checkpoint ref then runs `git prune` with the repository's `gc.pruneExpire`
(Git's two weeks when unset, nothing when `never`), never a shorter window:
large loose blobs alone never start Git's automatic gc, and the user's window
keeps their recovery time and the objects a `git add` or a snapshot under way
just wrote. Shoal does not repack, which would rewrite the user's packs, so
objects already packed stay until the user's gc. Checkpoint objects stay in the
repository's object store, since checkpoint refs also keep a removed workspace's
HEAD reachable after its branch is deleted. Restoring a checkpoint reserves the
`restoring` lifecycle state, which excludes other lifecycle operations and holds
new executions until it ends, refuses while another execution is recorded or the
checkpoint's HEAD or files collide with ignored ones, keeps the current state in
a checkpoint and stops when that one keeps HEAD only, then cleans untracked
files under the current ignore rules, resets the branch and applies the
snapshot's files without staging them. Scoped callers may do this in their own
workspace, since the replaced state stays a checkpoint. Every removed space
keeps one removal record, of either kind, in one table whose `chat` column tells
the kinds apart once the space's own row is gone, and one restore path reopens
both. The record of a workspace that is not disposable (base, parent, links and
stopped agents' recovery records, with its checkpoint) persists before any file
is deleted, so an interrupted or failed removal stays restorable; records of
spaces that still exist are not offered, and a retried removal replaces the
record with a new checkpoint while the earlier one stays. When the removal's
checkpoint keeps HEAD only, merged cleanup retains the workspace. A workspace's
record expires with its checkpoint; a chat's, taken before its state is deleted,
with its attached files. A restored space keeps its ID and takes a new number,
so its checkpoints, events, files and proposals are its own again without
rewriting. A workspace converted into a chat leaves the chat its ID, so the
conversion records it, with its checkpoints, under an ID of its own, which its
restore opens it under beside the chat. It goes below its recorded parent while
that takes it, else at the top level with a warning, and alone: the spaces
removed with it keep their records. `shoal restore` reopens the branch through
the existing-branch path, creating it at the removed HEAD when deleted and
refusing one that no longer contains it, then applies the snapshot's files
without staging them, links what the record names and saves its agents' recovery
records, which the same `shoal resume` then restores as a stopped space's, since
reopening is a step on the way back to the work; once the files are back, one
transaction deletes the record and what keeps its files from expiring. A chat
restores in the place it ran, refused while the workspace whose worktree it ran
in is removed, naming the restore that opens both. A restore first claims the
record, keeping it and its checkpoints and files from expiring for an hour while
it runs. A scoped caller restores a removed space only below its recorded
parent, which must exist within its scope, as it creates children only there;
the daemon decides, and lists a scoped caller only what it may restore.

A branch is merged when the local default branch or its remote-tracking branch
records HEAD's tree, contains HEAD, takes HEAD's merge without changing its tree
(squash merges), or has an equivalent patch for every commit since the fork
point (rebase merges); merged commits count as retained. Commits whose changes
the branch the workspace builds on has, its base workspace's branch or else the
local branch it was created from, are kept there for completion and idle
cleanup, but are neither merged nor enough to delete the branch. Every removal
deletes a branch whose commits a remote-tracking branch or the default branch
all have, since deleting it loses nothing, and keeps any other: users should not
have to decide about branches. The default branch, and every branch while the
default branch cannot be resolved, is retained. Manual removal asks only before
deleting uncommitted or untracked files or commits on a detached HEAD, which no
branch keeps. Shoal remembers each branch a removal kept, at the commit removal
left it on, and the cleanup sweep deletes it once a remote or the default branch
has all its commits, without notifying anyone. A kept branch never expires, and
the expiry of the removal's checkpoint notifies no one of it: its commits are
nowhere else, and the checkpoint expiry prunes unreachable objects, so expiring
it would lose that work for good. It needs a default branch fetched in the last
ten minutes, fetching it when the background sync has not, and at most every ten
minutes it asks every remote which branches it has; only those answers count,
never remote-tracking refs, which a narrowed fetch mapping or a deleted remote
branch can leave stale. A kept branch that moved or went away, is the default
branch or belongs to a workspace again is forgotten; one checked out, or the
start or target of a rebase or bisect elsewhere, waits. Removal records name the
branches kept before Shoal remembered them. `shoal cleanup` deletes the kept
branches the user picks, several at once, after checking each is still at the
commit removal left it on and not checked out. Automatic cleanup removes only
clean, idle worktrees whose commits are all kept, with no executions, directory
users, leases, or permits, rechecked immediately before deletion, without
fetching. Every unattended removal except for disk space first waits `[cleanup]
grace_minutes`, because removal stops the agent the user may still want to
continue: the first attempt that passes removal's initial checks persists an end
time, each sweep pass claims it again while some rule still removes the
workspace, and the pass ends periods no attempt claimed, so they restart instead
of carrying over to a later eligibility. Idle cleanup also counts a coordinator's running agent as use of the spaces
below it that are its work, as [spaces.md](spaces.md) describes. An assigned
issue keeps a workspace from idle cleanup, but not from merged cleanup. Idle cleanup keeps its timers in
memory, so a restart starts its grace period over too: resuming would need the
pre-restart snapshot to prove nothing changed. User requests and disk pressure
remove at once. Merged cleanup (`[cleanup.merged]`, off by default, 240 idle
minutes) decides from the workspace's state instead of its assignment: a merged
branch, unsaved files only as leftovers the removal's checkpoint keeps (the
branch has commits of its own), no Git operation under way, no waiting thread,
nothing running, no leases, permits, directory users, stacked workspaces, holds
or spaces below it, and no recorded activity, file change or per-worktree Git
change for the idle period. Idle time is persisted, so restarts do not reset it;
it decides only from a default branch fetched in the last ten minutes, by the
background sync or by a sync it runs itself, so a failed fetch keeps the
workspace. A link, a watch or a recorded `done` does not keep the workspace.
Idle cleanup retains saved agents and stopped commands; merged cleanup drops
queued restores and keeps the agents in removal's record for restore. `shoal
cleanup` is the one command for what Shoal left behind: the candidates of both
rules, without their idle periods and including where they are disabled, kept
branches, and shared caches. It lists them with nothing selected, since deleting
a kept branch loses its commits, and removes what the user picks; naming a kind
acts on that kind alone. It is unavailable to scoped callers. `repo rm` deletes
the checkout and every workspace through that path, refuses external worktrees
and dangerous paths, persists progress, and blocks new workspaces until an
interrupted removal is retried. PR cleanup is separately enabled by default;
disabling it pauses PR completion without affecting links, which serve more than
cleanup.

When completion requests cleanup, confirmed merges stop tracked agents through
shared removal, rechecking clean files and HEAD after stopping. Failures retain
work and leases; registered workspaces are excluded from idle cleanup until
cleared or removed. An invalid record retains its workspace without blocking
cleanup of others. A failed sweep step does not skip the others. Background
loops restart after a panic, and a failed daemon log write never stops them. A
step's failure in a workspace, including one that keeps the step from reading
the workspace's state, shows in that workspace's status, and other failures or a
stalled loop in Shoal's `status` and the inbox; each notifies once per run of
failed passes, about its workspace where it has one, and the log has a pass's
failures only when they differ from the pass before.

Moved worktrees stay unresolved until restored. Any other ownership failure is
recoverable by explicit reclaim, which a human requests after checking the
worktree by adopting its recorded path again: it re-marks a linked worktree of
the recorded repository at the recorded path on the recorded branch that no
other workspace owns, then repairs normally. It is never automatic, and scoped
callers request it only as their `admin` permission allows. A deleted directory
means the user removed the worktree: the cleanup sweep forgets it through the
shared removal path, releasing its leases and retaining its branch, unless it
has commands Shoal cannot verify stopped.
