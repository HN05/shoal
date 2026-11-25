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
of a new space: the named workspace, by number or ID, else the current
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
instead, as each host's section in [Terminal hosts](terminals.md) describes. With that host turned off they run in place, and a queued
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

Besides Claude and Codex, Shoal includes [opencode](https://opencode.ai)
(`opencode --prompt {prompt} {args}`), [pi](https://github.com/badlogic/pi-mono)
(`pi`) and [Grok Build](https://docs.x.ai/build/overview) (`grok`); pi and Grok
take the prompt as their first argument. Each starts with `--agent <name>` once
its executable is on PATH. An [`[agents.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) table in
global or repository config adds another AI tool, such as `droid`:
`--agent droid` then starts it, such as `shoal start --in 3 --agent droid` in
an existing workspace. The picker and
completion offer AI tools only, and leave out a tool whose agents
`enabled = false` turns off; their launches and reviews are refused, and a
stopped agent's restore waits until the tool is on again. It stops no running
agent, and that agent's wrapper may still restart it in place. A
[`base`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) makes a tool a launch profile of another, such as
Claude on its own claude.ai login while `claude` keeps going through a proxy.

`--model` and `--effort` on `shoal start`, `contribute`, `review run`,
`repo setup` and `config setup` override the [configured model and effort](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) for
that launch. The tool receives the values through `model_args` and
`effort_args`: Claude Code
gets `--model` and `--effort`, with `CLAUDE_CODE_EFFORT_LEVEL` set to the same
effort, since the variable otherwise overrides the flag; Codex gets `-m` and `-c
model_reasoning_effort="<effort>"`. A launch with a value its tool has no
template for is refused with the template to configure. A Happy session's agent, such as
`happy-claude`, takes them through the tool it starts, whose `model` and
`effort` fill in what nothing else sets. A restored, resumed or moved agent runs
with the values it started with, which its execution in `shoal status --json`
and its recovery records show; a `resume_command` gets them before its `--`.
One whose tool no longer has the templates for them is not restored until it
does. A desktop app takes
neither.

With a tool's [`retry`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools), on by default for Claude Code,
Shoal retries a turn an API error ended, for agents whose failed turns Shoal
learns of: Claude
Code, through its `StopFailure` hook, and any agent whose turn its
[proxy](agents.md) ends, such as Codex. Shoal waits half a minute, doubling
for each later attempt up to an hour for rate or usage limits, or a quarter hour
for overloaded APIs and server errors. Each wait varies randomly by ±20%.
It then sends the idle agent an [alert](alerts.md) to continue its
unfinished work, which reaches it in a new turn of the same conversation, and
restores an agent Shoal put to sleep meanwhile, whose stop names the rate or
usage limit or the failure it waits out. A new
turn before then cancels the retry, and the agent's exit, completion or the
space's removal drops it, a workspace's or a chat's. Attempts follow one another, with no cap, until a
turn ends normally, so an outage of any length or a daily or weekly usage limit
passes and the agent carries on; switching between transient failures and usage
limits starts the wait over. When the agent's proxy learns when the upstream
refuses requests until, such as a usage limit's reset from its error or
headers, the retry waits until half a minute past it instead, until the
upstream answers again. A rate limit's error that Claude Code's failure hook
passes states such a reset as well when it gives a duration after `reset
after`, `resets in`, `try again in` or `retry after`, such as `1h36m24s`, or a
JSON error with `resets_in_seconds`, `reset_seconds` or `resets_at`, within
eight days; without one, a usage limit is checked about hourly. While a retry waits for a known
reset, the agent's alerts and item activity, except an alert from the user,
wake it for no turn the limit would fail, nor restore it from sleep: they
stay unread and arrive with the retry's message, or with the user's alert
if it comes first. A turn Shoal does not retry, because `retry` is off or the error is
one a retry does not fix, such as failed authentication, gets a `turn_failed`
alert naming the failure, which a child space's coordinating agent
receives instead; later failed turns of the same agent add none until one of its
turns ends normally. A pending retry outlasts a daemon restart;
`status` shows it, and `--json` gives `turn_retry` with `failure`, `attempt`,
`due_at` and `delivered`.

`status` shows when an idle agent's [cache expires](#agent-state) from its
tool's [`prompt_cache_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools), which the `cache`
[schedule](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine) reads to restore agents with a warm cache first.

A tool's [`proxy`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent-tools-on-this-machine) sends its tracked
agents' API requests through Shoal's retrying proxy to the `upstream` of a
[`[machine.proxies.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#proxies). Claude Code names `/v1` in each
request's path, and Codex gets the proxy's URL with `/v1` as its base URL, as
gateways such as CLIProxyAPI, LiteLLM and OpenRouter serve the OpenAI API.

A proxy without `upstream` only adds the retries, sending each agent's requests
where they would go without it, which Shoal finds when the agent launches,
from the environment the agent runs with, its tool's `env_remove` included:

- Claude Code: the `ANTHROPIC_BASE_URL` of the first of its settings that sets
  one, as it ranks them (the `--settings` the launch has, then of the sources
  its `--setting-sources` enables, the workspace's `.claude/settings.local.json`,
  `.claude/settings.json` and the user's),
  else of its environment, else `https://api.anthropic.com`.
- Codex on a provider of the user's: that provider's `base_url`.
- Codex on its built-in OpenAI provider: `OPENAI_BASE_URL` when its environment
  sets it, else `https://api.openai.com/v1` when the `auth.json` under its
  `CODEX_HOME` records an API key, and `https://chatgpt.com/backend-api/codex`
  otherwise, as for a ChatGPT login. A provider of the user's without a
  `base_url` goes there too. Shoal cannot read a login Codex may keep in the
  keyring, with a `cli_auth_credentials_store` other than `file`, so such a
  launch fails unless the URL is named: by `OPENAI_BASE_URL` in the tool's
  `env` for the built-in provider, by a `base_url` for a provider of the user's.

Codex then gets the proxy's URL itself as its base URL. A launch profile
inherits its base's `proxy`, and its `env` cannot take it off, so to keep a
direct profile, name the proxy on another profile and leave the base without it.

The agent's wrapper runs one proxy on a loopback port for as long as the agent
runs and points the agent at it: Claude Code through `ANTHROPIC_BASE_URL` in the
settings Shoal passes, which win over its own settings and environment. Claude
Code takes only its last `--settings`, so the wrapper merges every one the launch
has, Shoal's and those forwarded or in a configured resume command, JSON or a
file relative to the workspace, into one: objects key by key, lists such as an
event's hooks one after another, and the proxy's URL as `ANTHROPIC_BASE_URL`, in a file only the user can read, which lasts as the proxy does, so tokens in a private settings file never reach the command line;
Codex by
moving the `base_url` of the provider the launch selects, keeping that
provider's own credentials. Shoal reads the selection as Codex does: the user's
`config.toml`, the `<name>.config.toml` of the profile `--profile` or its
`profile` names (or a legacy `[profiles.<name>]` table), each `.codex/config.toml`
of a trusted project from its root (by `project_root_markers`, `.git` by
default) down to the working directory, which `-C` may move, then the `-c`
overrides it is launched with. Shoal's own overrides come after the launch's, so
they win. On its built-in
OpenAI provider, through a provider Shoal defines with the same login. The proxy forwards each request with
the agent's own credentials, or, with the proxy's `key`, sends the upstream that
key in their place, so an agent on its own login can reach an upstream such as
CLIProxyAPI that wants its own key. A `key` is read when the agent starts, and a
variable that is not set or a command that fails or prints nothing fails the
launch. A restore whose proxy names another key command gives
it a minute. Any local user can reach a loopback port, so each proxy answers only requests that name a key it draws when it starts, in an `x-shoal-proxy-key` header the upstream never sees. The key reaches the agent through its environment, which only its user can read, not its command line: Claude Code gets it in `ANTHROPIC_CUSTOM_HEADERS`, after any headers it has, and Codex's provider names `SHOAL_PROXY_KEY` in its `env_http_headers`. An `ANTHROPIC_CUSTOM_HEADERS` in Claude's own settings replaces the one Shoal sets, and the proxy then refuses its requests. The proxy retries what the agent would otherwise fail on:
a rate limit, an overloaded or failing upstream, an unreachable one, and a stream
that reports one of those before any content, waiting from a second, doubling to
half a minute, or as long as the upstream's `Retry-After`, the reset time its
error names, or Anthropic's `anthropic-ratelimit-unified-reset`, when its
`anthropic-ratelimit-unified-status` of `rejected` says the usage limit refused
the request, asks. Once an outage has lasted five minutes, or a reset falls past
that, the agent gets the failure, and later requests get theirs at once until one
succeeds; a failure a retry does not fix, or one after content arrived, reaches
the agent unchanged. A Codex stream that gives up ends with a `response.failed`
event, carrying the upstream's own error such as a usage limit and its reset,
which ends its turn, instead of an HTTP failure Codex would retry itself, and the
wrapper reports that turn for `retry`, which waits out rate and usage limits. `status`
shows a request the proxy waits to retry, which the wrapper reports, through the installed release once an upgrade changed the daemon's protocol under it; `--json` gives `proxy_retry` with
`failure`, `attempt` and the Unix-seconds `next_at`. Through the proxy, Claude Code on a claude.ai login treats
its base URL as a gateway and leaves out Remote Control and remote managed
settings. Claude Code keeps its prompt cache for an hour only on a claude.ai
login within its usage limits, and for 5 minutes with an API key or auth token,
so a proxied Claude agent gets `ENABLE_PROMPT_CACHING_1H=1` unless the
environment it runs with sets that variable; a lifetime in
`CLAUDE_CODE_PROMPT_CACHE_TTL` or Claude's `promptCacheTtl` setting still wins. Only Claude Code and Codex, and their launch profiles, can go through it; a launch of another tool that names one is refused. A configured `resume_command`, which gets none of Shoal's hooks, still gets the arguments that point it at the proxy, before a `--` or at its end. An agent its wrapper restores in place, as `shoal restart` or an automatic restore does, runs through the proxy its restore command was resolved with: the same one keeps running, another upstream or key, or another default for a proxy without one, replaces it, and one removed since stops. A chat's agent goes through it as a workspace's does; a desktop app that `shoal open` hands a space to runs without it.

[`add_dir`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) gives an agent, fresh or restored, other working
directories, so a sandbox or a setting such as Claude Code's
`blockReadsOutsideWorkingDirectories` lets it work there: each `agent.dirs`
entry for every agent and chat, for a workspace's agent the repository's
workspaces directory, where `shoal create` puts children, its [temporary
directory](setup.md#temporary-directories) and the local config lie, and for a chat its temporary
directory, plus the global config's directory for a [setup](config.md#set-up-config) chat.
Claude Code's [`review`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) prints its JSON result with Claude's
`--json-schema`, loads no user, project or local settings, denies every tool but
reading, searching and `git diff`, `log`, `show` and `blame`, and runs commands in
Claude's sandbox, so it stays read-only whatever those settings allow; without a
sandbox on the machine, such as bubblewrap on Linux, it fails. Codex's runs
`codex exec` in a read-only sandbox with `--output-schema`.
Claude and Codex, and their launch profiles, also get Shoal's
hooks and trust entries, which other tools do without. A Claude Code agent, in
a workspace or a chat, is also allowed to run the
`shoal` commands that report on and finish its assignment, and `shoal stop
--processes` and `--leftovers`, with or without `--json`;
the daemon still limits them to its scope. A workspace agent's settings also add an
`autoMode.environment` entry, after Claude Code's own (`"$defaults"`), that
names the workspace's branch, its commits and its PR as the agent's own work,
so auto mode lets it push the rebased branch with `git push --force-with-lease`.
Auto mode still judges every command, including other pushes. A workspace at a
commit has no branch and gets no entry, and neither does a chat. A branch
renamed while the agent runs keeps its old name in the entry until the agent's
next launch.

`shoal open` hands a space to a desktop app in the directory its agent works
in: a workspace's worktree, the worktree or checkout a chat runs in, or
`root_dir` for a chat without a repository. Without `--app` it opens the space's [`app.default`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#desktop-apps),
else the only
installed desktop app, and on a terminal offers a picker when several are
installed. `--app codex|t3` runs
`codex app <path>` or `t3 app <path>` with any `--` arguments and preserves the
launcher's output and exit code. `--app claude`
opens the Claude app only on macOS, through `open claude://code/new?folder=<path>`,
takes no `--` arguments, and the menu offers it once `/Applications/Claude.app`
is installed. Desktop apps get no agent flags and provide no execution
tracking, scope token, port variables or output default (so automatic cleanup cannot see their activity);
T3's app must already be running.

Happy sessions are an optional integration: off until
[`agents.happy.enabled = true`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools). While off, pickers and completion leave Happy out and
launches are refused before any work starts.

`start --agent happy-claude|happy-codex`, with `--in <workspace>` for an
existing workspace, starts a [Happy](https://github.com/slopus/happy) session as Happy's own daemon
would (`happy <agent> --happy-starting-mode remote --started-by daemon`, then
your `--` arguments), so it registers with that daemon and appears in the Happy
app. The session is detached from your terminal (stdin from `/dev/null`, output
appended to `<state>/workspaces/<id>/happy-<agent>-<time>.log`, deleted with the
workspace) but runs through the tracked wrapper in a background `shoal` process:
it gets the scope token and port variables, counts as activity for idle cleanup,
and `stop`, `rm`, PR cleanup, and reconciliation treat it like any other command.
Shoal returns once the daemon records the launch, printing the execution, PID,
and log (`--json` adds `happy_session_id` and `happy_daemon_recorded`). Without `~/.happy/daemon.state.json` (`$HAPPY_HOME_DIR`
overrides `~/.happy`) Shoal warns that the session will not appear in the app
until `happy daemon start` runs, and launches anyway.

An `--issue` prompt reaches Claude as an argument. `happy codex` takes none, so
Shoal delivers it the way the app does: with the login in `~/.happy/access.key` it creates the session on Happy's server
(`$HAPPY_SERVER_URL`, the settings file, or Happy's default), encrypted as
happy-cli would encrypt it, starts `happy codex` attached to that session through
Happy's own `HAPPY_RECONNECT_*` variables, waits up to 90 seconds for the session
to report alive, and posts the prompt (`curl`, token on stdin). A launch whose
session Happy's server cannot create, such as on a machine not logged in to
Happy, fails before `happy codex` starts; one whose session does not receive the
prompt is stopped, its session on Happy's server deleted, and the launch fails.
Either way a workspace, queue entry or chat stays as any other failed launch
leaves it.

## Launchers and trust

An agent's launcher is the [`command`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) of its AI tool.
Overriding the `command` of Claude or Codex changes only the executable or
flags: prompt templates, trust setup and exit alerts still apply. Claude and
Codex run with their own sandbox and approval settings. Every Codex launch,
chats included, adds `-c sandbox_workspace_write.network_access=true`, so its
commands reach the daemon's socket.

Before a Claude or Codex launch, Happy's included, Shoal trusts the agent's
directories in the tool's user config:

- the repository's checkout, under which Claude Code looks up each worktree's
  trust;
- the repository's shared workspaces directory;
- a workspace placed outside that directory;
- for Claude, which asks about each, every other directory the launch passes
  with `--add-dir` that none of those covers, such as an `agent.dirs` entry;
- for a chat without a repository, `root_dir`.

Shoal creates the user config when it is missing and keeps its other settings.
When the update fails, Shoal warns and still launches the agent.

Claude's user config is `~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`.
Codex CLI and app read `~/.codex/config.toml`, or `$CODEX_HOME/config.toml`.
For a tracked agent, a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in the space's
`[env]` takes precedence over Shoal's environment, and one in the tool's
[`env`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) over both. A desktop app uses Shoal's environment. Trust entries
and skills follow these variables. Unsetting a variable does not undo one that
the agent's own settings file sets, such as the `env` of Claude's
`settings.json`; a separate `CLAUDE_CONFIG_DIR` avoids it.

Removing a repository, by any client or as a [contribution](repo.md#contributions),
removes its trust entries. It removes them from the default files and from each
file its agents, chats and app handoffs ran with, including one a
`CLAUDE_CONFIG_DIR` or `CODEX_HOME` they inherited names. Other project settings
stay. So do the entries for `root_dir` and for directories outside the
repository. An entry Shoal cannot change, on removal or a
[move](repo.md#move-repositories-to-a-new-root), stays in place with a
`trust_left` alert, and the removal or move still completes.

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
[sleep](sleep.md#idle-sleep), since stopping it ends the subagent. A subagent whose stop
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
