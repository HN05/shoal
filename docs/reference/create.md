# Child workspaces

This file says what `shoal create` does, how child workspaces and workspaces at
a commit work, and how copies work. [Spaces](../design/spaces.md) gives the
reasons.

```sh
shoal create --branch fix-typo               # Inside a space, a child of it, from the default branch
shoal create --issue 42                      # Branch named after the issue, which it links
shoal create --branch part-two --base fix-api  # Stacked on the parent's branch
shoal create --branch fix-typo --parent 3
shoal create my-project --branch fix-typo    # Outside a space, a top-level workspace
shoal create https://github.com/owner/repo/issues/34
shoal start --issue 43 --agent claude                  # Inside a workspace: a child with an agent of its own
shoal start my-project --branch api-part --agent codex -- "Move the API client into its own crate"
shoal start my-project --branch api-part --fork -- "Implement the API part of the plan"  # Forks the parent agent's session
shoal create --no-branch                # A child at the default branch, with no branch of its own
shoal create --no-branch --base v1.4.0  # At a tag or commit
```

## Create a workspace

`shoal create` creates a workspace as `shoal start` creates one, but starts no
agent there. It takes the arguments of `shoal start` that choose what the
workspace starts from and where it goes: a repository, an issue, PR, CI run or
branch URL, `--branch`, `--issue`, `--base`, `--copy` and the creation options.
`shoal start --in` starts an agent there later.

`shoal create` runs the workspace's setup and returns the workspace with its
`path`. From your own terminal, it then enters the workspace.

A create that names nothing a branch comes from opens the source picker on a
terminal. Without a terminal it is refused.

