# Agents

This file covers `shoal-agents`' AI tool providers, how agents launch with their
settings, and the turn state they report.

An agent Git profile selected the same way reaches tracked agents as Git's
environment-level config, so the agent's commands use it in any repository while
the worktree's config and other executions keep theirs; the push rewrites of the
agent's `push` permission travel the same way. Desktop handoffs have no
authentication override because they may reuse an existing process.

AI tools are providers: built-in ones plus `[agents.<name>]` entries, one table
per tool holding its launcher, resume command, launch mode and argument
templates, merged option by option across layers so a repository can replace
`command` and keep the global `resume`. Agents are not commands: every agent
starts through `shoal start` (or `contribute`), and `resume` and `restart`
bring stopped ones back, so a launch always passes the queue, takes a mode
and meets the one-worker rule, which a command per agent would skip.
`shoal run <name>` runs only `[commands]`, whose names may be agents', and an
agent's name needs no distance from the built-in commands; for an agent's
name without an entry, the error names `start --in`.
What a tool needs of the machine, its skill directory and proxy, sits under
`[machine.agents.<name>]`. Pickers and completion offer providers only. Agents run
as tracked agents with scope, an agent Git profile, and exit alerts. Their `{prompt}` argument combines general instructions
and issue context; without it, nonempty context precedes forwarded arguments.
Plain command invocations expand `{prompt}` to an empty string. User arguments
and inserted prompt text remain literal. A provider's instruction, resume and
extra directory arguments are configuration, built in for the tools Shoal ships;
an integration keyed by agent name adds only what configuration cannot express, such as hooks,
trust entries, waking an idle agent or a launch of its own (the Claude and Codex apps, Happy
sessions), so commands never name a tool. Other names add no tool-specific setup.
A `base` makes an entry a launch profile of another tool, run with the
integration its chain of bases ends at, so one tool can run with several
environments, such as a second login, without losing that setup. Its `env` and
`env_remove` apply to its launches after `[env]`; Shoal reads where the agent
keeps its config from them, so they can move that directory but not remove its
variable.
An agent's model and reasoning effort are `model` and `effort` options of its
tool, which a setting that chooses an agent, such as `[agent] default`, may set
beside it for that use, and `--model` and `--effort` override for one launch.
Values pass through unchanged, since each tool names its models and effort
levels differently; the tool receives them through `model_args` and
`effort_args` templates, built in for Claude and Codex, and a launch with a value
its tool has no template for is refused rather than run without it. Claude's
effort variable overrides its flag, so a launch with an effort sets both. The
execution and its recovery records keep the values, so restores, `shoal resume`
and reports use what the agent started with rather than the configuration at
that time; a restore that cannot pass them waits, as a launch is refused. Desktop apps take neither and say so.
Turn retry defaults to on for Claude and its launch profiles, off for other
tools, and is overridden per tool with `retry`. Backoff is built in, and no
failure a retry can fix has a deadline: an agent waits out an outage or a weekly
quota and then carries on. A reset the agent's proxy learns of from its upstream,
or that a rate limit's error states, holds the retry until it has passed,
whichever of the proxy's and the agent's reports arrives first; the
integration reads a reset from its failure event, and the daemon decides what
it means. While a retry waits for a known reset, the agent's updates wake it
for no turn, nor restore it, since that turn would fail as the last did and
each failure notifies; they arrive with the retry's message, and a message from
the user still wakes it. Retry scheduling knows no agent: an
integration that can report why a turn failed maps the agent's error to Shoal's failure, and the
daemon persists the retry and asks the idle agent to continue through its
messages, so hook-woken and prompt-woken agents get it alike. A retry is tied to
the reporting execution and cancelled by any new turn, so it never competes with
the user or with another delivery; it outlasts that execution only while a
restore of it waits, since Shoal puts an agent to sleep while its retry waits
and the retry's message is the update that restores it. A turn Shoal does not retry, with `retry` off or a
failure no retry fixes, notifies once per execution until the agent ends a turn
normally, so an idle agent whose turns fail is never silent and a run of failures
is one alert.
The retrying proxy lives in `crates/shoal-proxy`, which knows the wire protocols
and, as `shoal-forge` does, depends only on crates that know nothing of the daemon
or the CLI. Each tracked agent that names one gets its
own, run by its wrapper, so a daemon restart never cuts a stream and the proxy
ends with the agent; Shoal points the agent at it through the integration, which
knows how that agent takes a base URL. An upstream is a server root that every
tool shares, each integration naming where its API is under it; a proxy without
one forwards to the base URL the agent would use without it, which the
integration reads from the launch, so it only adds the retries. All its requests share one outage budget,
so the agent's own retries never multiply the proxy's. Claude Code keeps its prompt cache for 5 minutes outside
a claude.ai login within its usage limits, while an agent waiting for CI or a
review idles longer, so a proxied Claude agent gets the 1-hour cache unless its
environment or settings choose a lifetime.

