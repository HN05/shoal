# Git

This file covers how Shoal uses Git: worktrees, the default branch, branch
names, ownership markers, profiles, diffs, conflicts and local merges.

Plain Git creates and removes worktrees through the `git` module with hooks
disabled: `git worktree add` on a new or existing branch, at a commit, or
without checking files out for a copy, and `git worktree remove`, then `git
branch -D` when the removal decision deletes the branch. Before `git worktree
remove`, Shoal marks the worktree's admin directory, so that no decision reads
Shoal's own deletions as unsaved work. A removal stopped partway checks the
`.git` link and tracked files it deleted out again. If it cannot, it fails the
workspace with a `removal stopped` error, and startup or the next removal
restores them once the admin directory proves the worktree is the workspace's. A
workspace readied or failed otherwise since has lost that error: its marker is
dropped and its deletions count as its own. Typed removal outcomes preserve
unfamiliar strings; only confirmed branch deletion sets the response deletion
flag.

A new branch starts from the repository default branch: `origin/HEAD`, the sole
remote's HEAD, or the checkout's current branch without remotes, never a guessed
`main`. It starts from the local default branch as the background sync left it,
so creating a workspace waits on the remote only to discover a remote HEAD not
yet cached; the sync fast-forwards it from
its upstream, preserving an ahead branch and leaving it on divergence, dirty or
managed checkouts, and failed fetches. `--base REF` starts from any locally
resolvable commit ref, except a configured remote's branch, which is fetched first
and must succeed.
Creation, default-branch sync, setup, repository removal, and recovery share
a per-repository Git gate. Shoal's own Git commands skip optional locks, so
background status checks never hold a worktree's index lock, and except clones
are stopped after ten minutes, so a hung volume fails the request that waits on it. Daemon ref updates
disable hooks, Git credential prompts, and SSH askpass while preserving the user's
SSH transport configuration;
interactive Git commands retain normal prompting. Ancestry and exact-ref checks
distinguish negative answers from command failures; failed checks stop branch
selection, refresh, and removal with the Git diagnostic.
Decision: Shoal reads a repository's location, HEAD, refs, remotes and config
in process with `gix` rather than starting a Git process for each fact, since
those processes dominated the daemon's background load. Each read opens the
repository afresh, so a moved or replaced worktree is caught as before, sees
the user's and system config as Git does and none of the repository variables
Shoal's Git commands drop, and runs on the blocking pool. A repository `gix`
would not read as Git does, such as one whose refs are in a reftable, a partial
clone, whose missing objects Git fetches, one that enables an extension
`gix` lacks, or one whose config includes a file on a condition `gix` evaluates
otherwise, such as one on the branch, and a process whose environment points Git
at another repository read through Git. Writes, network access, `status`,
`ls-files` and `diff`, whose ignore and fsmonitor behaviour must match Git
exactly, revision syntax Git interprets, such as a caller's `--base HEAD~2`,
beyond HEAD, a full ref name or a full object ID peeled at most once, which mean
the same to both, and the ancestry, `rev-list` and
`cherry` checks behind removal and cleanup decisions stay with Git, where a
difference could cost work. Branch names are checked in Rust by Git's rules.

