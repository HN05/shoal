# Git

This file covers how Shoal uses Git: how it runs Git and reads repositories,
worktrees and their ownership, branch names, the default branch, Git profiles,
diffs, conflicts and local merges. The
[branches and merges reference](../reference/branches.md) says what the
commands do.

## Running Git

Only the `git` module names Git's program, so every Git command Shoal runs
goes through one of its operations. The push gate's commands are among them:
they run without the push rewrites, so they reach the real remote.

Shoal's own Git commands run with `--no-optional-locks`. A background status
check therefore never holds the index lock that the user's or an agent's Git
needs.

A Git command still running after ten minutes is stopped, so a hung volume
fails the request that waits on it. Clones have no limit, since their time
grows with the repository.

The daemon's ref updates disable hooks, credential prompts and SSH askpass,
since nobody can answer a prompt there. They keep the user's SSH transport
config. Interactive Git commands keep Git's usual prompts.

Ancestry and exact-ref checks tell a negative answer from a failed command,
since reading a failure as "no" could cost work. A failed check stops what
depends on it, as [Deleting the worktree](../reference/removal.md#deleting-the-worktree)
says.

### The repository's Git gate

Creation, default-branch sync, setup, repository removal and recovery share
one Git gate per repository. Each changes the repository's worktrees or refs,
and the gate keeps those changes from overlapping.

## Reads in process

Shoal reads a repository's location, HEAD, refs, remotes, config and
registered worktrees in process with `gix`. A Git process for each such fact
would dominate the daemon's background load.

Each read opens the repository afresh, so it catches a moved or replaced
worktree. It reads the user's and the system config as Git does, and none of
the repository variables Shoal's Git commands drop. It runs on the blocking
pool.

The daemon and each execution wrapper ask the Git on `PATH` once for its
system config file. They hand the answer down to the processes they start,
which use it while `PATH` finds the same Git. An agent's commands therefore
start no Git process to learn it.

### What Git reads

Git reads a repository that `gix` would read differently:

- one whose refs are in a reftable,
- a partial clone, whose missing objects Git fetches,
- one that enables an extension `gix` lacks,
- one whose config includes a file on a condition `gix` evaluates otherwise,
  such as one on the branch.

Git also reads for a process whose environment points Git at another
repository.

Work stays with Git where it must match Git exactly, or where a difference
could cost work:

- writes and network access,
- `status`, `ls-files` and `diff`, whose ignore and fsmonitor behaviour must
  match Git's,
- revision syntax beyond HEAD, a full ref name and a full object ID peeled at
  most once, such as a caller's `--base HEAD~2`; only those mean the same to
  both,
- the ancestry, `rev-list`, `cherry` and worktree-list checks behind removal
  and cleanup decisions.

Shoal checks branch names in Rust, by Git's rules.

## Worktrees

Shoal creates and removes worktrees with plain Git, with Git's hooks disabled.

### Ownership

A marker in the worktree's Git admin directory names the workspace and proves
that Shoal owns the worktree. Shoal writes it on creation, adoption and
verified repair.

Git keeps the admin directory across moves and makes a new one when the
worktree is created again. Device renumbering, restores and copies thus keep
ownership, and a replaced worktree loses it. A byte-for-byte copy of the admin
directory therefore counts as the same worktree.

The marker is the only proof of a worktree whose creation finished. An
unmarked admin directory needs an explicit reclaim, and Shoal never adopts a
moved or replaced directory silently. [Repair](../reference/daemon.md#repair)
says what the user does then.

### Removal

Before `git worktree remove`, Shoal marks the worktree's admin directory. No
decision then reads Shoal's own deletions as unsaved work.

A removal stopped partway checks out the `.git` link and the tracked files it
deleted again. When it cannot, it fails the workspace with a `removal stopped`
error. Startup or the next removal restores the files once the admin directory
proves the worktree is the workspace's.

A workspace that was readied or failed in another way since has lost that
error. Shoal then drops the mark, and the deletions count as the workspace's
own changes.

`git branch -D` deletes the branch only when the removal decides so, as
[Branches](../reference/removal.md#branches) says.

## Branch names

A space's name is a free-text label that may repeat. It says what the work is
and identifies nothing. A new space takes the most telling name known at its
creation, in the order [Space names](../reference/workspaces.md#space-names)
gives. Only `shoal rename` changes it, so a name never shifts under the user.

A workspace's directory name derives from its branch. It takes a suffix while
another workspace of any repository has that name, so a collision after
[normalization](../reference/workspaces.md#worktree-directories) touches no
existing work.

`start --branch` takes the name as typed, as `git switch` does. An existing
branch opens. A new branch is created under exactly that name or refused, since
a suffixed branch is not the one the user asked for. One flag both opens and
creates, so the user never picks a spelling for a branch that may or may not
exist. [Branches](../reference/workspaces.md#branches) says how a start opens
a local or remote branch.

Shoal suffixes only the branch names it derives, as
[Suffixes](../reference/workspaces.md#suffixes) says. A suffix never changes
the space's name.

### Branch rename

`shoal branch rename` changes a ready workspace's branch in one durable
operation. The workspace keeps its name, path, identity, resources and links.
An interrupted rename blocks reuse of the workspace until an explicit repair,
or until cleanup verifies that its worktree is deleted.
[Rename a branch](../reference/workspaces.md#rename-a-branch) says what it
refuses.

A branch switched outside Shoal, such as with `git switch` in the worktree,
shows in the daemon's sweeps as a `branch_changed` event. It changes no
recorded ownership.

## The default branch

Shoal finds the default branch from the remote's HEAD, or from the checkout's
current branch without remotes, as
[Base branch](../reference/workspaces.md#base-branch) says. It never guesses
`main`.

A new branch starts from the local default branch as the last sync left it.
Creation thus waits on the remote only to learn a remote HEAD it has not
cached. A `--base` that names a configured remote's branch fetches it first,
and a failed fetch refuses the creation.

### Sync

The daemon fetches the default branch's remotes under a fetch gate of its own,
which moving or removing the repository also takes. Only then does it
fast-forward the local default branch, under the repository's Git gate.
Creation therefore never waits on a sync's fetch.
[Default branch sync](../reference/repo.md#default-branch-sync) says when a
sync runs and what it leaves unchanged.

The sync advances a clean registered checkout itself, since Git refuses
`git fetch` into the checked-out branch. It never pushes or moves a
workspace's branch. A workspace takes the default branch through plain Git,
`shoal refresh`, or the fast-forward an admitted agent's start makes.

Commands that only read the default branch never sync. None of them waits on a
fetch or handles a failed sync. A failing sync needs the user only once it has
failed for the stretch of awake time the reference names, so a brief outage
or a wake before the network does not reach them.

A fetch of a whole remote also deletes the tracking refs of branches the
remote no longer has. Cleanup counts commits on a tracking ref as pushed, so a
stale ref would hide commits that exist only locally.

A refresh fetches a branch into a private temporary ref, never the shared
`FETCH_HEAD`, so concurrent fetches never read each other's result.

## Git profiles

Shoal applies a [Git profile](../reference/repo.md#git-profiles) through Git's
per-worktree config. One workspace's profile thus leaves the other worktrees'
settings alone. Shoal writes it before setup, so setup's Git commands use it.

A profile changes no repository layout or extension. Shoal enables the shared
`worktreeConfig` extension only where the layout supports it.

## Diffs

`shoal diff` compares with the fork point of the workspace's recorded base, so
commits the base gains later never show as work. It runs native `git diff`, so
the user's diff settings apply. [Diff](../reference/branches.md#diff) says how
it finds the fork point.

## Conflicts

`shoal conflicts` asks `git merge-tree` whether committed HEAD merges cleanly
into a target. It writes only objects, so it changes no worktree, index or ref.

### Files changed on both sides

Git merges two branches that pick the same migration or protocol version
cleanly, since their edits agree. A repository therefore lists the files that
hold such numbers as `[conflicts] sensitive` paths and globs, and `conflicts`
reports each one that both sides changed, as
[Files changed on both sides](../reference/branches.md#files-changed-on-both-sides)
says.

Ordinary changes rarely break the default branch this way, so Shoal has no
merge queue.

## Merges

Git already merges and rebases a workspace onto the default branch, so Shoal
has no command for that direction.

### One merge command

`shoal merge` is one command for a space's work, whichever way it merges. The
daemon decides from the space's links. It merges the one open PR the space
links as assigned or opened on the forge, and otherwise a workspace's branch
locally.

It reads each linked PR's state from the forge, since no poll follows an
opened PR. It refuses several open PRs rather than guess one.

No flag merges locally past an open PR, since that would bypass the PR's
review. Closing the PR is that decision.

`merge` merges a space's work either way, so it is top-level rather than under
`pr`. It reads a PR it is given as the cross-kind verbs read items.

### Method

`[merge] method` sets the method of both kinds of merge, so a repository
merges the same way with or without PRs.
[Method](../reference/issues.md#method) says which method a PR merge takes
without it. A squash merge's message goes to the forge only where Shoal knows
that the method squashes.

### Local merges

A local merge is the guarded substitute for a PR. It merges by any of a PR
merge's methods. It leaves the workspace's branch as it was, as a forge's
squash or rebase merge leaves a PR's.

A squash commit is made with `git merge-tree` and `git commit-tree`, so the
merge touches no checkout. A rebase replays the branch's commits in a worktree
of its own. The default branch then fast-forwards to the commit either makes.

A merge holds the repository's Git gate for its tracked execution. The daemon
of the workspace's machine checks the merge and refreshes the default branch.
It also decides, from its own settings, whether and where the merge pushes and
whether it asks first. An operation worker there merges. A merge or unmerge of
another machine's workspace therefore runs on that machine.

### Confirmation and approval

A push publishes the merge where a local reset cannot reach it, so a merge
that pushes asks the user first. It asks after the daemon's checks and before
anything changes. The question names the direction, since `git merge main` in
a workspace merges the other way.

An agent's approval request names the method and a squash's message, so
approving a merge commit lets no squash through. The approval is the merge's
confirmation, as [Confirmation](../reference/branches.md#confirmation) says.

### Unmerge

A local merge can be undone, so `unmerge` is a command of its own rather than
a flag that flips `merge`.

The daemon records each merge with its method when the merge's execution ends.
It reads the merge from what the default branch holds, not from what the
worker reports: a squash as the one commit whose tree merges the two, a rebase
as the chain that replays the branch's commits. It drops the record of a merge
the default branch no longer holds or its upstream has.

Records follow the branch, not the workspace's ID, so the merge of a restored
workspace stays undoable. `unmerge` runs in the workspace's tracked execution,
so a workspace that cleanup removed is restored first. When cleanup deleted
the branch, `unmerge` creates it again, so its commits stay on a branch.

`unmerge` runs under the same gate, checks and tracked execution as a merge.
When commits came after the merge, it replays them in a worktree of its own
and then moves the default branch to the result in one step. It rewrites the
records of the merges it replays.

Only a revert undoes a pushed merge, so `unmerge` refuses one. It makes no
revert commit; [When unmerge refuses](../reference/branches.md#when-unmerge-refuses)
gives the revert for the user to push.

### Cancellation

A cancelled merge or unmerge stops its process group first. The wrapper then
rolls back an incomplete merge, keeps a completed one and reports a recovery it
cannot make safely. An interrupted replay's worktree is removed. The default
branch has not moved then, since the replay moves it in one final step.
