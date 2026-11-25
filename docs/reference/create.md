# Child workspaces

This file says what `shoal create` does and how child workspaces and copies
work.

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

`shoal create` creates a workspace as `shoal start` creates one, from the same
arguments that choose what it starts from and where it goes, a repository, an
issue, PR, CI run or branch URL, `--branch`, `--issue`, `--base`, `--copy` and the
creation options, and starts no agent there; `shoal start` starts one later.
It runs the workspace's setup and returns it with its `path`, and from your own
terminal enters it. Inside a space it creates a child, recording the parent, so
work that needs its own PR gets its own branch and worktree; outside one it
creates a top-level workspace. The parent is the space `--parent` names, else
the current one, a workspace or a chat: in a chat, the workspace below it that
contains the current directory, else the chat, so a chat in a workspace's
worktree creates below itself. A child goes in its parent's repository unless
the start names another. A workspace parent must be ready, and a chat without
a repository creates only a `--copy`, in the copied workspace's repository, or
a workspace in the repository it names. `--base` works as for `shoal start`;
naming the parent's branch also [stacks](workspaces.md#stacked-workspaces) the child on it.
Children can have children. A create that names nothing a branch comes from
opens the source picker on a terminal and is refused without one. A space's
processes may create children of their own space, act on every space below it
and `shoal rm` one, or one the user holds or holds a workspace below, as their
space's [`user_holds` permission](modes.md#permitted-actions) allows.
Run inside a workspace or a chat, `shoal start` likewise creates a child of
that space, or of a space below it that `--parent` names, in its repository,
and a chat an agent starts opens in the background; that is the only creation
and removal scope allows, and the rest stays with the user. `shoal start
--fork` starts the agent of the space, a workspace or a chat, that `--parent`
names, else the current one, in the new workspace below it from a fork of the
agent's session, which stays unchanged, with the arguments after `--` as its
task.

