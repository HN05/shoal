# Branches and merges

This file says what `shoal merge`, `unmerge`, `refresh`, `diff`, `conflicts` and
the `branch` commands do.

## Merge into the default branch

`shoal merge [pr] [NUMBER|URL] [-s SPACE]` merges a space's work, or the PR
its item names as `shoal view` reads one: a number, which `--repo` places in
another repository, or a PR URL of any repository, each alone or after `pr`,
which alone names the space's assigned PR; issues, CI runs and reviews do not
merge. Without an item, a space with one open PR linked
as assigned or opened merges it on the forge as an
[item action](issues.md#issue-and-pr-actions), a space with several is refused, naming
them, a chat without one is refused, and a workspace without one merges its
recorded branch into the repository default branch, as below; to merge locally
while a PR is open, close the PR. `-X` and `--message` apply to both,
`--delete-branch`, `--auto` and `--cancel-auto` to a PR merge, and `--push` and
`--no-push` to a local merge; a flag of the other path is refused.
Scoped callers merge the work of the spaces below their own with `-s`, under
their own `merge` and `push` permissions. The local merge of a workspace
another machine of the hub runs, and its `unmerge`, run on that machine,
into its checkout's default branch, under the `[merge]` settings that
machine reads for the workspace.

A local merge is for repositories without a pull-request flow. When the
default branch has an upstream, Shoal fetches and fast-forwards it first,
preserving an ahead branch and refusing divergence or a failed fetch. The default
branch cannot be held by a managed workspace, and any other checkout of it must be
clean. The workspace must be clean and on its recorded branch. It merges by
`-X`, else by [`[merge] method`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#merges):

- `merge`, the default: a fast-forward, or a merge commit of the two that
  Shoal makes as `git merge` would, without touching a checkout;
- `squash`: one commit on the default branch, by whoever merges, whose files
  are the merge of the default branch and the branch, as `git merge --squash`
  and a commit make it. `--message` gives its message; without it, a branch of
  one commit keeps that commit's message, and one of several gets Git's own
  squash message. `--message` is refused for the other methods;
- `rebase`: the branch's commits that the default branch lacks, copied onto it
  as `git rebase` copies them, keeping their authors, in a worktree of Shoal's
  own; merge commits are left out.

Every method moves the default branch to the commit it made, fast-forwarding
its checkout. A squash or rebase leaves
the workspace's branch as it was, as a forge's squash or rebase merge leaves a
PR's branch. A merge that does not apply cleanly changes nothing: merge the
default branch into the workspace with Git (`git merge main`), or for a rebase
merge rebase the workspace onto it (`git rebase main`), resolve the conflicts
there, and run `shoal merge` again.
Merged commits count as pushed for `rm` and automatic cleanup.

A merge that its [review gate](reviews.md#local-reviews) refuses goes no further. An
agent's scoped `shoal merge` needs the user's approval for the branch at its
current commit, by its method and with a squash's message, and for where it pushes, unless its [`merge` permission](modes.md#mode-permissions) is `"direct"` and
its [`push` permission](approvals.md#approval-for-agent-pushes) is `"direct"` too. The first run records a
`merge` request in `shoal approvals` and exits 2 with `code: "approval_pending"`;
the request names the method, a squash's message subject, the branch, the default branch it merges into, its commit count,
the remote branch it pushes to, any other local commits of the default branch
that the push publishes too, and any files changed on both sides as below. After approval, running it again merges that commit with every check above. A new
commit, another method or message, a change to where it pushes, other local commits of the default branch
that its push would publish too, or a new set of files changed on both sides
replaces the request; a sync of the default branch from its upstream does not.
Approving a request after its branch moved is refused as stale, and a denied commit stays refused
with `"approval_denied"`.

With [`[merge] push`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#merges), or `--push` for one run, Shoal pushes the default branch to
its upstream after merging; `--no-push` keeps one merge local. A default branch without an
upstream is refused before merging, and a failed push keeps the merge. Where
[`[pr] watch_merge_runs`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#prs)
is on, the workspace follows the default branch's runs for the commit the push
published, on the forge of the first URL it pushed to on a network host, as it follows a merged PR's
[runs](links.md).

A merge that pushes asks first, after Shoal has checked it and before anything
changes. As a [PR merge](issues.md#issue-and-pr-actions) does, so does a merge whose last
check found files [changed on both sides](#conflicts) with the default branch.
The question names the method and direction, a squash's message subject, the commit count, where it pushes, other
local commits of the default branch that the push publishes too, and those
files:

```text
Merge fix-login (3 commits) into main, then push main to origin.
Sensitive files changed on both sides by fix-login and main: src/version.rs.
Are you sure? [y/N]
```

The default answer is no. A merge is refused when, while the question was open,
its branch moved, the default branch took commits other than its upstream's, its
method or message is another, the push destination changed, or a conflict check found other files changed on both
sides than the question named. `--yes` skips the question, and `[merge] confirm =
false` skips it for a merge that pushes; without a terminal, as in scripts or
with `--json`, a merge that would ask is refused unless the question is skipped.
A merge that stays local asks only about files changed on both sides. An agent's
merge asks about its push through its approval request instead; it still asks
about files changed on both sides, so the agent passes `--yes` and the approver
decides on them.

`[merge] enabled = false` turns local merges off for a repository that merges
only through PRs: it refuses both a local merge and an agent's request for one,
pointing to `shoal pr open` and `shoal merge`.

## Undo a merge

`shoal unmerge [-s SPACE]` undoes the workspace's latest local merge into the
default branch while that merge is unpushed. It works for every method, and the branch and its commits stay as they are; when cleanup deleted
the branch, `unmerge` creates it again at the commit that merged. A workspace
cleanup removed comes back first with `shoal restore <workspace>`, which
`unmerge` names when it refuses. Afterwards the branch's commits no longer count
as merged, so cleanup keeps the workspace.

When the default branch is still at the merge's result, Shoal moves it back to the
commit it was at before the merge. When commits came after the merge, Shoal
replays them onto that commit (`git rebase --rebase-merges`) in a worktree of its
own, then moves the default branch to the result, so each later merge keeps its
merge commit and can be unmerged in turn. `unmerge` changes nothing and says why
when:

- the default branch's upstream has the merge's result, as the last fetch saw it.
  The refusal gives the revert that undoes it, to push: `git revert -m 1 <commit>`
  for a merge commit, `git revert <commit>` for a squash, `git revert
  <before>..<commit>` for a fast-forward or rebase, or, when that fast-forward
  brought merge commits, the inverse diff applied and committed. After a merge
  commit or fast-forward, Git then counts the branch's commits as merged, so
  merging the branch again brings in only newer commits; after a squash or
  rebase, merging it again brings its changes back;
- a later merge's branch contains the unmerged branch's commits, as a stacked
  workspace's does, since replaying it would bring them back. The refusal names
  that merge, to unmerge first;
- a later merge commit records changes its parents do not merge into by
  themselves, such as a conflict resolution, which the replay would drop;
- the default checkout has ignored files where the undone default branch has
  files, as when the merge stopped tracking a path that was then recreated
  locally, since Git would overwrite them;
- the replay does not apply cleanly.

The default branch has the same requirements as for `merge`, and an agent's scoped
`unmerge` needs approval under the same `merge` and `push` permissions: the
request names the merge it undoes, and after approval the agent's retry undoes it.

## Refresh

```sh
shoal refresh              # The workspace this runs in, otherwise a picker
shoal refresh 4            # Workspace 4
```

`shoal refresh [WORKSPACE]` brings a workspace up to date with what it was made
from while it holds nothing of its own:

| Workspace | Refresh moves it to |
|---|---|
| On a branch with an upstream | the upstream, fetched |
| Any other branch workspace | the branch it builds on: its base workspace's, else the branch its recorded base names (a remote one fetched), else the local default branch, synced first as `shoal repo sync` does, or as the checkout has it when that sync fails |
| At a commit made for a PR | the PR's head as the forge reports it now |
| At a commit made for a branch | the branch's tip, resolved as `--base` resolves it |
| At a tag or a bare commit, or a copy of a checkpoint | nothing: it is up to date |
| A copy of a live workspace | the source's current files, copied again |

A branch workspace's branch moves only by fast-forward, never by rebase or
merge; a copy's branch follows its source's HEAD, wherever that went. A workspace at a
commit is checked out at its new commit, which becomes its starting commit and
what `shoal diff` compares with. A workspace that already has its target's
commits, or a copy whose source still has the HEAD, index and unsaved files it
was copied with, is up to date. Otherwise work of its own refuses the refresh,
which names it: commits its target lacks, or for a workspace at a commit
commits after its starting commit, with their subjects and for a branch the
`git rebase` that moves them; uncommitted changes, by path, to commit, stash or
discard; for a [copy](create.md#copies), commits since it was copied and files that
differ from those it started with, as cleanup compares them, with `shoal start
--copy` for a fresh copy. Nothing else refreshes on its own, except that an
agent the [queue](queue.md) admits in a workspace whose branch holds nothing
of its own starts on that branch fast-forwarded as a refresh would.

A copy is copied again in place, as `--copy` copies: the source must pass the
same checks, and a removed source is refused. A [checkpoint](removal.md#checkpoints-and-restore)
first keeps the copy's HEAD and unsaved files, then its tracked, untracked and
ignored files and its index become the source's: ignored files the copy had are
gone, as removal deletes them, since they count as nobody's work, and HEAD moves to the source's, its branch with
it. A new starting checkpoint records what it now holds, and the old one expires
as any checkpoint does. Setup and the post-setup hook run again, as `shoal
setup` runs them, unless the copy was created with `--skip-setup`; the other
cases run no setup. The diff base, links, leases, holds and agent session stay.

The workspace must be ready, with no Git operation under way, and no command or
agent may run there but the caller's own: an agent refreshes its own workspace,
and a coordinator not one where a child's agent runs; the refusal names what
runs and `shoal stop`. A refresh takes the workspace's `updating` state, so it
does not overlap removal, restore or setup. A chat has no worktree to refresh,
and `shoal refresh` run from a chat, even one in a workspace's worktree, is
refused. Workspace processes refresh their own workspace and those below it,
and a copy only from a source their scope reaches, as they copy.
`--json` prints the workspace with `target`, `from` and `to` (HEAD before and
after), `up_to_date`, and `setup`, whether setup ran again.

## Diff

```sh
shoal diff                 # Current workspace, otherwise fzf
shoal diff --thread 3.4    # The changes agents made for a thread
```

Runs native `git diff` (your pager and external diff apply) from the fork point
of the base branch recorded at creation, using its reflog with merge-base as a
fallback, so commits added to the base later are excluded even after a rebase.
Fixed-commit bases use that commit; older workspaces without base metadata use
`main`. Committed, staged, and unstaged tracked changes appear; untracked files
follow Git. A missing or unrelated base is an error. A chat's agent runs it as a
plain process in the worktree rather than a tracked execution of the
workspace, which its scope may read but not run in. `--thread` prints a
thread's [changes](threads.md#agents-changes) instead.

`shoal diff` of a workspace on another machine of the hub, by its number or
ID, runs `git diff` there as a plain read, under that machine's Git config for
the repository, such as `diff.algorithm`, `diff.external` and textconv, and
exits with Git's exit code. This machine's Git decides how it shows: on a
terminal, colour as `color.diff` says and the pager Git starts for `git diff`,
as `pager.diff` and `GIT_PAGER` choose it, with Git's `LESS` and `LV` defaults; otherwise the bytes unchanged. An external
diff tool that checks for a terminal itself sees a pipe, as under
`git diff | less`.

## Conflicts

```sh
shoal conflicts            # Against the base workspace's branch, otherwise the default branch
shoal conflicts release    # Against another branch or commit
```

Reports the paths a merge of the workspace's committed HEAD into the target would
leave conflicted, without touching the worktree, index or refs; uncommitted
changes are not checked. The default target is the local branch, so run
`shoal repo sync` first to compare with the remote's latest default branch. Requires
Git 2.38 or newer.

Some files hold a number each change picks for itself, such as a schema
migration or protocol version. Two branches that pick the same number make
identical edits, which Git merges cleanly. A repository lists such files in
[`[conflicts] sensitive`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#conflicts), and `shoal conflicts` also lists, under
"Changed on both sides", each listed file that both HEAD and the target changed
since their merge base, unless the target already has HEAD's changes as
[removal](removal.md) decides merges. Update the branch from the
target and pick a number past the target's.

Exits 1 on either; `--json` returns `target`, `target_commit`, `head`,
`conflicts`, `files` and `changed_on_both_sides`.

## Branches on the forge

```sh
shoal branch ls                     # the forge's branches of the space's repository
shoal branch ls --unmerged          # only those whose changes the default branch lacks
shoal branch ls --repo team/tool    # another repository's
shoal branch delete fix-login old-api  # delete branches on the forge
shoal branch delete spike --force   # one whose changes the default branch lacks
```

`shoal branch ls` lists the branches on the forge of the space's repository,
or of the one `--repo` names as for `issue ls`, never local ones: each with
its head commit and that commit's date, which GitHub gives only to a login,
whether it is the default branch or the forge protects it, whether the
default branch has its changes, the open PRs whose head it is, and the
workspace whose branch it is, with where that workspace's work stands: `done`
once marked done, else `merged` once the default branch has the changes of
its local branch, which may hold commits it has not pushed, else `active`.
Whether the default branch has a branch's changes is the check cleanup makes, which counts squash and rebase
merges, in the registered checkout against the commits the daemon fetched;
it is unknown for a commit the checkout lacks and in a repository that is not
registered. `--merged` and `--unmerged` list only the branches the check
answers so, leaving out the unknown ones. You read them with your login, a
scoped process as the agent account. `--json` returns each branch's `name`,
`head`, `committed_at`, `protected`, `default`, `merged` (`null` when
unknown), `open_prs` and `space` with its `id`, `number`, `name` and `state`.

`shoal branch delete` deletes the named branches on the forge, in the space's
repository or, with `--repo`, a registered repository, from a chat without a
repository any registered one, never a local branch. It checks every named
branch first and deletes none while one is refused: the default branch, a
branch the forge protects, the head or base of an open PR, a PR from a
fork into the repository it was forked from included, a branch whose
changes the default branch lacks, or of which Shoal cannot tell, unless
`--force`, and for a scoped process the branch of an active workspace outside
its scope, whatever `--force` says. Just before deleting any, it checks that
each branch still points to the commit it was read at, then deletes each with
a Git push over HTTPS that the forge refuses once the branch points elsewhere:
a branch that a push moved after the check stays, and so do the branches after
it. You delete with your login, a scoped process as
the agent account, as its space's [`push` permission](modes.md#mode-permissions)
allows: `"propose"` makes one approval request per branch, bound to the commit
it points to, so a branch that moved needs another. A workspace keeps its
local branch, whose commits count as unpushed until it pushes again. `--json`
returns the deleted branches' `name` and `head`.
