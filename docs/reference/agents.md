# Agents

This file says how Shoal runs AI tools as agents, with their accounts, secrets,
state and terminal status.

Every `shoal start` starts an agent: the one `-a`/`--agent` names, else
`agent.default`, else one picked on a terminal, whose picker offers only
agents; with `agent.default` set, no picker opens. A start that can do none of
these is refused before anything is created; `shoal create` makes a workspace
without an agent. `--model` and `--effort` apply to whichever agent starts.
`start --agent <name>` starts the agent after
worktree creation, setup, and
the post-setup hook succeed; arguments after `--` go to the agent. Shoal refuses
an agent whose executable is not on PATH before creating anything, and leaves
it and its shortcut command out of the agent picker, the workspace menu and tab
completion. CLI agents
run through the tracked execution wrapper and, when run in place, return the
agent's exit code, restoring OS terminal settings even after interruption. When
`TERM` is nonempty and not `dumb`, it also resets emulator input modes for the shell;
the workspace is retained even when launch fails. With shell
integration, your shell enters the new workspace after an agent run in place
exits; an agent in a zmx session leaves it where it was.
`--json` emits the workspace record first, then the agent's unmodified output,
or for an agent that starts detached its queue entry.

`shoal start --in [WORKSPACE]` in a mode that starts in a workspace, `worker`
without a terminal, starts the agent of that existing, ready workspace instead
of a new space: the named workspace, by number, name or ID, else the current
one, else a picker. Nothing is created: the workspace keeps its place in the
tree, its name and its links, whose activity reaches the new agent, and the
start reports its record and the agent's queue entry as a new workspace's
does. Arguments after `--` go to the agent as for any start; without them it
starts on the workspace's issue, followed by the mode's instructions, and
without an issue waits for you. It sees the branch's commits and continues from
them. The flags that start, queue and
place an agent apply as to any start, while one that names what a new space
starts as or on, such as `--parent`, `--name`, a repository or `--issue`, is
refused; the issue's names `shoal link issue <number> assigned --space
<workspace>`. A workspace has one such agent, which the [agent
queue](queue.md) enforces, and a workspace's agent starts this way only the
agent of a workspace below its own space. The workspace menu's agent choice
runs `shoal start --in <workspace> --mode worker --agent <name> --here`.

On a terminal, CLI agent launches (`start --agent` and `resume`) run in a [zmx](https://github.com/neurosnap/zmx)
session, so the agent keeps running when you detach with Ctrl-\ or close the
terminal. zmx passes the agent's output to your terminal unchanged, so images and
other terminal protocols keep working, and restores the screen when you attach
again. The tracked wrapper runs inside the session, so stop, resume, scope and
cleanup treat the agent as before. When the session returns control, Shoal resets
emulator input modes on the caller's terminal. zmx reports no exit status: these launches
exit 0 once the agent exits or you detach. A launch inside a
zmx session (`ZMX_SESSION` is set), or in a pane or block of another enabled host, such
as a Herdr pane with `terminal.herdr.enabled`, runs in place. `resume`, and scoped callers and callers
without a terminal, such as another agent's tool call, of `start` and `contribute`, start the agents detached
through the daemon instead (see [Agent queue](queue.md) and `shoal resume`),
so the call returns without running the agent and interrupting it leaves the
agent running. An agent Happy runs leaves the call at once anyway, so a
caller outside Shoal launches it as before, as its repository's settings
decide. `start --here` and `contribute --here` run the agent in
place, returning its exit code, and for callers outside Shoal so does `resume --focus`.
A scoped caller's `--here` runs the agent here once its workspace is prepared
and a slot is free: with none, it leaves the agent queued and returns, rather
than skipping the limits. Its agent is its own space's, with that space's scope: the caller's stops leave it running. A launch that would start a zmx session refuses the agent when zmx is missing,
as a missing agent executable does, or older than 1.0.1. A zmx reaches only
sessions a zmx of its own version started: before upgrading from 0.8 or
earlier, stop the agents in zmx sessions with `shoal stop`, and resume them
after with `shoal resume`. Sessions an older zmx started keep running but are
unreachable; Shoal reports them where it reads them, in `shoal status` and its
cleanup alerts, until their processes end, which `shoal stop` does for
an agent's.
`terminal.default_host` (`"zmx"` by default) sends these launches to another host
instead, as each host's section below describes. With that host turned off they run in place, and a queued
start or a demo that no other host takes fails.
`-H`/`--here` on `shoal start`, `contribute`, `repo setup` and `config setup`
runs the launch in this terminal instead of a new tab or detached session,
whatever the host. An agent that `start` or `contribute` then starts runs as
these launches do, such as in a zmx session attached here on a terminal; a chat runs in
place.

Interactive `shoal start` on a terminal no host serves, with zmx as
`terminal.default_host`, places its launch as a tab would be: an agent that starts with input runs setup and the
agent in a detached zmx session and returns with a `shoal attach` hint, and any
other launch runs here and attaches its agent's session. `terminal.focus`
overrides the choice, as it does for tabs, and `--here` keeps the launch
in this terminal. A detached session ends with its agent, whatever its exit
status; one whose start fails opens your login shell (`$SHELL -l`) there to show
why, and a workspace `shoal create` opens there gets its shell, as a tab does.
When `done` keeps the workspace, a session Shoal starts for `shoal start`, an
attached agent or a background `shoal resume` ends after the agent's final turn,
unless `[terminal] close_when_done = false` keeps it open.

`shoal attach [target]` reattaches to the session of the workspace or
[chat](chats.md) named `target`, with a picker or `--session <name>` when a
workspace has several; while a session shows its agent, the picker offers
only those that do. The picker and `shoal status` list each with the agent
the daemon recorded running there and mark it `attached` while a terminal shows
it: a zmx client, a Rex app with the block's session open, or, since Herdr does
not count its clients, the focused Herdr tab. Without a target it takes the
current workspace, else the only running chat of the current checkout, else a
picker of the workspaces and the chats with sessions or whose agent is
[asleep](sleep.md) or stopped, each with its status. An agent or chat running in
a Herdr pane counts as a session named after its tab: attaching focuses the
tab and, from a terminal outside Herdr, attaches a Herdr client to the tab's
Herdr session, and inside another Herdr session it is refused. A chat running
in the Happy app or in a terminal no host serves has no session, and attaching says
where it runs instead. Attach finds a session where the daemon recorded the
workspace's agent or the chat running, or one Shoal opened for the workspace,
such as a detached session left at its shell after a failed setup. Attaching
wakes an agent at once, whatever the agent limits and the queue's rank, and
attaches once it runs: the session of an agent that sleeps shows only the stop
while its wrapper waits, the pickers list that session as stopped, and
attaching to it starts its restore there, as `shoal queue start --attach`
does. Without a session showing an agent, attaching starts the restore an
asleep agent waits for, and without any session left it waits for the agent
a launch is starting there, or else resumes the agent as `shoal resume
--now` does, refusing a space with only stopped commands. Neither starts a second agent beside one whose wrapper waits. A session
whose agent stopped without a restore in the queue says why the agent stopped
and what restores it. A session that ends while its agent moves to a new
place, as a [conversion](chats.md#convert-a-chat-or-a-workspace) moves it, such as a
zmx session, which ends with its agent, keeps you attached: the attach waits
for the agent to run in its next session and attaches there.

`shoal attach <target>` of a space another machine of the hub runs, by its
number or ID, has that machine run the same attach as you, on a terminal of
your terminal's size and `TERM`: it wakes or restores the agent, picks a
session there, `--session` naming one, and draws its picker on that terminal,
which yours shows in raw mode. Every key goes there, so the host's own detach
key, such as zmx's Ctrl-\, detaches, and a resize follows. A zmx, tmux,
Zellij or Herdr session takes a client of its host on that terminal, which
switches a tmux, Zellij or Herdr session's window or tab for its other
clients as an attach from a terminal outside it does. A Rex block is selected
in your own Rex app, which connects to that machine's Rex server under the
label your machine's Rex reports, and your terminal gets its shell back; one
that does not connect there is refused, naming the address to add in Rex,
and without a Rex app here the refusal says to open Rex. A space with no
session there, such as a chat in the Happy app, is refused, naming its
machine and `shoal move`. When the link drops, that machine hangs the terminal
up, so its client detaches and the session keeps running; your terminal says
so on one line and attaches again to the same session, without a second wake
or restore, until Ctrl-C, which exits non-zero. A machine offline at the
start fails at once, naming it. The attach exits as the one there does, and a
space's process is refused it, as here. An app attaches the same way through
`shoal-client`, without a Rex app of its own.

Sessions are named after the workspace's directory, or for a
chat opaquely, such as `chat-0b9c3a52`, since nothing is built from its name, suffixed `-2`, `-3`, ... when the name
is in use, so `zmx ls` and `zmx attach` work as well, and labelled with their
owner, `shoal.workspace=<id>` or, once the chat is recorded, `shoal.chat=<id>`.
Sessions Shoal creates are also labelled `shoal.created=1`. Removing a
workspace ends the sessions Shoal created for it,
regardless of `terminal.close_when_done`, and daemon startup and `shoal cleanup` end those
whose workspace is gone. Once a space's agent starts in another place than
the one its stopped agent ran in, as a restore does when that place is not
back at its prompt, Shoal ends the one it left, if Shoal created it, nothing
Shoal tracks runs there and no other stopped agent waits there. A session you opened yourself stays open, even after a
Shoal agent ran in it, as do sessions from Shoal versions without the label. An
agent or chat launched in place refuses to start if Shoal cannot relabel the
session.

With [`[terminal] nested_zmx`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#terminal),
or a host's own `nested_zmx`, an agent or chat that would run in place in a
Herdr pane, Rex block, tmux pane or Zellij pane, and a demo a host runs in its
places, runs in a zmx session attached there, named and labelled as above. It
looks and works as before in that place, and from a terminal that host does
not serve, such as over ssh, `shoal attach` and opening the demo attach to the
session instead, where for Rex they would select the block in the Mac app; a
Rex app on another machine still selects the block. Once no zmx client is
attached, as after a detach in the place, attaching from anywhere attaches to
the session. Shoal inside the session
takes the pane or block for its place: starts it queues open beside it, a
restore runs there again in a new session, and `close_when_done`, removal and
cleanup close it and the session, which also ends with its agent. The picker
marks the space `attached` while the place shows it or another zmx client is
attached. A launch that would nest refuses as above when zmx is missing or
older than 1.0.1. The option changes nothing on a terminal no host serves or
in a zmx session. Terminal protocols pass through zmx unchanged, so images and
notifications behave as in the host, while a host that reads an agent's turn
from its pane, as Herdr does, sees only zmx there.

## Agent accounts

[`agent.git_profile`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent) names one of the global
[Git profiles](repo.md#git-profiles) for agents' Git commands.

Issue and PR changes and other forge writes Shoal makes for a workspace or a
chat use the agent account: the host's [`agent_token`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges), with
`agent_email` for a Bitbucket API token. Without an `agent_token`, Shoal
refuses the change and names `agent_token`; set
`agent_token = { same_as = "user_token" }` for agents to make changes with your
login. Changes you make from a registered checkout use your `user_token`.
Processes inside a workspace and a chat's agent read issues, PRs,
labels, reactions and the forge's capabilities as the agent account, with your
`user_token` on a host without an `agent_token`; anyone else reads with your
own logins.

Git over HTTPS to a host with an `agent_token` uses it too, as the agent
account, for tracked agents in workspaces and chats and for the pushes Shoal
runs for them (`shoal pr open` and approved pushes): fetches, pulls and pushes,
to that host's every repository. Their Git settings clear your credential
helpers for that host only and add Shoal's own, which asks the daemon for the
token each time, so a changed token applies at once while a new host applies
from the agent's next start. Git asks Shoal's helper before its first request
to that host, so with Git 2.46 or newer a login for it in your `.netrc` goes
unused; older Git sends that login first. The token stays out of the agent's environment and
Git config, and your helpers neither answer for that host nor save the token;
while the daemon cannot answer, as during a restart, Git on that host fails
instead of using your login or asking for a password. Your own terminal,
`shoal merge` and the daemon's fetches keep your login. A Git profile that sets
`credential.helper`, or a credential helper for the whole host, takes
precedence, and SSH remotes never ask a helper.

The agent Git profile reaches tracked agents through Git's `GIT_CONFIG_COUNT`
variables, after any inherited entries, so it overrides every Git config file for
the agent and its nested commands in any repository; the worktree's config and
ordinary executions keep their settings. When the profile sets `user.name` or
`user.email`, inherited `GIT_AUTHOR_*` and `GIT_COMMITTER_*` variables for that
field are dropped. Push credentials follow profile settings such as
`core.sshCommand`. An undefined profile fails before the agent starts. Where
the agent's [`push` permission](approvals.md#approval-for-agent-pushes) at launch is
`"propose"` or `"deny"`, the same variables carry the rewrites that hold or refuse the agent's pushes.

## Agent secrets

[`[secrets]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#secrets) gives tracked agents values, written in
the config, read from the environment or fetched by a command from your
secret store.