`--no-branch` creates a workspace at a commit instead: a worktree at a detached
HEAD with no branch and nothing to push, for comparing with
another commit or reading and running older code without touching the parent's
worktree. `--base` resolves as for a branch and defaults to the refreshed default
branch; a tag or commit that does not resolve makes Shoal fetch origin with its
tags once and try again, and naming the parent's branch starts it at the parent's
commit without stacking it. It is named after the branch at its short commit
(`main at 3f2a9c1`), the tag or the commit, in a directory such as
`main-3f2a9c1` with the usual suffix when taken, and
`shoal diff` compares with its starting commit, which
[`shoal refresh`](branches.md#refresh) moves to a PR's new head or a branch's new tip. Run by a workspace's or a chat's
process it is a child of the caller's space, or of `--parent` when that names
one below it; run by the user it is a child of `--parent`
or the current space, and outside one a top-level workspace of the repository
it names or the current one that the user owns. It does not take `--branch`, `--issue`,
`--agent` or `--fork`, as `create` starts no agent: its owner, a CI job, a
reviewer or the user runs things in it with `shoal run`, `shoal demo` or a
shell, and `shoal start --in` starts an agent of its own there, whose opening
says that without a branch it cannot push or open a PR.
Commands that need a branch, `pr open`, `merge`, `branch rename`
and CI runs on its branch, refuse it, saying it is at a commit; diffs, demos,
ports, simulators, checkpoints, reviews, ready marks, `done` and CI and PR views
work as anywhere. `status` shows `At:` with its commit and what it was made
for where a branch workspace shows `Branch:`; `list --json` and `status --json`
give `at`, with `commit` and `made_for` (`{"ref": name}` or `{"pr": number}`),
in place of `branch`. `{branch}` is empty for it in configured commands and agent
templates, and place names use its name instead.

The parent's agent, a workspace's or a chat's, receives the issue and PR
activity of the spaces below it that have no agent of their own, and of those
below them in turn, through its hooks and an unfiltered or kind-filtered
`watch`; each such update names its space in `workspace`. A child with an agent of its own, one that reports its turn state,
any agent Shoal tracks there or one whose restore waits in the
[agent queue](queue.md), receives its own activity
and its subtree's instead.

A child keeps its parent only from [merged cleanup](removal.md#merged-workspaces): the
parent's idle, completion and PR cleanup follow its own state. Every removal of a parent, `shoal rm`, cleanup or
the cleanup of its deleted directory, first removes the spaces below it,
deepest first, whatever their work, holds or threads: each workspace stops its
agents, and unless it is disposable keeps a checkpoint of its HEAD and unsaved
files, and its branch unless a remote or the default branch has its commits, so
`shoal restore` brings it back, and each chat stops its agent. A child that cannot go retains the parent. `shoal rm` names the children
before asking, and its result names them in `children`; each child's journal
records a `removed` event with the cause `parent`, and the user is notified of
those an unattended removal took. `shoal move` takes the spaces below a
space with it.

While a parent's `done` with cleanup stands at its HEAD, each workspace and
chat below it has `cleanup_reason` `ancestor_completed` and goes after the
grace period, whatever its own work, with the cause `parent_completion`, a
chat with its agent stopped; a hold, a waiting
thread it opened, a pending PR proposal or push request, or queued agent launch of its own keeps it, as do
spaces still below it until they go. `undone`, a commit after `done`, or a
`done` without cleanup leaves the children. A child's own `done` follows the
usual rules. `work` lists children indented below their parent;
`status` shows both directions, and `list --json` and `status --json` give
`parent` (`id`, `name`, `kind`, `workspace` or `chat`, and a workspace's
`branch`, or null) and `children`, the spaces below it, as do chats.

`--agent` starts an agent of its own in the ready child, prompted with the
issue or the arguments after `--`. It goes through the [agent queue](queue.md)
like any launch, but the daemon always starts it, detached in a background
place where queued agents start, such as a Herdr tab or a zmx session, so the
agent gets the child's own scope rather than the caller's; Codex runs as its
CLI. The agent and the program of the first host that would start it are
checked before the child is created, and the launch
reopens only the child, failing if it was removed meanwhile. `--json` adds the
queue entry as `agent`. Since the agent never runs in the caller, `--here` with
an agent is refused.
`--fork` starts the parent's agent there the same way, from a fork of its
session, so the child begins with the parent's conversation and the prompt after
`--`; the parent's session stays unchanged. Run from an agent, it forks that
agent's session, otherwise that of the parent's only running agent, which must
have reported its session through its hooks. The agent copies the session when
it starts, so a queued fork includes what the parent did meanwhile. An agent
without [`fork`](agents.md) arguments, or one an app such as Happy runs,
refuses it before the child is created.
Scoped processes may queue agents only in the workspaces their scope
reaches but their own.

Claude Code launched in a workspace runs `shoal internal worktree-create` and
`worktree-remove` as its `WorktreeCreate` and `WorktreeRemove` hooks, so a
worktree it creates, such as one for a subagent with `isolation: "worktree"`,
is a child of the workspace containing its directory, on a branch named
after the worktree, which it enters once setup finishes. The subagent then works
as that child without an agent of its own, so the child's activity reaches the
agent that started it. Removing the worktree marks the child done; Claude Code
removes none when a subagent finishes, so its child stays until `shoal done` or
merged cleanup.
Claude Code and Codex launched in a workspace also run `shoal internal
subagent-start` as their `SubagentStart` hook, which tells each subagent which
workspace it works in: the child whose directory it starts in, or, starting in
the agent's own workspace as a Codex subagent does with the agent's
instructions, the child its brief names, leaving the agent's own workspace for
the agent to finish.

A space's coordinator is the agent of its nearest ancestor that has one, a
workspace's or a [chat](chats.md)'s. The coordinator receives, as alerts naming
the space, what would otherwise notify the user. That covers the space agent's
alerts from `shoal send`, the threads it opens with them, its completion, and its
ready-for-review marks, which never notify the user otherwise. It also covers
the daemon's reports that the space's agent exited or was stopped. Without a
coordinator, or when its unread alerts are full, the user receives them as
before. A space without an agent of its own is its coordinator's own work, so
what a process does there reaches the user. A
coordinator answers a thread opened below it with `shoal send <S.N> "<answer>"`,
or passes it up with `shoal thread forward <S.N>`.

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
else one picked. `--checkpoint <NUMBER>` starts from that checkpoint of the
`--copy` workspace instead, a live or a removed one's; `shoal restore
<workspace> --checkpoint` lists a workspace's checkpoints, and `shoal restore` names each
removed workspace's removal checkpoint. Both conflict with `--base`, which names
a Git ref and so brings committed work only, even for a branch a workspace has
checked out. Uses include trying another approach from where an agent is now,
debugging a workspace's live state without touching it, and handing the same
state to a second agent.

A copy goes on a new branch at the copied HEAD: the branch given, else the
copied branch's name, or for a workspace at a commit its short HEAD, with `-copy` added,
suffixed as usual when taken. `shoal create --no-branch` makes it a workspace at
a commit instead. A copy of current files is a linked worktree that Git creates
without checking anything out; Shoal copies the source's index and worktree Git
config into it and clones its files (APFS clones on macOS, reflinks on Linux,
plain copies where the filesystem has neither, and a snapshot of each top-level
entry that is a btrfs subvolume holding no other, as a seed output is), so `git status` and `git diff`
match the source's. The source must be ready, its worktree must verify, and no
Git operation may be under way there; an agent running there does not matter,
since copying only reads it. An untracked nested repository or a changed
submodule, whose files a checkpoint cannot hold, is refused, and so is a
checked-out submodule or a worktree inside an ignored directory, whose Git
metadata the copy would share. A copy of a
checkpoint checks out the checkpoint's HEAD and writes its files as uncommitted
changes, without telling staged from unstaged, as `shoal restore` does; a
checkpoint that kept HEAD only is refused. A start the [agent queue](queue.md)
holds creates its copy when it leaves the queue, so it copies the files of that
moment; `--now` copies them at once, and a checkpoint taken first with `shoal
checkpoint` keeps the moment for `--checkpoint`. `shoal start` returns before
the copy is made, by the new workspace's preparation, which checks the
source's files as it copies them; `shoal setup` retries
one that failed, copying the source's files as they are then.

A copy of current files has its source's ignored files instead of the
[seed](setup.md#seed-builds) clone and `setup.copy_ignored`; a copy of a checkpoint, which
holds no ignored files, starts from the seed like any new workspace. The rest of
setup runs as for any workspace. A copy takes its source's diff base (base ref,
base commit and base workspace; a removed workspace's from its removal record),
so `shoal diff` and `shoal conflicts` show the source's work and its own. A copy
of current files keeps the source's worktree Git config, and so its
[Git profile](repo.md#git-profiles), unless `--git-profile` names another; a copy of a
checkpoint gets the profile a new workspace gets. It takes no links, leases,
holds, threads, ready marks or checkpoints, and no agent session: `shoal start
--copy --fork` starts its agent from a fork of the copied workspace's agent's
session, which stays unchanged, and `--fork` does not combine with
`--checkpoint`. Its parent follows the usual rules, not the source. A copy is an
ordinary, writable workspace with one editing agent of its own, which
[`shoal refresh`](branches.md#refresh) copies its live source's files into again.

Once its files are in, a copy takes a checkpoint of itself, its starting
checkpoint, which keeps every unsaved file and does not expire while the copy
exists; a later checkpoint, an agent's included, keeps them whatever their size
too while they still match it. The commits up to its HEAD count as kept, and
files that still match it count as unchanged, so a copy with nothing of its own
is removed by [automatic cleanup](removal.md#automatic-cleanup) like an empty workspace;
commits on top of it and files that differ are the copy's own work. Removal
still deletes the copy's branch only when a remote or the default branch has all
its commits, and otherwise [keeps](removal.md#kept-branches) it. Workspace
processes copy only their own workspace and those below it, never from a removed
workspace's checkpoint. `status` shows `Copied from:` with the workspace, and
`(checkpoint <n>)` for a copy of a checkpoint; `list --json` and `status --json`
give `copied_from` with `workspace` (`id`, `name` and `branch`, null once it is
removed), `checkpoint`, `starting_checkpoint`, that checkpoint's `head` and
`commit`, and `index`, the tree of the index the copy started with.