`--base` works as for `shoal start`. Naming the parent's branch also
[stacks](workspaces.md#stacked-workspaces) the child on it.

## Where a child goes

Inside a space, `shoal create` creates a child of it and records the parent.
Outside a space, it creates a top-level workspace.

The parent is the space `--parent` names, else the current one, a workspace or
a chat. In a chat, the current space is the workspace below the chat that
contains the current directory, else the chat itself. So a chat in a
workspace's worktree creates below itself.

A child goes in its parent's repository. A child in another repository is
refused. A chat without a repository creates only these:

- a `--copy`, in the copied workspace's repository;
- a workspace in the repository it names.

A workspace parent must be ready. Children can have children of their own.

## Children with an agent

Run inside a workspace or a chat, `shoal start` also creates a child of that
space, or of a space below it that `--parent` names, in its repository. A chat
it starts opens in a [background place](chats.md#place).

`--agent` gives the child an agent of its own. The agent starts on the issue or
on the arguments after `--`. It holds the child's own scope, never the
caller's. It starts through the [agent queue](queue.md), as any launch does,
once the child is ready. A detached Codex runs as its CLI.

Shoal checks the agent, and the program of the first host that would start it,
before it creates the child. The launch reopens only the child, and fails when
the child was removed meanwhile.

### Fork the parent's agent

`shoal start --fork` starts the parent's agent in the new workspace from a fork
of its session. The child's agent begins with the parent's conversation, then
the prompt after `--`. The parent's session stays unchanged.

The parent is the space, a workspace or a chat, that `--parent` names, else the
current one. Run from an agent, `--fork` forks that agent's session. Otherwise
it forks the session of the parent's only running agent, which must have
reported its session through its hooks.

The agent copies the session when it starts, so a queued fork includes what the
parent did meanwhile.

`--fork` is refused before the child is created for an agent without
[`fork`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
arguments, and for one an app such as Happy runs.

## What a scoped process may do

A space's processes may:

- create children of their own space;
- act on every space below it;
- `shoal rm` a space below it. Removing one the user holds, or one with a
  workspace below it that the user holds, follows their space's
  [`user_holds` permission](modes.md#permitted-actions).

They queue agents only in the workspaces their scope reaches, not in their own.
Every other creation and removal stays with the user.

## Workspaces at a commit

`--no-branch` creates a workspace at a commit: a worktree at a detached HEAD,
with no branch and nothing to push. Use one to compare with another commit, or
to read and run older code without touching the parent's worktree.

`--no-branch` takes no `--branch`, `--issue`, `--path` or `--git-profile`.

### Starting commit

`--base` resolves as for a branch. It defaults to the refreshed default branch.
When a tag or commit does not resolve, Shoal fetches origin with its tags once
and tries again. Naming the parent's branch starts the workspace at the
parent's commit without stacking it.

`shoal diff` compares with the starting commit.
[`shoal refresh`](branches.md#refresh) moves that commit to a PR's new head or
a branch's new tip.

### Name and place

The workspace is named after what it starts at: a branch with its short commit,
such as `main at 3f2a9c1`, a tag or a commit. Its directory takes the same
name, such as `main-3f2a9c1`, with the usual suffix when taken.

A space's process creates it as a child of its own space, or of the space below
it that `--parent` names. You create it as a child of `--parent` or of the
current space. Outside a space, you create a top-level workspace of the
repository you name or the current one.

### Work in it

Its owner, a CI job, a reviewer or you run things in it with `shoal run`,
`shoal demo` or a shell. `shoal start --in` starts an agent of its own there.
That agent's opening says that without a branch it cannot push or open a PR.

Commands that need a branch refuse it, saying it is at a commit, such as
`pr open`, `merge`, `branch rename` and CI runs on its branch. Every other
command works as in any workspace.

`{branch}` is empty for it in configured commands and agent templates. Place
names use its name instead.

### In status

`status` shows `At:` with its commit and what it was made for, where a branch
workspace shows `Branch:`. `list --json` and `status --json` give `at` in place
of `branch`, with:

- `commit`;
- `made_for`: `{"ref": name}` or `{"pr": number}`.

## Activity of the spaces below

The parent's agent, a workspace's or a chat's, receives the issue and PR
activity of each space below it that has no agent of its own. It also receives
the activity of the spaces below those in turn. The activity arrives through
its hooks and through an unfiltered or kind-filtered `watch`. Each update names
its space in `workspace`.

A child with an agent of its own receives its own activity and its subtree's
instead. An agent of its own is one that reports its turn state, any agent
Shoal tracks there, or one whose restore waits in the [agent queue](queue.md#restores).

## Coordinators

A space's coordinator is the agent of its nearest space above that has one, a
workspace's or a [chat](chats.md)'s.

The coordinator receives, as alerts naming the space, what would otherwise
notify the user:

- the alerts the space's agent sends with `shoal send`, and the threads it
  opens;
- its completion;
- its ready marks, which never notify the user otherwise;
- the daemon's reports that its agent exited or was stopped.

The user receives them instead when the space has no coordinator, or when the
coordinator's unread alerts are full.

A space without an agent of its own is its coordinator's own work, so what a
process does there reaches the user.

A coordinator answers a thread opened below it with
`shoal send <S.N> "<answer>"`, or passes it up with
`shoal thread forward <S.N>`.

## Parents and children in lists

`shoal list` lists children indented below their parent. `status` names the
parent and the children. `list --json` and `status --json` give, for workspaces
and chats alike:

- `parent`: its `id`, `name`, `kind` (`workspace` or `chat`) and a workspace's
  `branch`, or null;
- `children`: the spaces below it.

## Cleanup and removal

A child keeps its parent only from
[merged cleanup](removal.md#merged-workspaces). The parent's idle, completion
and PR cleanup follow its own state.

### Remove a parent

Every removal of a parent first removes the spaces below it, deepest first,
whatever their work, holds or threads. That covers `shoal rm`, cleanup and the
cleanup of a deleted directory.

- Each workspace stops its agents. Unless it is disposable, it keeps a
  checkpoint of its HEAD and unsaved files, so `shoal restore` brings it back.
  It also keeps its branch unless a remote or the default branch has its
  commits.
- Each chat stops its agent.

A child that cannot go retains the parent.

`shoal rm` names the children before asking, and its result names them in
`children`. Each child's journal records a `removed` event with the cause
`parent`. Shoal notifies the user of the children an unattended removal took.

[`shoal move`](move.md) takes the spaces below a space with it.

### A parent's completion

While a parent's `done` with cleanup stands at its HEAD, each workspace and
chat below it goes after the [grace period](removal.md#grace-period), whatever
its own work. Its `cleanup_reason` is `ancestor_completed`, and its removal
records the cause `parent_completion`. A chat goes with its agent stopped.

These of its own keep it:

- a hold;
- a waiting thread it opened;
- a pending PR proposal or push request;
- a queued agent launch;
- a space still below it, until that space goes.

`shoal done --undo`, a commit after `done`, or a `done` without cleanup leaves
the children. A child's own `done` follows the usual
[completion](completion.md) rules.

## Subagents in a child

Claude Code launched in a workspace runs `shoal internal worktree-create` and
`worktree-remove` as its `WorktreeCreate` and `WorktreeRemove` hooks. A
worktree it creates, such as one for a subagent with `isolation: "worktree"`,
becomes a child of the workspace that contains its directory. The child's
branch is named after the worktree, and Claude Code enters the worktree once
setup finishes.

The subagent then works in that child, which has no agent of its own. So the
child's activity reaches the agent that started the subagent.

Removing the worktree marks the child done. Claude Code removes none when a
subagent finishes, so the child stays until `shoal done` or merged cleanup.

Claude Code and Codex launched in a workspace also run
`shoal internal subagent-start` as their `SubagentStart` hook. It tells each
subagent which workspace it works in:

- the child whose directory it starts in;
- for one that starts in the agent's own workspace, as a Codex subagent does
  with the agent's instructions, the child its brief names. The agent's own
  workspace stays for the agent to finish.

## Copies

```sh
shoal start --copy                       # A copy of the current workspace, with an agent of its own
shoal start --copy 7 --fork -- "Try a queue instead of polling"
shoal create --copy 7                    # A child copy without an agent
shoal create --no-branch --copy 7        # A copy at 7's HEAD, without a branch
shoal start --copy 7 --checkpoint 3      # From 7's checkpoint 3, live or removed
```

`--copy [WORKSPACE]` on `shoal start` and `shoal create` starts a workspace from
a copy of a workspace's current files: its HEAD with its staged, unstaged,
untracked and ignored files. Without a value it copies the current workspace,
else one you pick. Use a copy to try another approach from where an agent is
now, to debug a workspace's live state without touching it, or to hand the same
state to a second agent.

`--checkpoint <NUMBER>` starts from that checkpoint of the `--copy` workspace
instead, a live or a removed one's. `shoal restore <workspace> --checkpoint`
lists a workspace's checkpoints, and `shoal restore` names each removed
workspace's removal checkpoint.

Neither combines with `--base`. `--base` names a Git ref, so it brings
committed work only, even for a branch a workspace has checked out.

A copy is an ordinary, writable workspace with one editing agent of its own.
Its parent follows the usual rules, not the source.

### Branch

A copy goes on a new branch at the copied HEAD. The branch is the one given,
else the copied branch's name with `-copy` added, suffixed as usual when taken.
For a workspace at a commit, the short HEAD takes the branch name's place.

`shoal create --no-branch` makes the copy a workspace at a commit instead.

### Copy current files

A copy of current files is a linked worktree that Git creates without checking
anything out. Shoal copies the source's index and worktree Git config into it
and clones its files, so `git status` and `git diff` match the source's.

Shoal clones each file the cheapest way the filesystem allows:

- an APFS clone on macOS;
- a reflink on Linux;
- a snapshot of each top-level entry that is a btrfs subvolume holding no
  other, as a [seed](setup.md#seed-builds) output is;
- otherwise a plain copy.

The source must be ready, its worktree must verify, and no Git operation may be
under way there. An agent running there does not matter, since the copy only
reads the source.

Shoal refuses a source with files a checkpoint cannot hold, or Git metadata the
copy would share:

- an untracked nested repository;
- a changed submodule;
- a checked-out submodule;
- a worktree inside an ignored directory.

### Copy a checkpoint

A copy of a checkpoint checks out the checkpoint's HEAD and writes its files as
uncommitted changes, as `shoal restore` does. It does not tell staged changes
from unstaged ones. A checkpoint that kept HEAD only is refused.

### When the copy is made

A start the [agent queue](queue.md) holds makes its copy when it leaves the
queue, so it copies the files of that moment. `--now` copies them at once. To
keep an earlier moment, take a checkpoint with `shoal checkpoint` and copy it
with `--checkpoint`.

`shoal start` returns before the copy is made. The new workspace's preparation
makes it, checking the source's files as it copies them. `shoal setup` retries
a copy that failed, copying the source's files as they are then.

### Setup

A copy of current files takes its source's ignored files instead of the
[seed](setup.md#seed-builds) clone and `setup.copy_ignored`. A copy of a
checkpoint holds no ignored files, so it starts from the seed like any new
workspace. The rest of setup runs as for any workspace.

### What a copy takes

A copy takes its source's diff base: its base ref, base commit and base
workspace, or a removed workspace's from its removal record. So `shoal diff`
and `shoal conflicts` show the source's work and the copy's own.

A copy of current files keeps the source's worktree Git config, and so its
[Git profile](repo.md#git-profiles), unless `--git-profile` names another. A
copy of a checkpoint gets the profile a new workspace gets.

A copy takes none of the source's links, leases, holds, threads, ready marks or
checkpoints, and no agent session. `shoal start --copy --fork` starts the
copy's agent from a fork of the copied workspace's agent's session, which stays
unchanged. `--fork` does not combine with `--checkpoint`.

[`shoal refresh`](branches.md#refresh) copies the live source's files into the
copy again.

### Starting checkpoint and cleanup

Once its files are in, a copy takes a checkpoint of itself, its starting
checkpoint. The starting checkpoint keeps every unsaved file and does not
expire while the copy exists. A later checkpoint, an agent's included, keeps
those files too, whatever their size, while they still match it.

The commits up to the copy's HEAD count as kept, and files that still match the
starting checkpoint count as unchanged. So
[automatic cleanup](removal.md#automatic-cleanup) removes a copy with nothing of
its own, as it removes an empty workspace. Commits on top and files that differ
are the copy's own work.

Removal still deletes the copy's branch only when a remote or the default
branch has all its commits. Otherwise it [keeps](removal.md#kept-branches) the
branch.

### Who copies

Workspace processes copy only their own workspace and the workspaces below it.
They never copy from a removed workspace's checkpoint.

### Copies in status

`status` shows `Copied from:` with the workspace, and `(checkpoint <n>)` for a
copy of a checkpoint. `list --json` and `status --json` give `copied_from`,
with:

- `workspace`: its `id`, `name` and `branch`, or null once it is removed;
- `checkpoint`, the copied checkpoint's number, for a copy of a checkpoint;
- `starting_checkpoint`, the number of the copy's starting checkpoint;
- `head` and `commit`, that starting checkpoint's HEAD and the commit that
  keeps its files;
- `index`, the tree of the index the copy started with.