New-branch creation accepts literal Git branch names. A space's name is a
free-text label that may repeat, so it says what the work is rather than
identifying anything: a new space takes `--name`, else what it was started for,
a CI job's name or the linked issue's or PR's title, else for a workspace the
branch as given, so the label a list or tab shows is the most telling one known
at creation; only `shoal rename` changes it, so a label never shifts under the
user. The workspace's portable directory name derives from the branch, suffixed
while another workspace of any repository has that directory name, so a
normalization collision touches no existing work. Branch conflicts of a name
Shoal derives or splits get numeric suffixes on the blocking component only,
never changing the workspace's name. `start --branch` takes the name as typed,
as `git switch` does: an existing branch opens, in the workspace that has it
when one does, through `--in`'s path, and a new one is created under exactly
that name or refused, since a suffixed branch is not the one the user asked for;
one flag both opens and creates, so the user never chooses between spellings for
a branch that may or may not exist. Names Git reads as a revision instead of a
branch, in either supported object format, get a leaf suffix on creation and an
error on existing-branch selection. Ownership of a worktree's Git admin
directory is proven by a marker naming the workspace, written there on creation,
adoption and verified repair: Git keeps it across moves and drops it when the
worktree is re-created, so device renumbering, restores and copies keep
ownership while replacement does not. A byte-for-byte copy of the admin
directory is therefore accepted as the same worktree. The marker is the only
proof of a worktree whose creation finished: an unmarked admin directory
requires explicit reclaim. Moved or replaced directories are never adopted
silently. Existing-branch selection creates a worktree without suffixing; remote
heads are discovered live and become local tracking branches. Local selection
preserves commits; remote selection fast-forwards matching tracking branches.
Ready owned workspaces reopen without setup, hooks, or refresh; other checkouts
block creation. Existing worktrees use an explicit base ref, otherwise the local
default, as their diff base, or the opening commit if unavailable or on that
same branch.

`shoal branch rename` changes a ready workspace's checked-out branch, keeping its
name, in one durable operation; its path, identity, resources, and associations stay
with the workspace. The default branch, a branch checked out elsewhere, or a
name collision is refused. No execution other than the scoped caller may be
recorded, and interrupted intent blocks reuse until explicit repair or verified
deleted-worktree cleanup.

Named Git profiles live in global config; repository or global `git_profile`
selects one for newly created worktrees, overridden by `start --git-profile`.
Apply it before setup using Git's
per-worktree config, preserving other worktrees' settings. Enable the shared
`worktreeConfig` extension only when the existing repository layout supports it;
profiles cannot change layout or extensions. Reopening keeps existing settings
and rejects an explicit profile flag.

