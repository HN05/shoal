# Agents

This file covers `shoal-agents`' AI tools, how agents launch with their
settings, and the turn state they report.
[Agents](../reference/agents.md) says what each part does.

## AI tools

Shoal runs AI tools as agents. The tools Shoal includes and each
`[agents.<name>]` table are AI tools alike. One table holds what a launch of
the tool needs: its launcher, its resume commands and its argument templates.
Its options merge one by one across config layers, so a repository can replace
`command` and keep the global `resume`. What a tool needs of a machine, such as
its skill directory and proxy, sits under `[machine.agents.<name>]`.

### Configuration and integrations

Configuration holds the arguments a launch passes, such as a tool's
instruction, resume and extra directory arguments. Shoal builds them in for the
tools it includes. An integration, keyed by the tool's name, adds only what
configuration cannot express: hooks, trust entries, waking an idle agent, or a
launch of its own, such as a desktop app's or a Happy session's. Commands thus
never name a tool. A tool without an integration gets no tool-specific setup.

### Launch profiles

A `base` makes a table a launch profile of another tool. The profile runs with
the integration its chain of bases ends at. One tool can thus run with several
environments, such as a second login, and keep that setup.

A profile's `env` and `env_remove` apply to its launches after `[env]`. Shoal
reads from them where the agent keeps its config. A profile can therefore move
that directory but not remove its variable.

## Agents are not commands

