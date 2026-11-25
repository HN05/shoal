# Chats

This file says how chats work, how `shoal convert` changes a space's kind or
agent tool, and what a master session is.

`shoal start [repository | --in [WORKSPACE]] --mode NAME [--permission KEY=VALUE]... [--issue [ISSUE]] [--parent SPACE] [--agent NAME] [--name NAME] [--here] [-- ARGS]`
in a [mode](modes.md) whose kind is `chat`, such as `chat` or `plan`,
starts a new chat with an agent about a repository, to discuss it, its issues
and plans, or to coordinate work, without changing code. The
repository is the named one, else the issue URL's, else the current
checkout's or workspace's, else a picker. The
agent is `--agent`, else `agent.default`, else a picker of the installed ones.
The agent's configured launcher runs in the registered checkout with the mode's
rendered rules as its instructions. With `--issue`, the chat is assigned the
issue and named after its title without asking, and the agent starts
on it with the mode's instructions instead of waiting for you; without a value,
`--issue` offers the open issues, those carrying the mode's `label` where it
has one. An agent with an
`instructions` template (built in for Claude Code and Codex CLI) takes them
apart from its first message and waits for yours; another receives them as its
first message, through `{prompt}` or as its first argument, ending with a
request to reply only that it is ready and wait for you. The template substitutes `{repo}` and `{path}`; in the launcher,
`{path}` is the checkout, and a launcher using a placeholder only a workspace has a value for is refused. The checkout is trusted in the agent's
config as workspace launches trust theirs.