`diff` compares against the recorded base's fork point (merge-base fallback, fixed
commits stay fixed) with native Git settings, so advancing the base is never shown
as work. `conflicts` asks `git merge-tree` whether committed HEAD merges cleanly
into a target, by default the base workspace's branch, otherwise the local default
branch, which the daemon keeps current; it writes only objects. Git merges two
branches that pick the same migration or protocol version cleanly, since their
edits agree, so a repository lists the files that hold such numbers as
`[conflicts] sensitive` paths and globs, without value patterns, and `conflicts`
also lists each one that HEAD and the target both changed since their merge base
as changed on both sides, left out like conflicts once the target has HEAD's
changes. Ordinary changes rarely break the default branch this way, so Shoal has
no merge queue. Every minute, after a
PR of the repository merges, and on `sync`, the daemon fetches the default branch's remotes under a fetch gate of its own,
which moving or removing the repository also takes, and only then fast-forwards
the local default branch to what it fetched under the repository's Git gate, so
creation never waits on a sync's fetch, advancing a clean registered checkout where
`git fetch` into the checked-out branch is refused; it never pushes or moves workspace branches, so updating
a workspace from it stays plain Git, an explicit `shoal refresh`, or the
fast-forward an admitted agent's start makes. Commands that only read the default branch
never sync, so none waits on a fetch or handles a failed sync; workspace
status shows a failing sync, and one failing for 30 minutes of awake time, which
a brief outage or a wake before the network does not reach, needs the user and
joins the inbox with one alert. Git already merges and rebases a workspace onto the
default branch, so Shoal has no command for that direction. `merge` is one
command for a space's work, whichever way it merges: the daemon decides from the
space's links, merging the one open PR it links as assigned or opened on the
forge, and otherwise a workspace's branch locally. It reads each linked PR's
state from the forge, since a poll follows no opened PR, and refuses several
rather than guessing one. There is no flag to merge locally past an open PR,
which would bypass its review: closing the PR is that decision. Because it
merges a space's work either way, it is top-level rather than under `pr`, and
reads a PR it is given as the cross-kind verbs read items. `[merge] method`
sets the method of both kinds of merge, so a repository merges the same way
with or without PRs. A PR merge without a method takes the repository's
default, which each forge's merging implementation reads or leaves to the
forge; GitHub has none, so it takes the first method the repository allows. A
squash merge's message goes to the forge only where the method is known to
squash. The local merge, the guarded
substitute for a pull request, merges the workspace branch into the default
branch by any of a PR merge's methods: a fast-forward or merge commit, one
squash commit by whoever merges, made with `merge-tree` and `commit-tree`
without touching a checkout, or the branch's commits rebased in a worktree of
its own, keeping their authors; the default branch then fast-forwards to the
commit either makes, and the workspace's branch stays as it was, as a forge's
squash or rebase merge leaves a PR's. Without a message, a squash of one commit
keeps its message and of several takes Git's own. It first refreshes a configured upstream while preserving an ahead branch and
refusing divergence or fetch failure. It pushes the default branch to that upstream
only on request, checking the upstream before merging and keeping the merge when the
push fails. The default branch cannot be
held by a managed workspace, and any checkout of it must be clean. It aborts a merge
that does not apply cleanly, leaving conflicts to a Git merge of the default branch into
the workspace, or for a rebase merge, whose replay a merge does not resolve, a
rebase of the workspace onto it. Merging holds the repository Git gate for the tracked execution; the
daemon of the workspace's machine validates, resolves from its own settings
whether and where the merge pushes and whether it asks first, and refreshes,
and an operation worker there merges, so a merge or unmerge of another
machine's workspace runs on that machine.
Pushing publishes the merge where a local reset cannot reach it, so a merge
that pushes asks the user first, after the daemon's checks and before it
changes anything; it names the direction, since `git merge main` in a workspace
merges the other way. A
local merge asks only when the last check found sensitive files changed on both
sides, which the same question names. An agent's approval request carries the
same details, the method and a squash's message among them, so an approved
merge commit lets no squash through, and is its confirmation; the agent still answers the question about
sensitive files with `--yes`, so the approver decides on them.
A repository that merges only through PRs turns local merges off with `[merge]
enabled = false`, which refuses agents' requests for them too.
Because a local merge can be undone, `unmerge` is a command of its own rather than
a flag that flips `merge`. The daemon records each merge with its method when its
execution ends, from what the default branch holds rather than what the worker
reports: a squash as the one commit whose tree is the merge of the two, a rebase
as the chain that replays the branch's commits; and drops
records the default branch no longer holds or its upstream has. `unmerge` undoes
the branch's latest recorded merge under the same gate, checks and tracked
execution: it moves the default branch back when nothing came after the merge, and
otherwise replays the later history onto the commit before it with
`--rebase-merges` in a worktree of its own, then moves the default branch to the
result in one step, rewriting the records of the merges it replays. It refuses a
pushed merge, since only a revert undoes that, and a replay that would bring the
merged commits back through a later merge, drop a later merge commit's own
changes, or does not apply cleanly, and an undo that would overwrite ignored files
in the default checkout; Shoal makes
no revert commits. It recreates a branch cleanup deleted, so the commits stay on one.
Since it runs in the workspace's tracked execution, a workspace cleanup removed is
restored with `shoal restore` first; records follow the branch, not the
workspace's ID, so the restored workspace's merge stays undoable.
After cancellation, the wrapper rolls back incomplete merges after stopping the
process group, preserving completed merges and reporting unsafe recovery failures;
an interrupted replay's worktree is removed, and the default branch has not moved.
Refresh fetches use private temporary refs; merges run in the tracked wrapper.
A fetch of a whole remote also deletes the tracking refs of branches the remote no
longer has, since cleanup counts commits on a stale ref as pushed.