Every agent starts through `shoal start` or `shoal contribute`, and
`shoal resume` and `shoal restart` bring a stopped one back. A launch thus
always passes the agent queue, takes a mode and meets the rule of one agent per
workspace. A command per agent would skip all three. `shoal run` runs only
`[commands]`, as [Run](../reference/run.md#agents-are-not-commands) describes.

## Model and effort

An agent's model and reasoning effort are its AI tool's `model` and `effort`. A
setting that chooses an agent, such as `[agent] default`, may set them beside
it for that use. `--model` and `--effort` override them for one launch.

Shoal passes the values through unchanged, since each tool names its models and
effort levels its own way. The tool receives them through its `model_args` and
`effort_args` templates. Shoal refuses a launch with a value its tool has no
template for, rather than run the agent with a model nobody chose.

The execution and its recovery records keep the values the agent started with.
Restores, `shoal resume` and reports thus use those values, not the
configuration of the time. A restore that cannot pass them waits, as a launch
is refused.

## Turn retry

Shoal retries a turn that an API error ended. No failure that a retry can fix
has a deadline: an agent waits out an outage or a weekly quota and then carries
on. [Turn retry](../reference/agents.md#turn-retry) gives the backoff and the
defaults.

### Integrations report, the daemon decides

Retry scheduling knows no AI tool. An integration that can report why a turn
failed maps the tool's error to Shoal's failure, and reads a reset from the
failure event. The daemon decides what the failure means. It persists the retry
and asks the idle agent to continue through an alert. Agents that hooks wake
and agents that a prompt wakes thus get the retry alike.

### Known resets

The agent's proxy may learn a reset from its upstream, and a rate limit's error
may state one. The retry holds until the reset has passed, by whichever of the
two reports arrives first.

While a retry waits for a known reset, the agent's updates start no turn and
restore no agent. That turn would fail as the last one did, and each failure
notifies. The updates arrive with the retry's alert instead. An alert from the
user still wakes the agent.

### A retry never competes

A retry belongs to the execution that reported the failure. Any new turn
cancels it, so it never competes with the user or with another delivery. It
outlasts its execution only while a restore of it waits: Shoal puts an agent to
sleep while its retry waits, and the retry's alert restores it.

### Turns Shoal does not retry

Shoal does not retry a turn when the tool's `retry` is off or no retry fixes
the failure. Such a turn notifies once per execution, until the agent ends a
turn normally. An idle agent whose turns fail is thus never silent, and a run
of failures is one alert.

## Retrying proxy

`shoal-proxy` holds the retrying proxy and knows the APIs' wire protocols. Like
`shoal-forge`, it depends only on crates that know nothing of the daemon or the
CLI.

### One proxy per agent

Each tracked agent whose tool names a proxy gets its own, which its wrapper
runs. A daemon restart thus never cuts a stream, and the proxy ends with the
agent. The integration points the agent at the proxy, since it knows how its
tool takes a base URL.

### Upstreams

An upstream is a server root that every AI tool shares. Each integration names
where its API lies under that root. A proxy without an upstream forwards to the
base URL the agent would use without it, which the integration reads from the
launch. Such a proxy only adds the retries.

### One outage budget

All of a proxy's requests share one outage budget. The agent's own retries thus
never multiply the proxy's.

### Claude Code's prompt cache

Outside a claude.ai login within its usage limits, Claude Code keeps its prompt
cache for 5 minutes. An agent that waits for CI or a review idles longer. A
proxied Claude agent therefore gets the 1-hour cache, unless its environment or
settings choose a lifetime.

## Sandbox and permissions

Agents launch through the [execution wrapper](execution.md). Claude Code and
Codex keep their own sandbox and approval settings. Shoal adds what its own
commands need.

Every Codex launch, chats included, turns on network access in its
workspace-write sandbox, whatever the configured command. The agent needs the
daemon's socket, and Codex allows a socket only along with the network.

Every Claude agent, chats included, may run the Shoal commands that finish its
assignment. Completion thus never depends on the check of Claude Code's auto
mode.

A workspace agent's settings tell auto mode that the workspace's branch,
commits and PR are the agent's own work. They do so in an
`autoMode.environment` entry after Claude Code's own, since the classifier
reads settings but not the agent's instructions. Auto mode then allows the
`--force-with-lease` push of a rebased branch. The classifier still judges each
command, where an allow rule would pass every push.

## Working directories

Every agent, chats included, gets the `agent.dirs` entries as working
directories. It thus reads what the tools it runs leave there, such as in
`/tmp`, without a permission prompt. A workspace's agent also gets the
repository's workspaces directory. It thus works in the child workspaces it
creates there without leaving its sandbox or permission rules.
[Working directories](../reference/agents.md#working-directories) lists what
each kind of space gets.

## Trust

Shoal trusts an agent's directories in its AI tool's user config before the
launch, creating the file if needed. Happy launches and Claude and Codex app
handoffs do so too. The trust covers the repository's checkout, since Claude
Code keys each worktree's trust by it. For Claude, it also covers each other
working directory that the rest leave out, since Claude Code asks about each.
[Trusted directories](../reference/agents.md#trusted-directories) lists them.

The user config is the one the agent reads. A tracked agent's follows its
space's `[env]`, while a desktop app runs with Shoal's environment.

### Trust when a repository moves or goes

Removing or moving a repository, whoever requests it, updates the trust entries
through one daemon operation. A removal drops the entries for the checkout, the
workspaces directory and the worktrees. A move carries each moved path to its
new path. Both keep other project settings.

The operation edits the tools' default configs and each config location an
agent ran with. The daemon records those locations, inherited variables
included, when the CLI trusts its space there. It cannot find them later: a
renamed or removed space, or a changed `[env]`, no longer resolves to them, and
the daemon never reads a caller's environment. A removal forgets the locations
after cleanup, when no agent can add one.

A failure is logged and alerts the user, and never fails the removal or move.
A development build changes only the configs in a home that is not the user's
own, and skips recorded locations outside it.

## Instructions

Shoal passes an agent's guidance and templates as its CLI's native
instructions. Happy's Codex gets them as a prefix of its first message instead.
A desktop app gets none.

## Agent Git profile

The agent Git profile reaches tracked agents as Git's environment-level config.
The agent's commands thus use it in any repository, while the worktree's config
and other executions keep theirs. The rewrites that hold or refuse pushes under
the agent's `push` permission travel the same way.

## Where agents run

Agents always start as CLI agents. A host, such as zmx or Herdr, runs Shoal's
tracked agents, chats and demos and shows them, as
[Terminal hosts](terminals.md) describes. An external app, such as Quay, is a
UI and not a host: it reads and acts on Shoal's state through `shoal-client`.
It holds a workspace while it shows it, and queues agents. The daemon starts
those agents detached in `terminal.default_host`, which never names an external
app.

### Desktop apps

Only `shoal open` hands a space to a desktop app, in the directory its
agent works in. A desktop app, such as the Claude or Codex app or T3, gets no
tracking, scope or instructions. It also gets no agent account, since a handoff
may reuse a process that already runs.

The Claude app opens through the `claude://` link that `claude --desktop`
opens. That command refuses a CLI signed in through an API token or a gateway.

## Happy sessions

Happy is an optional integration, off until `agents.happy.enabled` turns it on.
A Happy session is an agent like any other, started with
`--agent happy-claude` or `--agent happy-codex`. It serves the phone-driven
flow: a console session that Happy's daemon started outside any workspace
creates workspaces and starts sessions in them.

Shoal launches `happy <agent>` with Happy's daemon's own flags, so the session
registers with Happy and appears in its app. The session runs detached from the
terminal but inside the tracked wrapper. It thus stays stoppable and visible to
cleanup.

Happy's Codex takes no prompt argument. Shoal therefore delivers the first
prompt as the Happy app does, through Happy's server, with the machine's own
Happy login. Shoal reads Happy's credentials only for this and stores none.

A session that cannot receive its first message is no working launch. Shoal
refuses a launch that Happy cannot seed, and stops one whose delivery fails.
Nothing is left on disk for the user to send.

Shoal launches each Happy session itself. It takes no request for a workspace
from a hook that Happy runs before it spawns one.

## Agent state

Agent state is the turn state a space's agent reports from its hooks:
`working`, `waiting` for the user mid-turn, or `idle` before its first turn and
after each. Integrations read it instead of each inferring it from the
terminal.

### State hooks

Shoal's terminal Claude Code and Codex launches, resumed ones included, add the
state hooks to the launch option that carries their alert hooks. The agent's
own configuration thus stays the user's.
[State hooks](../reference/agents.md#state-hooks) says what each reports.

### State in lists

Lists name each state by what the user does: `working`, `needs input` for
`waiting`, and `waiting` for `idle`. An idle agent waits for the user or for
activity that wakes it. A done space whose work stopped shows `finished`: its
agent has nothing left to do, and `stopped` would ask the user to resume it.
A row's marker groups statuses by what they ask of the user, in a shape that
reads without color. [Statuses](../reference/workspaces.md#statuses) gives
their order.

### What state affects

Like a ready mark, agent state is a signal for integrations. It never notifies,
records completion or affects idle cleanup. Overload protection reads it only
to stop idle agents first.

### How long a report lasts

Every report counts from when its hook made it. A workspace keeps the latest
report. A report from a tracked execution shows only while that execution runs,
and ends with its record. A report from outside lasts until another replaces it
or the space goes. A chat's late report counts only while the chat runs the
agent that made it.

Each change and each end of a workspace's state is a journal event. Repeating
the current state is no change, and keeps its start time.

### Reports the daemon misses

A report from a space's own process that the daemon does not receive alerts the
user with the error, which the hooks would discard. The hook keeps its report
on disk until the daemon answers. Neither the alert nor the state thus depends
on the call that failed, or on the hook outliving its time limit. The daemon
finds the reports left, alerts, and records each late, in the order its hook
made them, unless a later report was recorded first.

### Prompt cache lifetime

An idle report that ends a turn also starts the agent's prompt cache lifetime,
which each request renews. The `cache` schedule thus knows which restores find
their cache warm.

The AI tool's configured lifetime wins, then the one the agent's own settings
give, then the tool's default. Only the agent's hook reads those settings,
since they come from the environment the agent runs with. Codex's default is
the least time its OpenAI models keep a session's cache, which a resume in a
new process reads too, though some models keep it for hours. Other tools'
default is a guess.
