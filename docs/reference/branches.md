# Branches and merges

This file says what `shoal merge`, `unmerge`, `refresh`, `diff`, `conflicts` and
the `branch` commands do. [Git](../design/git.md) gives the reasons.

## Merge into the default branch

`shoal merge [pr] [NUMBER|URL] [-s SPACE]` merges a space's work, or the PR
you name. It reads the PR as `shoal view` reads an item:

- a number, which `--repo` places in another repository,
- a PR URL of any repository.

Each may stand alone or follow `pr`. `pr` alone names the space's assigned PR.
Issues, CI runs and reviews do not merge.

Without an item, `shoal merge` decides from the space's links:

- A space with one open PR linked as assigned or opened merges it on the forge
  as an [item action](issues.md#issue-and-pr-actions).
- A space with several such PRs is refused, and the refusal names them.
- A chat without one is refused.
- A workspace without one merges its recorded branch into the default branch
  locally, as [local merges](#local-merges) says.

To merge a workspace locally while its PR is open, close the PR first.

`-X` and `--message` apply to both kinds of merge. `--delete-branch`, `--auto`
and `--cancel-auto` apply to a PR merge, and `--push` and `--no-push` to a
local merge. A flag of the other kind is refused.

A scoped caller merges the work of a space below its own with `-s`. Its own
`merge` and `push` permissions govern that merge.

A workspace that another machine of the hub runs merges and unmerges on that
machine. The merge goes into that machine's checkout of the default branch,
under the `[merge]` settings that machine reads for the workspace.

### Local merges

A local merge serves a repository without PRs. It has these requirements:

- When the default branch has an upstream, Shoal fetches and fast-forwards it
  first. A default branch ahead of its upstream stays so. A diverged branch or
  a failed fetch refuses the merge.
- No managed workspace has the default branch checked out, and any other
  checkout of it is clean.
- The workspace is clean and on its recorded branch.

The merge takes its method from `-X`, else from
[`[merge] method`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#merges):

- `merge`, the default, fast-forwards the default branch, or makes a merge
  commit of the two branches as `git merge` would. It touches no checkout.
- `squash` makes one commit on the default branch, authored by whoever merges.
  Its files are the merge of the default branch and the branch, as
  `git merge --squash` and a commit make them. `--message` gives its message.
  Without it, a branch of one commit keeps that commit's message, and a branch
  of several gets Git's own squash message.
- `rebase` copies the branch's commits that the default branch lacks onto it,
  as `git rebase` copies them, and keeps their authors. It works in a worktree
  of Shoal's own and leaves merge commits out.

Only `squash` takes `--message`. Every method moves the default branch to the
commit it made and fast-forwards the default branch's checkout. A squash or
rebase leaves the workspace's branch as it was, as a forge's squash or rebase
merge leaves a PR's branch.

A merge that does not apply cleanly changes nothing. Merge the default branch
into the workspace with Git (`git merge main`), or for a rebase merge rebase
the workspace onto it (`git rebase main`). Resolve the conflicts there and run
`shoal merge` again.

`rm` and automatic cleanup count merged commits as pushed.

### Approval of an agent's merge

A merge that its [review gate](reviews.md#review-gates) refuses goes no
further.

An agent's scoped `shoal merge` needs your approval unless both its
[`merge` permission](modes.md#mode-permissions) and its
[`push` permission](approvals.md#approval-for-agent-pushes) are `"direct"`.
The approval covers the branch at its current commit, the method, a squash's
message and where the merge pushes.

The first run records a `merge` request in `shoal approvals` and exits 2 with
`code: "approval_pending"`. The request names:

- the method and a squash's message subject,
- the branch, the default branch it merges into and its commit count,
- the remote branch it pushes to,
- other local commits of the default branch that the push publishes too,
- files [changed on both sides](#files-changed-on-both-sides).

After approval, the agent runs the merge again. It merges the approved commit
and makes every check above.

A new request replaces the old one when any of these changes:

- the branch's commit,
- the method or the message,
- where the merge pushes,
- the other local commits of the default branch that the push would publish,
- the set of files changed on both sides.

A sync of the default branch from its upstream replaces no request. Approving
a request whose branch moved since is refused as stale. A denied commit stays
refused with `"approval_denied"`.

### Push after merging

With [`[merge] push`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#merges),
or `--push` for one run, Shoal pushes the default branch to its upstream after
merging. `--no-push` keeps one merge local. A merge that would push a default
branch without an upstream is refused before it merges. A failed push keeps the
merge.

Where [`[pr] watch_merge_runs`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs)
is on, the workspace follows the default branch's [runs](links.md) for the
commit the push published, as it follows a merged PR's. It reads them on the
forge of the first URL the push went to on a network host.

### Confirmation

A local merge asks you first in two cases:

- it pushes,
- its last check found files changed on both sides, as a
  [PR merge](issues.md#issue-and-pr-actions) does.

It asks after Shoal has checked the merge and before anything changes. The
question names the method and direction, a squash's message subject, the commit
count, where the merge pushes, other local commits of the default branch that
the push publishes too, and the files changed on both sides:

```text
Merge fix-login (3 commits) into main, then push main to origin.
Sensitive files changed on both sides by fix-login and main: src/version.rs.
Are you sure? [y/N]
```

The default answer is no. The merge is refused when one of these changed while
the question was open:

- the branch moved,
- the default branch took commits other than its upstream's,
- the method or the message,
- where the merge pushes,
- the files changed on both sides that a new conflict check found.

`--yes` skips the question. `[merge] confirm = false` skips it for a merge that
pushes. Without a terminal, as in scripts or with `--json`, a merge that would
ask is refused unless something skips the question.

An agent's merge asks about its push through its approval request instead. It
still asks about files changed on both sides, so the agent passes `--yes` and
the approver decides on them.

### Turn local merges off

`[merge] enabled = false` turns local merges off for a repository that merges
only through PRs. It refuses a local merge and an agent's request for one, and
points to `shoal pr open` and `shoal merge`.

## Undo a merge

`shoal unmerge [-s SPACE]` undoes the workspace's latest local merge into the
default branch while that merge is unpushed. It works for every method. The
branch and its commits stay as they are. When cleanup deleted the branch,
`unmerge` creates it again at the commit that merged.

Restore a workspace that cleanup removed with `shoal restore <workspace>`
first. `unmerge` names that command when it refuses. After an unmerge, the
branch's commits no longer count as merged, so cleanup keeps the workspace.

When the default branch is still at the merge's result, Shoal moves it back to
the commit before the merge. When commits came after the merge, Shoal replays
them onto that commit with `git rebase --rebase-merges`, in a worktree of its
own, and then moves the default branch to the result. Each later merge keeps
its merge commit, so you can unmerge it in turn.

The default branch has the same requirements as for a
[local merge](#local-merges). An agent's scoped `unmerge` needs approval under
the same `merge` and `push` permissions. The request names the merge it undoes,
and after approval the agent's retry undoes it.

### When unmerge refuses

`unmerge` changes nothing and says why when:

- the default branch's upstream has the merge's result, as the last fetch saw
  it,
- a later merge's branch contains the unmerged branch's commits, as a stacked
  workspace's does, since the replay would bring them back; the refusal names
  that merge, to unmerge first,
- a later merge commit records changes its parents do not merge into by
  themselves, such as a conflict resolution, which the replay would drop,
- the default checkout has ignored files where the undone default branch has
  files, since Git would overwrite them; this happens when the merge stopped
  tracking a path that was then recreated locally,
- the replay does not apply cleanly.

For a pushed merge, the refusal gives the revert that undoes it, for you to
push:

| Merge | Revert |
|---|---|
| Merge commit | `git revert -m 1 <commit>` |
| Squash | `git revert <commit>` |
| Fast-forward or rebase | `git revert <before>..<commit>` |
| Fast-forward that brought merge commits | the inverse diff, applied and committed |

After you revert a merge commit or a fast-forward, Git still counts the
branch's commits as merged. Merging the branch again then brings in only newer
commits. After you revert a squash or rebase, merging the branch again brings
its changes back.

## Refresh

```sh
shoal refresh              # The workspace this runs in, otherwise a picker
shoal refresh 4            # Workspace 4
```

`shoal refresh [WORKSPACE]` brings a workspace up to date with what it was made
from, while the workspace holds nothing of its own:

| Workspace | Refresh moves it to |
|---|---|
| On a branch with an upstream | the upstream, fetched |
| Any other branch workspace | the branch it builds on: its base workspace's, else the branch its recorded base names (a remote one fetched), else the local default branch, synced first as `shoal repo sync` does, or as the checkout has it when that sync fails |
| At a commit made for a PR | the PR's head as the forge reports it now |
| At a commit made for a branch | the branch's tip, resolved as `--base` resolves it |
| At a tag or a bare commit, or a copy of a checkpoint | nothing: it is up to date |
| A copy of a live workspace | the source's current files, copied again |

A branch workspace's branch moves only by fast-forward, never by rebase or
merge. A copy's branch follows its source's HEAD, wherever that went. A
workspace at a commit is checked out at its new commit. That commit becomes its
starting commit, which `shoal diff` compares with.

`--json` prints the workspace with `target`, `from` and `to` (HEAD before and
after), `up_to_date`, and `setup`, which says whether setup ran again.

### Up to date or refused

A workspace is up to date when it already has its target's commits. A copy is
up to date when its source still has the HEAD, index and unsaved files the copy
was made with.

Otherwise, work of the workspace's own refuses the refresh. The refusal names
that work, naming commits by their subjects:

- commits its target lacks, and for a branch the `git rebase` that moves
  them,
- for a workspace at a commit, commits after its starting commit,
- uncommitted changes, by path, to commit, stash or discard,
- for a [copy](create.md#copies), commits since it was copied and files that
  differ from those it started with, as cleanup compares them. The refusal
  names `shoal start --copy` for a fresh copy.

### Refresh a copy

A refresh copies a copy again in place, as `--copy` copies. The source must
pass the same checks, and a removed source is refused. The refresh runs these
steps:

1. A [checkpoint](removal.md#checkpoints) keeps the copy's HEAD and
   unsaved files.
2. The copy's tracked, untracked and ignored files and its index become the
   source's. The copy's own ignored files go, as removal deletes them, since
   they count as nobody's work.
3. HEAD moves to the source's, and the copy's branch with it.
4. A new starting checkpoint records what the copy now holds. The old one
   expires as any checkpoint does.
5. Setup and the post-setup hook run again, as `shoal setup` runs them, unless
   the copy was created with `--skip-setup`.

The diff base, links, leases, holds and agent session stay. A refresh of any
other workspace runs no setup.

### When a refresh runs

A workspace refreshes only when you ask. One exception: when the
[queue](queue.md) admits an agent in a workspace whose branch holds nothing of
its own, the agent starts on that branch fast-forwarded as a refresh would.

A refresh needs a ready workspace with no Git operation under way. No command
or agent may run there but the caller's own:

- an agent refreshes its own workspace,
- a coordinator does not refresh a workspace where a child's agent runs.

The refusal names what runs and `shoal stop`. A refresh takes the workspace's
`updating` state, so it never overlaps removal, restore or setup.

A chat has no worktree to refresh. `shoal refresh` run from a chat is refused,
even from a chat in a workspace's worktree.

Workspace processes refresh their own workspace and those below it. They
refresh a copy only from a source their scope reaches, as they copy.

## Diff

```sh
shoal diff                 # The current workspace, otherwise a picker
shoal diff --thread 3.4    # The changes agents made for a thread
```

`shoal diff` runs native `git diff`, so your pager and external diff apply. It
shows committed, staged and unstaged changes to tracked files. Untracked files
follow Git.

The diff starts at the fork point of the base branch recorded when the
workspace was made. Shoal finds the fork point from the base branch's reflog,
and falls back to the merge base. Commits added to the base later stay out,
even after a rebase. A workspace made at a commit compares with its merge base
with that commit. A missing or unrelated base is an error.

A chat's agent runs `shoal diff` as a plain process in the worktree, since its
scope reads the workspace but runs nothing there. `--thread` prints a thread's
[changes](threads.md#agents-changes) instead.

### A workspace on another machine

`shoal diff` of a workspace on another machine of the hub, by its number or ID,
runs `git diff` there as a plain read. That machine's Git config for the
repository applies, such as `diff.algorithm`, `diff.external` and textconv. The
command exits with Git's exit code.

This machine's Git decides how the diff shows:

- On a terminal, `color.diff` sets its colour. It goes through the pager Git
  starts for `git diff`, as `pager.diff` and `GIT_PAGER` choose it, with Git's
  `LESS` and `LV` defaults.
- Otherwise, the bytes go out unchanged.

An external diff tool that checks for a terminal itself sees a pipe, as under
`git diff | less`.

## Conflicts

```sh
shoal conflicts            # Against the base workspace's branch, otherwise the default branch
shoal conflicts release    # Against another branch or commit
```

`shoal conflicts` reports the paths that a merge of the workspace's committed
HEAD into the target would leave conflicted. It touches no worktree, index or
ref, and checks no uncommitted changes. It needs Git 2.38 or newer.

The target defaults to the base workspace's branch, otherwise the local
default branch. Run `shoal repo sync` first to compare with the remote's latest
default branch.

The command exits 1 when it finds conflicts or files changed on both sides.
`--json` returns `target`, `target_commit`, `head`, `conflicts`, `files` and
`changed_on_both_sides`.

### Files changed on both sides

Some files hold a number each change picks for itself, such as a schema
migration or a protocol version. Two branches that pick the same number make
identical edits, which Git merges cleanly. A repository lists such files in
[`[conflicts] sensitive`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#conflicts).

`shoal conflicts` lists under "Changed on both sides" each such file that both
HEAD and the target changed since their merge base. It leaves the file out when
the target already has HEAD's changes, as [removal](removal.md#branches) decides merges.
To fix one, update the branch from the target and pick a number past the
target's.

## Branches on the forge

```sh
shoal branch ls                     # The forge's branches of the space's repository
shoal branch ls --unmerged          # Only those whose changes the default branch lacks
shoal branch ls --repo team/tool    # Another repository's
shoal branch delete fix-login old-api  # Delete branches on the forge
shoal branch delete spike --force   # One whose changes the default branch lacks
```

The `branch` commands act on the forge's branches, never on local ones. You act
with your login, and a scoped process as the agent account.

### List branches

`shoal branch ls` lists the branches of the space's repository, or of the one
`--repo` names as for `issue ls`. Each branch shows:

- its head commit and that commit's date, which GitHub gives only to a login,
- whether it is the default branch and whether the forge protects it,
- whether the default branch has its changes,
- the open PRs whose head it is,
- the workspace whose branch it is, and where that workspace's work stands.

A workspace's work is `done` once marked done. Otherwise it is `merged` once
the default branch has the changes of its local branch, which may hold commits
it has not pushed, and else `active`.

Whether the default branch has a branch's changes is the check cleanup makes,
which counts squash and rebase merges. Shoal makes it in the registered checkout
against the commits the daemon fetched. The answer is unknown for a commit the
checkout lacks and in a repository that is not registered. `--merged` and
`--unmerged` list only the branches the check answers so, and leave out the
unknown ones.

`--json` returns each branch's `name`, `head`, `committed_at`, `protected`,
`default`, `merged` (`null` when unknown), `open_prs`, and `space` with its
`id`, `number`, `name` and `state`.

### Delete branches

`shoal branch delete` deletes the named branches in the space's repository.
With `--repo` it deletes them in a registered repository, and from a chat
without a repository in any registered one.

It checks every named branch first, and deletes none while it refuses one. It
refuses:

- the default branch,
- a branch the forge protects,
- the head or base of an open PR, including a PR from a fork into the
  repository it was forked from,
- a branch whose changes the default branch lacks, or for which Shoal cannot
  tell, unless `--force`,
- for a scoped process, the branch of an active workspace outside its scope,
  whatever `--force` says.

Just before deleting, it checks that each branch still points to the commit it
was read at. It then deletes each one with a Git push over HTTPS, which the
forge refuses once the branch points elsewhere. A branch that a push moved
after the check stays, and so do the branches after it.

A scoped process deletes as its space's
[`push` permission](modes.md#mode-permissions) allows. `"propose"` makes one
approval request per branch, bound to the commit the branch points to, so a
branch that moved needs another request.

A workspace keeps its local branch. Its commits count as unpushed until it
pushes again. `--json` returns the deleted branches' `name` and `head`.