Agent launches use the execution wrapper: by default Codex CLI and Claude run with their
own sandbox and approval settings. Every Codex launch, chats included, turns on its
workspace-write sandbox's network access, which reaching the daemon's socket needs since
Codex allows a single socket only along with the network, whatever the configured
command. Every Claude agent, chats included, gets permission to run the Shoal commands that
finish its assignment, so completion never
depends on Claude Code's auto-mode check. A workspace agent's settings also tell auto mode, in
an `autoMode.environment` entry after Claude Code's own, that the workspace's
branch, commits and PR are the agent's own work, since the classifier reads
settings but not the agent's instructions; it then allows the
`--force-with-lease` push of a rebased branch. The classifier still judges each command, rather than an allow rule
passing every push. Every agent, chats included, gets the
configured `agent.dirs`, `/tmp` by default, as working directories, so it reads what the
tools it runs leave there without a permission prompt. A workspace's agent also
gets the repository's workspaces directory as one, so it works in the children it
splits there without leaving its sandbox or permission rules; chats get only
their temporary directory, plus the global config's directory for a
setup chat without a repository. Both trust the repository's checkout,
which Claude Code keys every worktree's trust by, and its shared Shoal workspaces
directory, or an explicitly placed workspace outside it, and Claude launches each
other working directory none of those covers, which Claude Code asks about, in their
user config before launch, creating the file if needed; Happy
launches and Claude and Codex app handoffs do so too. The user config is the one the agent
reads: a tracked agent's follows its space's `[env]`, while the app runs with
Shoal's environment. Agent guidance and templates become native
CLI instructions or a first-message prefix for Happy Codex; desktop handoffs carry no instructions.
Agents always start as CLI agents; only `shoal open --app` hands a space to
a desktop app, in the directory its agent works in. The Claude app opens through the `claude://` link that `claude --desktop`
opens, since that command refuses a CLI signed in through an API token or gateway.
The daemon's repository removal and move, whoever requests them, update those
trust fields through one operation: a successful removal drops them for the
checkout, the workspaces directory and the worktrees, and a move carries each one a
moved path held to its new path, while preserving other project settings. It edits
the tools' default configs and each location an agent ran with, which the daemon
keeps, inherited variables included, from when the CLI trusts its space there, since
a renamed or removed space, or a changed `[env]`, no longer resolves to
it; it never reads a caller's environment. A removal forgets the locations after
cleanup, when no agent can add one. Failures are logged and alert the user, and
never fail the removal or move. A development build changes only the configs in a
home that is not the user's own, skipping kept locations outside it.

Desktop handoffs (Claude and Codex apps, T3) provide no tracking or scope. A
host, such as zmx or Herdr, runs Shoal's tracked agents, chats and demos and
shows them; an external app, such as Quay, is a UI that is not a host and reads
and acts on Shoal's state through `shoal_client`. It holds a workspace while it
shows it and queues agents, which the daemon starts detached in
`terminal.default_host`, which never names an external app. Happy sessions are
an optional integration behind the `agents.happy.enabled` setting, off by
default. Happy is an agent backend like any other: `shoal happy` is gone, and
its agents start as other agents do, with `--agent happy-claude|happy-codex`.
They are the phone-driven flow: a console session that Happy's daemon started in
a non-workspace directory creates workspaces and starts sessions in them. Shoal
launches `happy <agent>` with the daemon's own flags so the session registers
with Happy and appears in the app, detached from the terminal but inside the
tracked wrapper (a background `shoal` process holding the execution), so it
stays stoppable and visible to cleanup. Happy's Codex mode takes no prompt
argument, so Shoal delivers prompts the way the app does: it creates the session
on Happy's server with the machine's own Happy login, encrypted as happy-cli
would, attaches the CLI through Happy's reconnection variables, and posts the
first message once the session is alive. A session that cannot receive its first
message is no working launch, so a launch Happy cannot seed is refused and one
whose delivery fails is stopped; nothing is left on disk for the user to send.
Shoal reads Happy's credentials only for this and stores none. A Happy-side
pre-spawn hook asking Shoal for a workspace was considered and not adopted.

Agent state is the turn state a space's agent reports from its hooks:
`working`, `waiting` for the user mid-turn, or `idle` before its first turn
and after each. Lists name them by what the user does: `working`, `needs input`,
and `waiting`, since an idle agent waits for the user or for activity that wakes
it; a ready workspace whose agent or command reports no state, or a running
chat whose agent reports none yet, is `process running`,
one where nothing runs `inactive`, a ready mark `ready`, and a recorded
completion `finished`, which only an agent needing input outranks. A done
space with stopped work is `finished` too: its stopped agent has nothing left
to do, and `stopped` would ask the user to resume it. A row's marker groups statuses by what they ask of the user, in a
shape that reads without color. Like
ready marks it is a status signal for integrations that never notifies,
completes, or affects idle cleanup, though a report from a space's own
process that the daemon does not receive alerts the user with the error, which
the hooks discard. The hook keeps its report on disk until the daemon answers,
so neither the alert nor the state depends on the call that failed or on the
hook outliving its time limit: the daemon finds the reports left, alerts, and
records each late, in the order its hook made it, unless a report made after
it was recorded first.
Every report counts from when its hook made it, and a chat's late report
only while the chat runs the agent that made it; overload protection reads it only to stop
idle agents first. A workspace keeps the
latest report; a report from a tracked execution shows only while that
execution runs and ends with its record, while one from outside lasts until
replaced or removal. Changes and
endings of a workspace's state are journal events; repeating the current state is not a change and
keeps its start time. Shoal's terminal Claude Code and Codex launches, resumed
ones included, add the state hooks to the launch option that carries their
message hooks (Happy sessions do not), so the agents' own configuration stays the
user's; an interrupted Claude Code turn has no hook and ends with Claude Code's
idle-prompt notification, while an interrupted Codex turn stays working until
its next turn ends.
An idle report that ends a turn also starts the agent's prompt cache lifetime,
which each request renews, so the `cache` schedule knows which restores find
their cache warm. The AI tool's configured lifetime wins, then the one the
agent's own settings give, which only its hook reads from the environment the
agent runs with, then the tool's default: for Codex 45 minutes, the least its
OpenAI models keep a session's cache, which a resume in a new process reads
too, though some keep it for hours, and a 30-minute guess for other tools.
