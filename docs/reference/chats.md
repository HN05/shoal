# Chats

This file says how chats work, how `shoal convert` changes a space's kind or
agent tool, and what a master session is. [Chats](../design/chats.md) gives
the reasons.

## Start a chat

`shoal start [repository | --in [WORKSPACE]] --mode NAME [--permission KEY=VALUE]... [--issue [ISSUE]] [--parent SPACE] [--agent NAME] [--name NAME] [--here] [-- ARGS]`
starts a chat when the [mode's](modes.md) kind is `chat`, as it is for `chat`
and `plan`. A chat is a conversation with an agent about a repository. The
agent discusses the repository, its issues and plans, or coordinates work. It
changes no code.

Every call starts a new chat. Its repository is the first of these:

- the repository named,
- the issue URL's repository,
- the repository of the current checkout or workspace,
- one a picker offers.

Its agent is `--agent`, else `agent.default`, else one a picker of the
installed agents offers.

The agent's configured launcher runs in the registered checkout, where
`{path}` is the checkout. A launcher that uses a placeholder only a workspace
fills is refused. Shoal trusts the checkout in the agent's config, as it
trusts a workspace's worktree; [launchers and
trust](agents.md#launchers-and-trust) says how.

### Instructions

The agent's instructions are its mode's rules, rendered with `{repo}` and
`{path}`. How they reach the agent depends on its tool:

- A tool with an
  [`instructions` template](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools),
  built in for Claude Code and Codex CLI, takes them apart from the first
  message. The agent waits for you.
- Another tool receives them as its first message, through `{prompt}` or as
  its first argument. The message ends by asking the agent to reply only that
  it is ready and to wait for you.

### Start on an issue

`--issue` assigns the chat an issue. The chat takes the issue's title as its
name without asking. The agent starts on the issue with the mode's
instructions instead of waiting for you. `--issue` without a value offers the
open issues, only those with the mode's `label` where the mode has one.

### Name

The chat's name is `--name`, else `<repo> <mode>`, such as `shoal chat`.
Shoal never asks for one. The agent renames the chat with `shoal rename` once
it knows the work.

A chat's name is a label, as a workspace's is. Chats and workspaces may share
a name, and commands select a chat by its number or ID.
[`shoal rename`](workspaces.md#rename-a-space) renames a chat as it renames a
workspace. The chat's zmx session keeps the name it started with.

### Place

A chat starts focused when its [mode's](modes.md) `focus`, else
`terminal.focus`, says so. With neither set, it starts focused when it opens
on no task. Otherwise it starts in a background tab or a detached session.

Where it runs depends on where you start it:

- Inside a host that opens places beside its own, it opens one labelled with
  its name, such as a Herdr tab. A host whose config keeps launches in the
  current place, as `new_tab = false` does, runs it there.
- On a terminal no host serves, it runs where `terminal.default_host` puts it.
  A zmx session there takes an opaque name, such as `chat-0b9c3a52`, and
  [`shoal attach <number>`](agents.md#attach-to-an-agent) returns to it.
- In any other place of a host, such as a zmx session, it runs in place.
- `--here` runs it in place in this terminal.

Every start, of a chat or a workspace, runs its agent in one of these ways:
here, beside your terminal, in a background place or detached.

With `--json`, a chat whose agent runs outside this terminal reports its
record as `space` and how its agent started as `start`. The `kind` of `start`
is one of these:

- `background`, with the `place` it runs in,
- `queued`, with the `waiting` entries and the agent's queue `entry` beside it,
- `detached`, for an app's session.

### Happy chats

A Happy chat, with `happy-claude` or `happy-codex` as its
[agent](agents.md#happy-sessions), needs no terminal. It starts detached in the checkout, as
`start --in --agent happy-codex` starts a workspace's session, and you
continue it in the Happy app. Happy's Claude takes the instructions as
arguments. Happy's Codex receives them as its first message, the way it
receives prompts. Its log, and what `--json` reports in `start`, are a
workspace session's.

## Scope

A chat's agent carries a scope of its own. The scope lasts as long as the
chat, across stops and restores. Only your own terminal is unscoped.

The agent acts on its chat and the spaces below it as a workspace's agent acts
on its own, by the [scoped workspace commands](modes.md#scoped-workspace-commands)
rules. It runs none of the commands that need a worktree. It also:

- opens workspaces and chats only below its chat and in its repository,
- renames only its own chat,
- decides the requests routed to it only with a grant,
- opens issues only where its [permissions](modes.md#mode-permissions) let it
  open them directly.

Its other permissions decide merging, CI approval, your alerts and
administration. PR verdicts stay with you.

A workspace process that cleared its scope token cannot run `shoal start` or
`shoal rename --request`.

### Where a chat goes

`--parent` names the space the chat goes below, by number or ID.

A workspace's or a chat's agent that starts a chat starts it below its own
space, or below a space under it that `--parent` names. The chat starts in the
background. The command returns once the daemon has recorded the chat and
queued its agent, with the agent's queue entry.

A start without a terminal also runs its chat in the background. The daemon
records these chats and queues their agents as it does for an app, and the
[agent queue](queue.md) opens their place.

`--here` runs the chat in the caller's terminal with the chat's own scope. The
caller's stops leave it running.

## The chat's agent

A chat's agent runs as a tracked agent of the chat, as a workspace's agent
does. Its mode's
[weight](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
decides whether it takes a slot of the [agent queue](queue.md) and whether
load and idle stops stop it. A restore of one stopped between turns waits for
an update, which `shoal attach` and `shoal queue start` skip. Only a clean
exit of the agent differs: it sends no alert.

The daemon records the chat under its name, linked to the issue it starts on,
before its agent runs or its place opens. A detach from its zmx session
therefore names `shoal attach <number>`. What linking warns of reaches the
agent of the nearest space above the chat with one, else your alerts.

The daemon records the agent's session as it records a workspace agent's: the
one the agent's session-start hook last reported.
[`shoal move`](move.md) continues that session on another machine.

### Queued starts

A heavy chat's start waits in the queue when no slot is free, as a new
workspace's does. The daemon records the chat at once, below where it goes,
and the start reports its number with its entry. The queue starts the
agent once a slot is free.

`--now` starts the chat regardless of the limits. A scoped caller's `--now`
works as its space's [`limits` permission](modes.md#permitted-actions)
allows.

A chat whose place fails to start its agent stays, as a new workspace does.
Its [failed start](queue.md#failed-starts) is reported the same way as its link warnings.

### Stop and resume

The chat runs while its agent runs. It stops when the agent exits or
[`shoal stop`](load.md#shoal-stop) stops it, and then waits for
`shoal resume <chat>`. A chat the daemon stopped for a shutdown, a logout or a
signal comes back on its own once the daemon runs again. It comes back where
it ran, or in a background place.

Once a chat's agent stops, the approval requests the chat decided come to you.

A stopped chat keeps its links, files, alerts, threads, workspaces and
`TMPDIR`. `shoal list` shows it as `stopped`.

## Remove a chat

A chat ends only when it is removed:

- `shoal rm <chat>` first removes the spaces below it, then stops its agent.
- [Cleanup](removal.md#automatic-cleanup) removes it as it removes a
  workspace.

## Spaces below a chat

`shoal list` shows the chat by name, with the spaces below it.
[Space events](log.md#space-events) report its start, renames, stops,
restores and removal. Its `parent` and `children` fields name the spaces above
and below it.

A workspace the chat's agent opens with `shoal start` goes below the chat. One
created below a workspace stays there. The workspace's `parent` field and
`shoal status` name the chat until the workspace is reopened outside a chat
for its issue or a picked branch, which returns it to you. A start of its own
agent, with `--in` or with the `--branch` it has, leaves it where it is.

The chat opens workspaces only in its own repository. One in another
repository is refused.

With `--agent`, the workspace's agent queues once the workspace is set up. The
daemon starts it with the workspace's own scope when a slot is free, as for
any agent a workspace process starts. An agent that `--add-blocker` or the
issue's open blockers block queues with its workspace. The workspace opens
below the chat once they clear. `--now` and `--focus` are refused there.

### Coordination

While the chat's agent runs, or waits to come back on its own, the chat
[coordinates](create.md#coordinators) the spaces below it. It receives the activity of
those without an agent of their own.

While the chat is stopped otherwise, their events go on up, to you at the top.
When it is removed before receiving their events, you receive the alerts they
stand for.

## Chat in a workspace's worktree

`shoal start --in [WORKSPACE]` in a mode whose kind is `chat`, such as
`-m chat`, starts the chat in a ready workspace's worktree instead of the
checkout. The chat debugs, audits or reviews the workspace's work, committed
and uncommitted, whatever the workspace's own agent is doing. In a mode that
starts in a workspace, `--in` starts [the workspace's own agent](agents.md#start-a-workspaces-own-agent)
instead.

The workspace is the one named, else the current one, else one a picker of
the ready workspaces offers. A workspace that is not ready, or whose worktree
is missing, is refused. The chat's repository is the workspace's, and its
default name is `<workspace name> chat`.

### What the chat is

The chat is not the workspace's agent. It runs as any chat does, with its own
identity, scope, `TMPDIR` and alerts. It has none of the workspace's scope,
`[env]` or leases.

The chat is not below the workspace. It goes below the space of the agent that
started it, if any. Such an agent names only its own workspace or one below
it.

Its scope also reads the workspace, such as its status, diff, links and CI
runs, and refuses changes to it.

Its facts describe a chat in a worktree. They name the workspace, its branch
or commit and its path. They tell the agent that the workspace's own agent may
be editing there and makes the code changes.

The chat's record, and `--json`, name the `workspace`. `shoal resume` restores
the chat there.

### Commands in the chat

Commands that act only on workspaces act on the workspace, such as
`shoal diff`, `shoal conflicts` and `shoal branch base`. `shoal status` takes
the current directory's workspace before the chat, so it acts on the workspace
too.

Commands that act on chats act on the chat, such as `shoal view`, `shoal ci`,
`shoal link`, `shoal ready` and `shoal done`. `--space <workspace>` reads the
workspace's items instead, as `shoal view --space` and `shoal ci ls --space`
do.

### The workspace's lifetime

A running chat keeps the workspace from idle, merged, PR and completion
cleanup. A chat Shoal saved keeps it from idle cleanup.

Removing the workspace removes its chats first and stops their agents. This
includes the cleanup of a worktree deleted outside Shoal, which nothing keeps.
`shoal rm` lists the chats before asking.

A chat never moves to another directory. A chat about another workspace is a
new chat.

## Convert a chat or a workspace

```sh
shoal convert workspace fix-login        # This chat becomes a workspace on fix-login
shoal convert workspace --issue 42       # On the branch the issue names, which it links
shoal convert workspace                  # On the branch the chat's linked issue names
shoal convert chat                       # This workspace becomes a chat in the checkout
shoal convert chat --space 7 --mode plan # The user's terminal converts any space
shoal convert agent codex                # Codex takes this space's work over
shoal convert agent codex -- "Focus on the failing test"
shoal convert chat -a codex              # This workspace becomes a chat Codex works in
```

`shoal convert workspace [BRANCH] [--issue N] [--base REF] [--skip-setup] [--mode MODE] [--space S]`
turns a chat into a workspace. `shoal convert chat [--mode MODE] [--space S]`
turns a workspace into a chat. Both take the [agent
options](#hand-a-space-to-another-agent-tool) of `shoal convert agent`.
Converting a space into the kind it already is, is refused.

The space stays the same. It keeps these:

- its ID, number, name, parent and children,
- its links and watched PRs, threads, attached files, progress and `TMPDIR`,
- the places Shoal opened for it, such as the tab or session its agent runs
  in.

The [journal](log.md#space-events) records a `converted` event instead of a
new space. Unread alerts go to the agent in the new place.

A space's agent converts the spaces its scope reaches. Your terminal converts
any space: the one `--space` names by number or ID, else the current
workspace, else one a picker offers.

### Chat to workspace

`convert workspace` takes its branch from the first of these:

- `BRANCH`: the local or remote branch of that name, which no workspace may
  hold, or a new one from the default branch or `--base`,
- the issue `--issue` names, which the workspace links,
- the chat's linked issue.

An issue names the branch as [`shoal start --issue`](workspaces.md) derives
it. A chat with neither a branch nor an issue is refused.

The worktree, its directory and its setup are a new workspace's. The command
follows setup as `shoal create` does. It returns once the workspace is ready,
or with its setup failure. After a setup failure, the agent runs on in the
chat's place until [`shoal setup`](setup.md) repairs the setup. A worktree
that cannot be made gives the chat back.

The workspace's mode is `--mode`, `worker` by default. Its permissions are
that mode's. Each `--permission` given to the chat's agent still applies over
them.

These chats are refused:

- a chat in a workspace's worktree (`shoal start --in`),
- a chat without a repository,
- a stopped chat.

### Workspace to chat

`convert chat` removes the worktree as [`shoal rm`](removal.md) does, with its
removal hooks. A [checkpoint](removal.md#checkpoints) keeps HEAD
and the unsaved files first, so the command asks nothing. A workspace whose
unsaved files no checkpoint can keep is refused.

The branch stays, as a [kept branch](removal.md#kept-branches).
`shoal restore` brings the files back in a new workspace.

What the workspace held goes this way:

- Demos stop.
- Ports, resources, simulators and secrets are released.
- Agents' holds stay with the space.
- Pending PR proposals and pushes go with the worktree.
- Chats in the worktree are removed.

These refuse it:

- a hold the user placed on the workspace,
- another workspace that builds on its branch (`shoal branch base`),
- no agent running there to carry on.

The chat runs in the registered checkout, in `--mode`'s mode, `chat` by
default.

### The agent in the new place

`convert chat` returns at once. Once the agent's current turn ends, as its
hooks report, Shoal stops it where it ran. Shoal then starts it in the new
place from a fork of its session, as `shoal start --fork` starts one, so it
carries on with its conversation:

- A chat that became a workspace resumes in the worktree, as `shoal resume`
  continues a last session.
- A workspace that became a chat restores in the checkout as a stopped chat
  does. It restores where the agent ran when the terminal host still has that
  place, else in a background place.

Its first message says what the space is now, with its path, and with its
branch for a workspace. The mode's instructions follow.

A restart that fails leaves the agent for `shoal resume` and records a
`recovery_blocked` alert.

A stop of the agent before then, such as `shoal stop`, keeps it down until it
is resumed. The space still converts:

- A workspace that becomes a chat stays a stopped chat.
- A chat's agent keeps its session as the workspace's last session.

No new agent starts in a converting space until its agent has moved:

- `shoal resume` of a workspace that becomes a chat is refused until it is the
  chat. `shoal resume` then continues the chat.
- Another agent's start in a chat's new workspace is refused until the chat's
  agent runs there.

### Hand a space to another agent tool

`shoal convert agent AGENT [--mode MODE] [--model M] [--effort E] [--permission KEY=VALUE]... [--space S] [-- NOTE]`
hands a space's work over to another agent tool, such as from Claude Code to
Codex when a usage limit hits. `-a/--agent AGENT` on `convert workspace` and
`convert chat` hands it over with the kind change. Every kind of space takes
it, including a chat in a workspace's worktree and one without a repository.

Shoal checks the tool as `shoal start` checks it, before anything changes.
`convert agent` refuses the space's own tool. On the kind conversions, the
space's own tool continues from a fork of its session, as without `-a`.

A running agent hands over once its turn ends, as a conversion restarts one. A
stopped or asleep agent hands over at once, also with the kind change. A
conversion that keeps the tool refuses a stopped or asleep agent.

#### Sessions the new tool reads

Shoal never converts one tool's session into another's. It copies every
session the space's agents reported into the space's
[attached files](files.md) as `handoff` copies, whatever their size: every
file `transcripts` lists for it. A session an earlier handoff copied is copied
again only once its files changed.

The new tool starts a new session, even when the space used that tool before. Its first
message says:

- what the space is, with its path, its branch and its issue,
- that it continues work another tool started,
- each copied session, oldest first, with its tool, its session ID and the
  path of its copy.

The list marks the session just stopped to read first. It marks as not copied
a session whose files are not on this machine, or whose tool Shoal does not
know. The mode's instructions follow, then `NOTE`, which only a new tool
takes.

#### Mode and options

The new agent runs in `--mode`, else the space's mode. `--model` and
`--effort` apply over the configured ones. `--permission` changes what the
earlier agent was given, as `shoal resume --permission` does. On the kind
conversions, these options apply to a forked agent too.

A stop before the turn ends keeps the new tool's agent stopped for
`shoal resume`, as it keeps a converting agent down.

The [journal](log.md#space-events) records the handoff. `shoal status --json`
lists the session the handoff started in `transcripts`, with the session it
`continued_from`.

## Master session

`shoal start --mode orchestrate [--parent SPACE] [--agent NAME] [--name NAME] [--here] [-- ARGS]`
starts a chat without a repository: a master session, whose agent manages the
spaces of every registered repository. Any mode with `repository = false`
starts its chat the same way. A repository, `--repo`, `--issue` or `--in` is
refused.

The chat runs in `root_dir`, with its `TMPDIR` in `root_dir`'s `.tmp`. Its
settings come from the global config alone. The agent waits for you to speak
first, as in any chat.

`shoal list` and `--json` list it with a null `repository_id` and the
`root_dir` it started in as its `workdir`. It resumes there and keeps its
`TMPDIR` there when `root_dir` changes later. It stops, resumes and is
removed like any other chat. Removing it first removes every space below it,
in every repository.

### Spaces below a master session

`shoal start` run from a master session starts workspaces and chats below it,
in any registered repository. Their alerts, threads, completion, approval
requests and activity reach it first, as they reach any parent. Spaces you
start stay top-level and report to you. Several master sessions may run at
once.

A space without a repository is the only parent of a space in another
repository.

### What a master session reaches

Its scope reaches every space on the machine, under the
[scoped workspace commands](modes.md#scoped-workspace-commands) rules. It:

- lists and reads them,
- sends their agents alerts, opens threads with them and answers theirs,
- holds them,
- stops and queues their agents, and resumes the ones it or the spaces below
  it stopped,
- removes those you hold nothing in.

It decides only the requests routed to it, with a grant. Merging, PR
verdicts, CI approval and your alerts stay with you.

### Repositories

Every registered repository counts as its own. `--repo`, or an issue or PR
URL, names the repository a command acts in. There it:

- reads, opens, comments on, labels and edits issues,
- comments on PRs,
- lists CI runs, and reruns or cancels a PR's checks,
- watches issues, PRs and runs,
- sends forge API requests.

It does these under its [permissions](modes.md#mode-permissions) as that
repository's config resolves them. Its proposals go to you as that
repository's. Through `--space` it acts on a space's items as a coordinator
does. It opens no PRs and links nothing.

In a repository that is not registered, it only opens an issue, named by its
URL. Its `issue_other_repositories` permission, resolved in the global config,
must be `"direct"`. It reads such a repository only through a space whose own
repository reads it.

### Attached files

Its attached files stay in the `.attachments` of the `root_dir` it started
in, also when `root_dir` changes later. The daemon marks that directory with
its state directory, as it marks `.tmp`.
[`shoal repo move`](repo.md#move-repositories-to-a-new-root) moves both with
the repositories. [`shoal move`](move.md) refuses a master session.