Commands run without input. Without
`tools`, agent launches read the value before the agent starts and add it
to the agent's environment, which nested commands inherit; a command that
fails, prints nothing or prints NUL stops the launch. With `tools`, the
agent's environment never holds the value: the launch puts a launcher for
each listed program first on a private PATH, and each run of the program
through it reads the value, a written one from the daemon, and runs the next
program of that name on PATH with the value set, or fails with the
command's error; no file of the launch holds a written value. Launchers are
cooperative: an absolute program path or a reset PATH bypasses them. Other executions fetch no secrets of their own, and hooks started
beneath an agent drop the inherited values and launchers.

A tool-scoped secret can set `requires_approval = true` and `approval_lifetime`
like a [protected resource](approvals.md#resource-approvals); an environment secret cannot,
because it is fetched before the agent can ask. `shoal catalog secret` lists the
configured secrets with their tools and approval settings, without their values. The agent requests access with
`shoal acquire secret NAME --reason ...`, and the user decides with `shoal
approvals`. Each run of a listed tool in a workspace asks the daemon about
every secret it serves, under the current policy, and fetches a gated value
only once the workspace holds a grant for the secret's current `value` and
`tools`; otherwise the run fails, naming the pending or denied request, or the
`acquire` command when there is none; a run that cleared `SHOAL_SCOPE_TOKEN`
fails too. A launcher serves the config of its agent launch, so once a
secret's `value` or `tools` change, its runs fail until the agent restarts. `shoal release secret NAME` cancels the request or
ends a lease grant.

## Agent state

```sh
shoal internal agent-state working   # Also waiting (for the user mid-turn) or idle (turn finished)
```

Agent hooks report the agent's turn state with this command, and integrations
read it instead of each inferring it from the terminal. Shoal's terminal
Claude Code and Codex launches, including `start --agent`, [chats](chats.md) and resumed sessions, register
the hooks with the [alert hooks](alerts.md); Happy sessions do not.
A session that starts, resumes or clears reports `idle`, so a new agent shows
`waiting` until its first turn. A turn that ends reports `idle` with
`--ends-turn <integration>`, which reads how long the agent's prompt cache lives
from the environment the hook runs in, the agent's own. For Claude Code that is
5 minutes with `FORCE_PROMPT_CACHING_5M`, else `CLAUDE_CODE_PROMPT_CACHE_TTL`,
else `promptCacheTtl` in its user settings, else an hour with
`ENABLE_PROMPT_CACHING_1H`, else 5 minutes with an API key, auth token,
`apiKeyHelper` or another provider such as Bedrock, and an hour on a claude.ai
login, which Shoal assumes is within its usage limits. A turn an API error ends reports `idle` through
`shoal internal turn-failed --integration <name>`, which also reads the failure
the integration's hook event names.
Their `SubagentStart` and `SubagentStop` hooks report each subagent, by the
`agent_id` the hook event names, through
`shoal internal subagent --integration <name>`, and a session that starts,
resumes or clears ends the subagents of the one before. A tracked agent that
is idle while one of its subagents runs, such as a background agent or a
workflow's, counts as working wherever Shoal decides whether to stop an idle
agent, as for [overload protection](load.md) and
[sleep](sleep.md), since stopping it ends the subagent. A subagent whose stop
never arrives counts until the agent's session starts again or the agent
exits, or, once the agent's [proxy](agents.md) has passed a request of its
subagents in the current session, until none has passed for an hour; its status still shows `waiting`.
Neither agent has a hook for an interrupted turn: Claude Code shows `working` until
it notifies that its prompt is waiting for input, and Codex until its next turn
ends. Codex has no hook for a permission prompt, so it reports `working`
instead of `waiting`. Other agents can call the command from their own hooks; a JSON event
on stdin with a `session_id`, as Claude Code and Codex hooks pass, names the session
to [resume](load.md). The command reads only that first event, and stops waiting after
two seconds without input. `list` and `status` show
`working`, `needs input` for `waiting`, or `waiting` for `idle`, since an idle
agent waits for the user or for activity that [wakes it](alerts.md); `status`,
`inspect` and `list --json` give `agent_state` with `state` and the Unix-seconds
`since` it began. An idle report that ends a turn starts the agent's prompt cache
lifetime: until the agent works again, `status` shows `Prompt cache` with when
the cache expires, and the JSON adds `prompt_cache_expires_at` in Unix seconds.
The lifetime is the AI tool's [`prompt_cache_minutes`](agents.md), else what the
agent's own settings give, else the tool's default. A state
reported from a tracked execution shows only while that execution runs, so an
agent whose process died, such as with a reboot, shows none, and the state ends
with the execution's record; one reported from outside lasts until the next
report or removal. A chat's state ends when the chat stops or is restored, and a
report arriving while it is stopped is not recorded. Each change, and each end,
of a workspace's state is a [space event](log.md#space-events); repeating the
current state is not a change. Agent state never notifies, records completion, or affects cleanup.
A space's own process keeps each report it sends in the state directory until
the daemon answers, so a report the daemon does not receive, which the hooks
would discard, stays there even when the hook's time limit ends it first: the
daemon looks for such reports at start, each time a wrapper reattaches after a
restart, and every 15 seconds, records an
[alert](alerts.md) naming the space with the error, and records each
report in the order its hook made it, unless one made later was recorded
first.
Scoped callers report only for their own space, which their scope names
wherever they run; `--space` selects another one for unscoped callers.

## Terminal status

Where an agent runs in a terminal that reads the
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)
(OSC 7501), such as a Rex block, Shoal reports its space's status there on a
record of its own, `id=shoal` with `app=shoal` and the space's name as its
title, leaving the agent's own root record to an agent that reports itself, as
Claude Code does. Before the agent starts, Shoal asks the terminal whether it
reads the protocol, followed by a device attributes request; a terminal that
answers only that request, such as tmux, zmx or Zellij, receives nothing, nor
does an agent that runs detached. Input typed during the query, before the
agent starts, may be lost. The first of these that holds decides the report:

| Space | Report | Message |
|---|---|---|
| A [thread](threads.md) of the space waits for the user | `blocked`, `kind=question` | the thread's number and the first line of its latest post |
| An [approval request](approvals.md#resource-approvals) of the space waits for the user | `blocked`, `kind=permission` | the request's number and what it asks for |
| The agent's turn failed and Shoal does not [retry](agents.md) it | `error` | the failure |
| `shoal done` recorded completion, between the agent's turns | `done` | the summary |
| A [ready mark](completion.md#ready-for-review) stands, between the agent's turns | `done` | the marked item |
| Shoal stopped the agent and brings it back on its own | `idle` | why it stopped |
| The agent's hooks report `working`, for an agent that does not report itself | `working`, with the share of its [steps](progress.md) done | its status line |
| The hooks report `waiting`, likewise | `blocked`, `kind=permission` | |
| The hooks report `idle`, likewise | `idle` | |
| Nothing above | `clear` | |

A thread or request a coordinating agent holds blocks nothing. The report
changes as the space does. The wrapper reports the agent's exit itself, also
while the daemon is away: an exit with an error replaces the report with
`error` and the exit code or signal, and a clean exit clears a report of the
hooks' turn.