Every call starts a new chat, focused when its [mode's](modes.md)
`focus`, else `terminal.focus`, says so, or when neither is set and it opens
on no task; otherwise it starts in a background tab or detached session. Its
name is `--name`, else the default `<repo> <mode>`, such as `shoal chat`,
without asking; names may repeat, and the agent renames it with `shoal rename`
once it knows the work. Inside a host that opens places beside its own it
opens one labelled with the name, such as a Herdr tab, unless that host's
config keeps it in the current place, as `new_tab = false` does; on a terminal
no host serves it runs where `terminal.default_host` puts it, such as a zmx
session with an opaque name such as `chat-0b9c3a52`, since nothing is built
from a chat's name, which [`shoal attach <number>`](agents.md)
returns to; in any other place of a host, such as a zmx session, it runs in
place. `--here` runs it in place in this terminal. With `--json`, a chat whose
agent runs outside this terminal is reported with its record as `space` and
how its agent started as `start`, whose `kind` is `background` with the
`place` it runs in, `queued` with the entries `waiting` and the agent's queue
`entry` beside it, or `detached` for an app's session. A Happy chat
(`happy-claude` or `happy-codex`, see [Agents](agents.md)) needs no terminal: it
starts detached in the checkout as `start --in --agent happy-codex` starts a workspace session, and
you continue it in the Happy app. Happy's Claude takes the instructions as
arguments, and Happy's Codex receives them as its first message the way its
prompts are delivered. Its output is kept in a log under Shoal's state, as a
workspace session's is, whose path it reports; `--json` reports the `log`, `pid`, `happy_session_id`
and `happy_daemon_recorded` in `start`. A chat's agent carries a scope of its own, which
lasts as long as the chat, across stops and restores, so only your own terminal
is unscoped. It acts on the chat and the spaces below it as a workspace's agent
acts on its own, under the [scoped workspace commands](modes.md#scoped-workspace-commands)
rules, without those that need a worktree. It opens workspaces and chats only
below the chat and in its repository, renames only its own chat, decides the requests
routed to it only with a grant, and opens issues only where its
[permissions](modes.md#mode-permissions) let it open them directly; its other
permissions decide merging, CI approval, your alerts and
administration, and PR verdicts stay with you. `--parent`
names the space the chat goes below, by number or ID; a workspace's or a
chat's agent that starts a chat starts it below its own space, or below one
under it that `--parent` names, in the background, and returns once the
daemon has recorded the chat and queued its agent, with its entry. A start
without a terminal also runs its chat in the background. The daemon records
these chats and queues their agents as it does for an app, and the agent
queue opens their place. `--here` runs the chat in
the caller's terminal with the chat's own scope, and the caller's stops leave
it running. Every start, of a chat or a workspace, runs its agent the same
way: here, beside your terminal, in a background place or detached. A
workspace process that cleared its scope token cannot run `shoal start` or
`shoal rename --request`.
A chat's agent runs as a tracked agent of the chat, as a workspace's agent
does: its mode's [weight](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules) decides whether it takes
a slot of the [agent queue](queue.md) and whether load and idle stops
stop it, and a restore of one stopped between turns waits for an
update, which `shoal attach` and `shoal queue start` skip. Only its agent
exiting cleanly differs: it sends no alert. A heavy chat's start
waits in the queue when no slot is free, as a new workspace's does: the
daemon records the chat at once, below where it goes, the start reports its
number with its entry, and the queue starts the recorded chat's agent once a
slot is free; `--now` starts the chat regardless of the
limits, a scoped caller's as its space's
[`limits` permission](modes.md#permitted-actions) allows. The
daemon records each chat under its name, linked to the issue it starts on,
before its agent runs or its place opens, so a detach from its zmx session
names `shoal attach <number>`: what linking warns of reaches the agent of the nearest
space above the chat with one, else your alerts, and a chat whose
place fails to start its agent stays, as a new workspace does, with its
[failed start](queue.md) told the same way. It
records its agent's session as a
workspace agent's, the one the agent's session-start hook last reported, which
[`shoal move`](move.md) continues on another machine.
The chat runs while its agent runs, and stops when it exits or
[`shoal stop`](daemon.md#recovery) stops it: it waits for `shoal resume <chat>`, while
one the daemon stopped for a shutdown, a logout or a signal comes back on its
own once the daemon runs again, where it ran or in a background place. Once a
chat's agent stops, the approval requests the chat decided come to you. A
stopped chat keeps its links, files, alerts, threads, workspaces and
`TMPDIR`, and `shoal list` shows it as `stopped`. A chat ends only when it is
removed: by `shoal rm <chat>`, which first removes the spaces below it and
stops its agent, or by [cleanup](removal.md#automatic-cleanup), as a workspace is.
A chat's name is a label like a workspace's: chats and workspaces may share one,
and commands select a chat by its number or ID.

`shoal start --in [WORKSPACE]` in a mode whose kind is `chat`, such as
`-m chat`, starts the chat in a ready workspace's
worktree instead of the checkout, to debug, audit or review its work, committed
and uncommitted, whatever the workspace's own agent is doing: the named
workspace, else the current one, else a picker of the ready workspaces; in a
mode that starts in a workspace, it starts [the workspace's own
agent](agents.md) instead. The repository is the workspace's, the default name
`<workspace name> chat`, and a workspace that is not ready or whose worktree is
missing is refused. The chat is not the workspace's agent: it runs as any chat
does, with its own identity, scope, `TMPDIR` and alerts, without the
workspace's scope, `[env]` or leases, and is not below the workspace, but
below the space of the agent that started it, if any; such an agent names only
its own workspace or one below it. Its
scope also reads the workspace, such as its status, diff, links and CI runs,
but refuses changes to it. Its facts
describe a chat in a worktree instead of the checkout: they name the workspace,
its branch or commit and its path, and tell the agent that the workspace's own
agent may be editing there and makes its code changes. Its record, and
`--json`, name the `workspace`, and `shoal resume` restores it there. In the
chat, commands that act only on workspaces, such as `shoal diff`,
`shoal conflicts` and `shoal branch base`, act on the workspace, and so does
`shoal status`, which takes the current directory's workspace before the chat;
commands that act on chats, such as `shoal view`, `shoal ci`, `shoal link`,
`shoal ready` and `shoal done`, act on the chat, and `--space <workspace>`
reads the workspace's, as `shoal view --space` and `shoal ci ls --space` do. A running chat keeps the workspace from idle, merged,
PR and completion cleanup, and one Shoal saved keeps it from idle cleanup.
Removing the workspace, including the cleanup of a worktree deleted outside
Shoal, which nothing keeps, removes its chats first and stops their agents;
`shoal rm` lists them before asking. A chat
never moves to another directory: one about another workspace is a new chat.

[`shoal rename`](workspaces.md#rename-a-space) renames a chat as it renames a
workspace. The chat's zmx session keeps the name it started with, since
`shoal attach` finds the session where the chat runs.

`shoal list` shows the chat by name with the spaces below it, and
[space events](log.md#space-events) report its start, renames, stops,
restores and removal. A workspace the chat's agent opens with `shoal start` is
below the chat, unless it was created below a workspace, where it stays: its `parent` field and `shoal status` name the chat until the
workspace is reopened outside a chat for its issue or a picked branch, which
returns it to you; a start of its own agent, with `--in` or the `--branch` it
has, leaves it where it is. The chat opens
workspaces only in its own repository; one in another repository is refused.
With `--agent`, the agent is queued once the workspace is set up, and the
daemon starts it with the workspace's own scope when a slot is free, as for
any agent a workspace process starts. An agent that `--add-blocker` or the
issue's open blockers block queues with its workspace, which opens below the
chat once they clear; `--now` and `--focus` are refused. While it runs, or waits to come back on its own, the chat
[coordinates](create.md) the spaces below it and receives the activity
of those without an agent of their own; while it is otherwise stopped, their
events go on up, to you at the top, and one removed before receiving their
events gives you the alerts they stand for. The chat's `parent` and
`children` fields name the spaces above and below it.

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
turns a chat into a workspace, and `shoal convert chat [--mode MODE] [--space S]`
turns a workspace into a chat; both take the [agent options](#hand-a-space-to-another-agent-tool)
of `shoal convert agent`. The space stays the same: it keeps its ID,
number, name, parent, children, links and watched PRs, threads, attached
files, progress, `TMPDIR` and the places Shoal opened for it, such as the
tab or session its agent runs in, and the [journal](log.md#space-events) records a
`converted` event instead of a new space. Unread alerts go to the agent in
the new place. Converting a space into the kind it already is, is refused.

`convert workspace` takes a branch, `--issue` and `--base`. With `BRANCH`, the
workspace uses the local or remote branch of that name, which no workspace may
hold, or creates it from the default branch or `--base`; without one, the issue
`--issue` names, which it links, or else the chat's linked issue names the
branch as [`shoal start --issue`](workspaces.md) derives it, and a chat without
either is refused. The worktree, its directory and setup are a new
workspace's: the command follows setup as `shoal create` does and returns once the
workspace is ready, or with its setup failure, after which the agent runs on in
the chat's place until [`shoal setup`](setup.md) repairs the
setup. A worktree that cannot be made gives the chat back. The workspace's mode and permissions are `--mode`'s,
`worker` by default, with the permissions given to the chat's agent. A chat
in a workspace's worktree (`shoal start --in`), one without a repository and a
stopped chat are refused.

`convert chat` removes the worktree as [`shoal rm`](removal.md) does,
with its removal hooks, once a [checkpoint](removal.md#checkpoints) keeps its
HEAD and unsaved files, so it asks nothing; one whose unsaved files no
checkpoint can keep is refused. The branch stays, as a [kept
branch](removal.md#kept-branches), and `shoal restore` brings the files back in a new
workspace. Demos stop, and ports, resources, simulators and secrets are
released; agents' holds stay with the space, pending PR proposals and pushes
go with the worktree, and chats in it are removed. It is refused while a hold the user
placed keeps the workspace, while another workspace builds on its branch
(`shoal branch base`), and while no agent runs there to carry on. The chat runs
in the registered checkout, in `--mode`'s mode, `chat` by default.

The command returns at once for a workspace that becomes a chat. Once the
agent's current turn ends, as its hooks report, Shoal stops it where it ran and
starts it in the new place from a fork of its session, as `shoal start --fork`
starts one, so it carries on with its conversation: a chat that became a
workspace resumes in the worktree as `shoal resume` continues a last session,
and a workspace that became a chat restores in the checkout as a stopped chat
does, where the agent ran when the terminal host still has that place, else in
a background place. Its first message says what the space is now, with its path,
and its branch for a workspace, followed by the mode's instructions. A restart
that fails leaves the agent for `shoal resume` and records a `recovery_blocked`
alert. A stop of the agent before then, such as `shoal stop`, keeps it
down until it is resumed: the space still
converts, a workspace becoming a chat that stays stopped, and a chat's agent
keeping its session as the workspace's last session. No new agent starts in
a converting space's workspace until its agent has moved: `shoal resume` of a
workspace that becomes a chat is refused until it is the chat, which
`shoal resume` then continues, and another agent's start in a chat's new
workspace is refused until the chat's agent runs there. A space's agent converts the spaces its scope reaches; your terminal
converts any space, by `--space` with its number or ID, or the current
workspace, else a picker.

### Hand a space to another agent tool

`shoal convert agent AGENT [--mode MODE] [--model M] [--effort E] [--permission KEY=VALUE]... [--space S] [-- NOTE]`
hands a space's work over to another agent tool, such as from Claude Code to
Codex when a usage limit hits, and `-a/--agent AGENT` on `convert workspace`
and `convert chat` does it with the kind change. Every kind of space takes
it, a chat in a workspace's worktree and one without a repository too. The
tool is checked as `shoal start` checks it before anything changes; `convert
agent` refuses the space's own tool, which on the kind conversions continues
from a fork of its session as without `-a`. A running agent hands over once
its turn ends, as a conversion restarts one; a stopped or asleep one hands
over at once, also with the kind change, which a conversion that keeps the
tool refuses.

Shoal never converts one tool's session into another's. It copies every
session the space's agents reported, every file `transcripts` lists for it,
into the space's [attached files](files.md) as `handoff` copies,
whatever their size; a session an earlier handoff copied is copied again only
once its files changed. The new tool starts a new session, also one the space
used before. Its first message says what the space is, with its path, its
branch and its issue, that it continues work another tool started, and lists
each copied session, oldest first, with its tool, its session ID and the path
of its copy, marking the one just stopped to read first and a session whose
files are not on this machine, or whose tool Shoal does not know, as not
copied; then its mode's instructions, then `NOTE`, which only a new tool
takes. It runs in `--mode`, else the space's mode, with `--model` and
`--effort` over the configured ones and `--permission` changing what the
earlier agent was given, as `shoal resume --permission` does; on the kind
conversions they apply to a forked agent too. A stop before the turn ends
keeps the new tool's agent stopped for `shoal resume`, as it keeps a
converting agent down. The [journal](log.md#space-events) records the handoff, and
`shoal status --json` lists the session it started in `transcripts` with the
one it `continued_from`.

## Master session

`shoal start --mode orchestrate [--parent SPACE] [--agent NAME] [--name NAME] [--here] [-- ARGS]`
starts a chat without a repository, a master session whose agent manages the
spaces of every registered repository; any mode with `repository = false`
starts its chat the same way. It runs in `root_dir`, with its `TMPDIR` in
`root_dir`'s `.tmp`, and its settings come from the global config alone; a
repository, `--repo`, `--issue` or `--in` is refused. The agent waits for you
to speak first, as in any chat. `shoal list` and `--json` list it with a null
`repository_id` and the `root_dir` it started in as its `workdir`, where it resumes and keeps its
`TMPDIR` however `root_dir` changes later, and it is stopped, resumed and removed
like any other chat; removing it first removes every space below it, in every
repository.

`shoal start` run from it starts workspaces and chats below it in any
registered repository, whose alerts, threads, completion, approval
requests and activity reach it first, as they reach any parent; spaces you
start stay top-level and report to you, and several master sessions may run at
once. A space without a repository is the only parent of a space in another
repository. Its scope reaches every space on the machine: it lists and reads
them, sends their agents alerts, opens threads with them and answers theirs, holds them, stops and
queues their agents, resumes the ones it or the spaces below it stopped, and
removes those you hold nothing in, under the
[scoped workspace commands](modes.md#scoped-workspace-commands) rules. It decides only
the requests routed to it, with a grant; merging, PR verdicts, CI approval and
your alerts stay with you. Every registered repository counts as its
own: `--repo` or an issue or PR URL names the one a command acts in, and
there it reads, opens, comments on, labels and edits issues, comments on PRs,
lists CI runs and reruns or cancels a PR's checks, watches issues, PRs and runs,
and sends forge API requests under its [permissions](modes.md#mode-permissions) as that repository's config resolves them, and
its proposals go to you as that repository's; through `--space` it acts on a
space's items as a coordinator does. It opens no PRs and links nothing. In
a repository that is not registered it only opens an issue, named by its
URL, where its `issue_other_repositories` permission, resolved in the global
config, is `"direct"`, since a proposal belongs to a registered repository; it reads
such a repository only through a space whose own repository reads it. Its attached files stay in
the `.attachments` of the `root_dir` it started in, however `root_dir` changes
later, which the daemon marks with its state directory as it does `.tmp`, and [`shoal move`](move.md) refuses it.
