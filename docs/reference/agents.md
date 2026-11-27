# Agents

This file says how Shoal runs AI tools as agents: where they run, how you reach
them, what they are given, and the state they report.
[Agents](../design/agents.md), [terminal hosts](../design/terminals.md) and
[execution](../design/execution.md) give the reasons.

## Start an agent

Every `shoal start` starts an agent. It starts the agent `-a`/`--agent` names,
else `agent.default`. Without either, a terminal offers a picker of agents, and
a start without a terminal is refused before anything is created.
`shoal create` makes a workspace without an agent.

The agent starts once worktree creation, setup and the post-setup hook succeed.
Arguments after `--` go to the agent. `--model` and `--effort` apply to
whichever agent starts, as [model and effort](#model-and-effort) says.

Shoal refuses an agent whose executable is not on `PATH`, before it creates
anything. The agent picker, the workspace menu and tab completion leave that
agent and its shortcut command out.

An agent runs through the tracked execution wrapper. An agent run in place
returns its exit code. It restores the terminal's OS settings, even after an
interruption. When `TERM` is set and is not `dumb`, it also resets the
emulator's input modes for the shell. A failed launch keeps the workspace.

With [shell integration](install.md#shell-integration), your shell enters the
new workspace once an agent run in place exits. An agent in a zmx session
leaves your shell where it was.

`--json` prints the workspace record first, then the agent's own output
unchanged. For an agent that starts detached, it prints the agent's queue entry
in place of that output.

## Start a workspace's own agent

`shoal start --in [WORKSPACE]` in a mode that starts in a workspace starts the
agent of an existing, ready workspace. [Modes](modes.md#start-in-a-mode) says
which mode a start takes and what a chat mode does with `--in`. The start takes
the workspace named by number or ID, else the current workspace, else one from
a picker.

Nothing is created. The workspace keeps its place in the tree, its name and its
links, and its links' activity reaches the new agent. The start reports the
workspace's record and the agent's queue entry, as for a new workspace.

Arguments after `--` go to the agent. Without them, the agent starts on the
workspace's issue, followed by the mode's instructions. Without an issue, it
waits for you. The agent sees the branch's commits and continues from them.

The flags that start, queue and place an agent work as for any start. A flag
that names what a new space starts as or on is refused, such as `--parent`,
`--name`, a repository or `--issue`. The refusal of `--issue` names
`shoal link issue <number> assigned --space <workspace>`.

A workspace has one such agent, which the [agent queue](queue.md#one-agent-per-workspace) enforces. A
workspace's agent starts this way only the agent of a workspace below its own
space.

The workspace menu's agent choice runs
`shoal start --in <workspace> --mode worker --agent <name> --here`.

## Where an agent runs

On a terminal, `start --agent` and `resume` run the agent in a
[zmx](https://github.com/neurosnap/zmx) session. The agent keeps running when
you detach with Ctrl-\ or close the terminal. zmx passes the agent's output
through unchanged, so images and other terminal protocols keep working. It
restores the screen when you attach again. The tracked wrapper runs inside the
session, so stop, resume, scope and cleanup treat the agent as any other.

[`terminal.default_host`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#terminal)
sends these launches to another host instead, as
[Terminal hosts](terminals.md) describes for each host. With that host turned
off, they run in place, and a queued start or a demo that no other host takes
fails.

### Run in place

A launch runs in place inside a zmx session, where `ZMX_SESSION` is set. It also
runs in place in a pane or block of another enabled host, such as a Herdr pane
with `terminal.herdr.enabled`.

`-H`/`--here` on `shoal start`, `contribute`, `repo setup` and `config setup`
runs the launch in this terminal, whatever the host, instead of a new tab or a
detached session. An agent that `start` or `contribute` then starts runs as the
launches above do, such as in a zmx session attached here. A chat runs in
place.

The hub can [place](hub.md#placement) such a start, or one that would open a
tab, on another machine. This terminal then attaches to its agent there.

`start --here` and `contribute --here` return the agent's exit code. For a
caller outside Shoal, `resume --focus` also runs the agent in place and returns
its exit code.

### Detached agents

These calls start the agent detached, through the daemon:

- `resume`;
- `start` and `contribute` from a scoped caller;
- `start` and `contribute` from a caller without a terminal, such as another
  agent's tool call.

The call returns without running the agent, and interrupting the call leaves
the agent running. The [agent queue](queue.md) starts it.

An agent Happy runs leaves the call at once in any case. A caller outside Shoal
therefore launches it directly, as its repository's settings decide.

A scoped caller's `--here` runs the agent here once its workspace is prepared
and a slot is free. Without a free slot, it leaves the agent queued and
returns, rather than skip the limits. The agent belongs to its own space, with
that space's scope, so the caller's stops leave it running.

### On a terminal no host serves

With zmx as `terminal.default_host`, an interactive `shoal start` on a terminal
no host serves places its launch as it would a tab:

- An agent that starts with input runs setup and the agent in a detached zmx
  session. The start returns with a `shoal attach` hint.
- Any other launch runs here and attaches to its agent's session.

`terminal.focus` overrides that choice, as it does for tabs, and `--here` keeps
the launch in this terminal.

A detached session ends with its agent, whatever its exit status. When the
agent's start fails, the session opens your login shell (`$SHELL -l`) to show
why. A workspace that `shoal create` opens there gets its shell, as a tab does.

When `done` keeps the workspace, a session Shoal starts for `shoal start`, an
attached agent or a background `shoal resume` ends after the agent's final
turn. `[terminal] close_when_done = false` keeps it open.

## zmx sessions

A launch that would start a zmx session refuses the agent when zmx is missing,
as it does a missing agent executable. It also refuses a zmx older than 1.0.1.

zmx reports no exit status, so these launches exit 0 once the agent exits or
you detach. When the session returns control, Shoal resets the emulator's input
modes on your terminal.

### Session names and labels

A workspace's session is named after the workspace's directory. A chat's
session takes an opaque name, such as `chat-0b9c3a52`, since nothing is built
from its name. A name in use gets `-2`, `-3` and so on. `zmx ls` and
`zmx attach` work with these names.

Each session is labelled with its owner: `shoal.workspace=<id>`, or
`shoal.chat=<id>` once the chat is recorded. Sessions Shoal creates are also
labelled `shoal.created=1`. An agent or chat launched in place refuses to start
when Shoal cannot relabel its session.

### End sessions

Removing a workspace ends the sessions Shoal created for it, whatever
`terminal.close_when_done` says. Daemon startup and `shoal cleanup` end the
sessions whose workspace is gone.

A space's agent can start in another place than the one its stopped agent ran
in, as a restore does when that place is not back at its prompt. Shoal then
ends the session it left when all of these hold:

- Shoal created the session;
- nothing Shoal tracks runs there;
- no other stopped agent waits there.

A session you opened yourself stays open, even after a Shoal agent ran in it.
So does a session without Shoal's labels.

### Upgrade zmx

A zmx reaches only the sessions a zmx of its own version started. Before you
upgrade from zmx 0.8 or earlier, stop the agents in zmx sessions with
`shoal stop`. Resume them afterwards with `shoal resume`.

Sessions an older zmx started keep running but are unreachable. `shoal status`
and Shoal's cleanup alerts report them until their processes end. `shoal stop`
ends an agent's.

## Nested zmx sessions

[`[terminal] nested_zmx`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#terminal),
or a host's own `nested_zmx`, runs zmx sessions inside a host's places. An
agent or chat that would run in place in a Herdr pane, Rex block, tmux pane or
Zellij pane then runs in a zmx session attached there. So does a demo a host
runs in its places. The session is named and labelled as
[for any launch](#session-names-and-labels).

The agent looks and works as before in its place. From a terminal that host
does not serve, such as over ssh, `shoal attach` and opening the demo attach to
the zmx session. For Rex they would otherwise select the block in the Mac app.
A Rex app on another machine still selects the block. Once no zmx client is
attached, as after a detach in the place, attaching from anywhere attaches to
the session.

Shoal inside the session takes the pane or block for its place:

- starts it queues open beside it;
- a restore runs there again, in a new session;
- `close_when_done`, removal and cleanup close the place and its session.

The session also ends with its agent.

The picker marks the space `attached` while the place shows it or another zmx
client is attached. A launch that would nest is refused when zmx is missing or
older than 1.0.1.

The option changes nothing on a terminal no host serves, or inside a zmx
session. Terminal protocols pass through zmx unchanged, so images and
notifications behave as in the host. A host that reads an agent's turn from its
pane, as Herdr does, sees only zmx there.

## Attach to an agent

`shoal attach [target]` reattaches to the session of the workspace or
[chat](chats.md) that `target` names. Without a target, it takes the current
workspace, else the only running chat of the current checkout. Otherwise it
offers a picker of the workspaces and chats that have sessions or whose agent is
[asleep](sleep.md) or stopped, each with its status.

For a workspace with several sessions, it offers a picker, or
`--session <name>` names one. While a session shows the agent, the picker
offers only the sessions that do.

The picker and `shoal status` list each session with the agent the daemon
recorded running there. They mark it `attached` while a terminal shows it:

- a zmx client;
- a Rex app with the block's session open;
- the focused Herdr tab, since Herdr does not count its clients.

Attach finds a session where the daemon recorded the workspace's agent or the
chat running. It also finds one Shoal opened for the workspace, such as a
detached session left at its shell after a failed setup.

### Places without a zmx session

An agent or chat running in a Herdr pane counts as a session named after its
tab. Attaching focuses the tab. From a terminal outside Herdr, it attaches a
Herdr client to the tab's Herdr session. Inside another Herdr session, it is
refused.

A chat running in the Happy app, or in a terminal no host serves, has no
session. Attaching says where it runs instead.

### Wake and restore on attach

Attaching wakes an agent at once, whatever the agent limits and the queue's
rank, and attaches once the agent runs.

While an asleep agent's wrapper waits, its session shows only the stop, and the
pickers list the session as stopped. Attaching to it starts the agent's restore
there, as `shoal queue start --attach` does.

Without a session that shows an agent, attaching starts the restore an asleep
agent waits for. Without any session left, it waits for the agent a launch is
starting there. Otherwise it resumes the agent as `shoal resume --now` does,
and refuses a space with only stopped commands. Neither starts a second agent
beside one whose wrapper waits.

A session whose agent stopped without a restore in the queue says why the agent
stopped and what restores it.

A session can end while its agent moves to a new place, as a
[conversion](chats.md#convert-a-chat-or-a-workspace) moves it. A zmx session,
for one, ends with its agent. You stay attached: the attach waits for the agent
to run in its next session and attaches there.

### Attach on another machine

`shoal attach <target>` can name a space that another machine of the hub runs,
by its number or ID. That machine then runs the same attach for you, on a
terminal of your terminal's size and `TERM`. It wakes or restores the agent and
picks a session there, which `--session` may name. It draws its picker on that
terminal, which yours shows in raw mode.

Every key goes to that machine, so the host's own detach key, such as zmx's
Ctrl-\, detaches. A resize follows too.

A zmx, tmux, Zellij or Herdr session takes a client of its host on that
terminal. For tmux, Zellij and Herdr, the client switches the session's window
or tab for its other clients, as an attach from a terminal outside it does.

A Rex block is selected in your own Rex app, which connects to that machine's
Rex server under the label your machine's Rex reports. Your terminal gets its
shell back. When your app does not connect there, the attach is refused, naming
the address to add in Rex. Without a Rex app here, the refusal says to open
Rex.

A space with no session there, such as a chat in the Happy app, is refused. The
refusal names its machine and `shoal move`.

When the link drops, that machine hangs up the terminal, so its client
detaches and the session keeps running. Your terminal says so on one line and
attaches again to the same session, without a second wake or restore. It keeps
trying until Ctrl-C, which exits non-zero. A machine that is offline at the
start fails the attach at once, naming the machine.

The attach exits as the one on that machine does. A space's process is refused
it, as on this machine. An app attaches the same way through `shoal-client`,
without a Rex app of its own.

## AI tools

Shoal includes Claude Code and Codex, and these tools:

- [opencode](https://opencode.ai), launched as
  `opencode --prompt {prompt} {args}`;
- [pi](https://github.com/badlogic/pi-mono), launched as `pi`, which takes the
  prompt as its first argument;
- [Grok Build](https://docs.x.ai/build/overview), launched as `grok`, which
  takes the prompt as its first argument.

Each starts with `--agent <name>` once its executable is on `PATH`.

An
[`[agents.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
table in global or repository config adds another AI tool, such as `droid`.
`--agent droid` then starts it, as in `shoal start --in 3 --agent droid`. The
picker and completion offer only AI tools.

`enabled = false` turns a tool's agents off:

- the picker and completion leave the tool out;
- its launches and reviews are refused;
- a stopped agent's restore waits until the tool is on again.

Turning a tool off stops no running agent, and that agent's wrapper may still
restart it in place.

A `base` makes a tool a launch profile of another. For example, a profile runs
Claude on its own claude.ai login while `claude` keeps going through a
[proxy](#retrying-proxy).

## Model and effort

`--model` and `--effort` on `shoal start`, `contribute`, `review run`,
`repo setup` and `config setup` override the
[configured model and effort](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
for one launch.

The tool receives the values through its `model_args` and `effort_args`:

- Claude Code gets `--model` and `--effort`. Shoal also sets
  `CLAUDE_CODE_EFFORT_LEVEL` to the same effort, since that variable otherwise
  overrides the flag.
- Codex gets `-m` and `-c model_reasoning_effort="<effort>"`.

A launch with a value its tool has no template for is refused, naming the
template to configure.

A Happy session's agent, such as `happy-claude`, takes the values through the
tool it starts. That tool's `model` and `effort` fill in what nothing else
sets.

A restored, resumed or moved agent runs with the values it started with. Its
execution in `shoal status --json` and its recovery records show them. A
`resume_command` gets them before its `--`. An agent whose tool no longer has
the templates for them is not restored until it has them again.

A desktop app takes neither value.

## Turn retry

A tool's
[`retry`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
makes Shoal retry a turn that an API error ended. Shoal learns of these failed
turns from:

- Claude Code, through its `StopFailure` hook;
- any agent whose turn its [proxy](#retrying-proxy) ends, such as Codex.

### Backoff

Shoal waits half a minute before the first attempt, and doubles the wait for
each later one. The wait grows to at most an hour for rate or usage limits, and
to at most a quarter hour for overloaded APIs and server errors. Each wait
varies randomly by ±20%. A switch between transient failures and usage limits
starts the wait over.

Attempts follow one another, with no cap, until a turn ends normally. An outage
of any length, or a daily or weekly usage limit, thus passes and the agent
carries on.

### Known resets

The agent's proxy may learn when the upstream accepts requests again, such as a
usage limit's reset from its error or headers. The retry then waits until half a
minute past that time, and again until the upstream answers.

A rate limit's error that Claude Code's failure hook passes can state such a
reset too:

- as a duration after `reset after`, `resets in`, `try again in` or
  `retry after`, such as `1h36m24s`;
- as a JSON error with `resets_in_seconds`, `reset_seconds` or `resets_at`.

Shoal takes a reset within eight days. Without a known reset, a usage limit is
checked about hourly.

While a retry waits for a known reset, the agent's alerts and item activity
start no turn, which the limit would fail, and restore no agent from sleep.
They stay unread and arrive with the retry's message. An alert from the user
still wakes the agent, and the others arrive with it.

### Continue the turn

When the wait ends, Shoal sends the idle agent an [alert](alerts.md) to
continue its unfinished work. The alert reaches it in a new turn of the same
conversation. Shoal restores an agent it put to sleep meanwhile, whose stop
names the rate limit, usage limit or failure it waits out.

A new turn before then cancels the retry. The agent's exit, its completion or
the space's removal drops the retry, in a workspace or a chat.

A pending retry outlasts a daemon restart. `status` shows it, and `--json`
gives `turn_retry` with `failure`, `attempt`, `due_at` and `delivered`.

### Turns Shoal does not retry

Shoal does not retry a turn when `retry` is off, or when a retry does not fix
the error, such as failed authentication. The turn gets a `turn_failed` alert
naming the failure. A child space's coordinating agent receives it instead of
the user. Later failed turns of the same agent add no alert until one of its
turns ends normally.

## Retrying proxy

A tool's
[`proxy`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent-tools-on-this-machine)
sends its tracked agents' API requests through Shoal's retrying proxy. The
proxy forwards them to the `upstream` of a
[`[machine.proxies.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#proxies)
entry.

Only Claude Code, Codex and their launch profiles go through a proxy. A launch
of another tool that names one is refused. A launch profile's `env` cannot take
off the `proxy` it inherits. To keep a direct profile, name the proxy on
another profile and leave the base without it.

A chat's agent goes through the proxy as a workspace's agent does. A desktop
app that `shoal open` hands a space to runs without it.

### Upstreams

Claude Code names `/v1` in each request's path. Codex gets the proxy's URL with
`/v1` as its base URL, as gateways such as CLIProxyAPI, LiteLLM and OpenRouter
serve the OpenAI API.

The proxy forwards each request with the agent's own credentials. With the
proxy's `key`, it sends the upstream that key in their place. An agent on its
own login thus reaches an upstream that wants its own key, such as CLIProxyAPI.

Shoal reads a `key` when the agent starts. A variable that is not set, or a
command that fails or prints nothing, fails the launch. A restore whose proxy
names another key command gives that command a minute.

### Proxies without an upstream

A proxy without `upstream` only adds the retries. It sends each agent's
requests where they would go without the proxy. Shoal finds that address when
the agent launches, from the environment the agent runs with, its tool's
`env_remove` included:

- Claude Code: the `ANTHROPIC_BASE_URL` of the first of its settings that sets
  one, as it ranks them. It ranks the launch's `--settings` first, then the
  sources its `--setting-sources` enables: the workspace's
  `.claude/settings.local.json`, `.claude/settings.json` and the user's
  settings. Else Shoal takes `ANTHROPIC_BASE_URL` from its environment, else
  `https://api.anthropic.com`.
- Codex on a provider of the user's: that provider's `base_url`.
- Codex on its built-in OpenAI provider: `OPENAI_BASE_URL` when its environment
  sets it. Else `https://api.openai.com/v1` when the `auth.json` under its
  `CODEX_HOME` records an API key, and `https://chatgpt.com/backend-api/codex`
  otherwise, as for a ChatGPT login. A provider of the user's without a
  `base_url` goes there too.

Codex then gets the proxy's URL as its base URL, without `/v1`.

Shoal cannot read a login that Codex keeps in the keyring, with a
`cli_auth_credentials_store` other than `file`. Such a launch fails unless the
URL is named. For the built-in provider, set `OPENAI_BASE_URL` in the tool's
`env`. For a provider of the user's, set its `base_url`.

### Point the agent at the proxy

The agent's wrapper runs one proxy on a loopback port for as long as the agent
runs, and points the agent at it.

Claude Code gets the proxy's URL as `ANTHROPIC_BASE_URL` in the settings Shoal
passes, which win over its own settings and environment. Claude Code takes only
its last `--settings`, so the wrapper merges every one the launch has into one.
These are Shoal's, those forwarded, and those in a configured resume command,
each as JSON or as a file relative to the workspace. The wrapper merges objects
key by key, and lists, such as an event's hooks, one after another. It writes
the result to a file only the user can read, which lasts as long as the proxy.
Tokens in a private settings file thus never reach the command line.

Codex gets the proxy's URL as the `base_url` of the provider the launch
selects, which keeps its own credentials. On Codex's built-in OpenAI provider,
Shoal defines a provider with the same login instead. Shoal reads the selection
as Codex does, from these sources in order:

1. the user's `config.toml`;
2. the `<name>.config.toml` of the profile that `--profile` or its `profile`
   names, or a `[profiles.<name>]` table;
3. each `.codex/config.toml` of a trusted project, from the project's root down
   to the working directory, which `-C` may move; `project_root_markers` finds
   the root, `.git` by default;
4. the `-c` overrides the launch has.

Shoal's own overrides come after the launch's, so they win.

### Proxy access

Any local user can reach a loopback port. Each proxy therefore answers only
requests that carry a key it draws when it starts, in an `x-shoal-proxy-key`
header. The upstream never sees that header.

The access key reaches the agent through its environment, which only its user
can read, never through its command line:

- Claude Code gets it in `ANTHROPIC_CUSTOM_HEADERS`, after any headers it has.
  An `ANTHROPIC_CUSTOM_HEADERS` in Claude's own settings replaces the one Shoal
  sets, and the proxy then refuses its requests.
- Codex's provider names `SHOAL_PROXY_KEY` in its `env_http_headers`.

### What the proxy retries

The proxy retries what the agent would otherwise fail on:

- a rate limit;
- an overloaded or failing upstream;
- an unreachable upstream;
- a stream that reports one of those before any content.

It waits a second at first, doubling up to half a minute. It waits longer when
the upstream asks: as long as its `Retry-After` says, until the reset time its
error names, or until Anthropic's `anthropic-ratelimit-unified-reset`. Shoal
reads that header when `anthropic-ratelimit-unified-status` is `rejected`, which
says the usage limit refused the request.

Once an outage has lasted five minutes, or a reset falls further away than
that, the agent gets the failure. Later requests get theirs at once, until one
succeeds. A failure a retry does not fix, or one after content arrived, reaches
the agent unchanged.

When the proxy gives up on a Codex stream, it ends the stream with a
`response.failed` event. The event carries the upstream's own error, such as a
usage limit and its reset. It ends Codex's turn, where an HTTP failure would
make Codex retry on its own. The wrapper reports that turn for
[turn retry](#turn-retry), which waits out rate and usage limits.

The wrapper reports a request the proxy waits to retry, and `status` shows it.
After an upgrade changes the daemon's protocol, the wrapper reports it through
the installed release. `--json` gives `proxy_retry` with `failure`, `attempt`
and the Unix-seconds `next_at`.

### Claude Code through a proxy

On a claude.ai login, Claude Code treats a proxy's URL as a gateway. It then
leaves out Remote Control and remote managed settings.

Claude Code keeps its prompt cache for an hour only on a claude.ai login within
its usage limits, and for 5 minutes with an API key or auth token. A proxied
Claude agent therefore gets `ENABLE_PROMPT_CACHING_1H=1`, unless the
environment it runs with sets that variable. A lifetime in
`CLAUDE_CODE_PROMPT_CACHE_TTL` or Claude's `promptCacheTtl` setting still wins.

### Resume commands and restores

A configured `resume_command` gets none of Shoal's hooks. It still gets the
arguments that point it at the proxy, before a `--` or at its end.

An agent its wrapper restores in place, as `shoal restart` or an automatic
restore does, runs through the proxy its restore command was resolved with:

- the same proxy keeps running;
- another upstream or key replaces it, and so does another default address for
  a proxy without an upstream;
- a proxy removed since stops.

## Working directories

[`add_dir`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
gives an agent, fresh or restored, other working directories. A sandbox, or a
setting such as Claude Code's `blockReadsOutsideWorkingDirectories`, then lets
it work there:

- every agent and chat gets each `agent.dirs` entry;
- a workspace's agent gets the repository's workspaces directory, which holds
  the children `shoal create` makes, its
  [temporary directory](setup.md#temporary-directories) and the local config;
- a chat gets its temporary directory;
- a [setup](config.md#set-up-config) chat without a repository also gets the
  global config's directory.

## Review runs

Claude Code's
[`review`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
prints its JSON result with Claude's `--json-schema`. It loads no user, project
or local settings. It denies every tool but reading, searching and `git diff`,
`log`, `show` and `blame`. It runs commands in Claude's sandbox, so it stays
read-only whatever those settings allow. Without a sandbox on the machine, such
as bubblewrap on Linux, the review fails.

Codex's review runs `codex exec` in a read-only sandbox with `--output-schema`.

## Launchers and trust

An agent's launcher is the
[`command`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
of its AI tool. Overriding the `command` of Claude or Codex changes only the
executable or flags. Prompt templates, trust setup and exit alerts still
apply.

Claude Code, Codex and their launch profiles get Shoal's hooks and trust
entries. Other tools do without.

### Sandbox and permissions

Claude and Codex run with their own sandbox and approval settings. Every Codex
launch, chats included, adds `-c sandbox_workspace_write.network_access=true`,
so its commands reach the daemon's socket.

A Claude Code agent, in a workspace or a chat, may run the `shoal` commands
that report on and finish its assignment. It may also run
`shoal stop --processes` and `--leftovers`, with or without `--json`. The
daemon still limits these commands to its scope.

A workspace agent's Claude settings add an `autoMode.environment` entry, after
Claude Code's own (`"$defaults"`). The entry names the workspace's branch, its
commits and its PR as the agent's own work. Auto mode then lets the agent push
the rebased branch with `git push --force-with-lease`. Auto mode still judges
every command, other pushes included.

A workspace at a commit has no branch and gets no entry, and neither does a
chat. A branch renamed while the agent runs keeps its old name in the entry
until the agent's next launch.

### Trusted directories

Before a Claude or Codex launch, Happy's included, Shoal trusts the agent's
directories in the tool's user config:

- the repository's checkout, under which Claude Code looks up each worktree's
  trust;
- the repository's shared workspaces directory;
- a workspace placed outside that directory;
- for Claude, which asks about each, every other directory the launch passes
  with `--add-dir` that none of those covers, such as an `agent.dirs` entry;
- for a chat without a repository, `root_dir`.

Shoal creates the user config when it is missing, and keeps its other settings.
When the update fails, Shoal warns and still launches the agent.

### Where a tool keeps its config

Claude's user config is `~/.claude.json`, or `$CLAUDE_CONFIG_DIR/.claude.json`.
Codex CLI and app read `~/.codex/config.toml`, or `$CODEX_HOME/config.toml`.
Trust entries and skills follow these variables.

For a tracked agent, a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` in the space's
`[env]` takes precedence over Shoal's environment. One in the tool's
[`env`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools)
takes precedence over both. A desktop app uses Shoal's environment.

Unsetting a variable does not undo one that the agent's own settings file sets,
such as the `env` of Claude's `settings.json`. A separate `CLAUDE_CONFIG_DIR`
avoids that.

### Remove trust entries

Removing a repository removes its trust entries, whichever client removes it
and also for a [contribution](repo.md#contributions). Shoal removes them from
the default files. It also removes them from each file that the repository's
agents, chats and app handoffs ran with, including one that an inherited
`CLAUDE_CONFIG_DIR` or `CODEX_HOME` names. Other project settings stay, and so
do the entries for `root_dir` and for directories outside the repository.

An entry Shoal cannot change, on removal or a
[move](repo.md#move-repositories-to-a-new-root), stays in place with a
`trust_left` alert. The removal or move still completes.

## Desktop apps

`shoal open` hands a space to a desktop app, in the directory its agent works
in:

- a workspace's worktree;
- the worktree or checkout a chat runs in;
- `root_dir` for a chat without a repository.

Without `--app`, it opens the space's
[`app.default`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#desktop-apps),
else the only installed desktop app. On a terminal, it offers a picker when
several are installed.

- `--app codex` and `--app t3` run `codex app <path>` or `t3 app <path>` with
  the arguments after `--`. They keep the launcher's output and exit code.
  T3's app must already be running.
- `--app claude` opens the Claude app through
  `open claude://code/new?folder=<path>`, only on macOS. It takes no arguments
  after `--`. The menu offers it once `/Applications/Claude.app` is installed.

Desktop apps get no agent flags. They have no execution tracking, scope token,
port variables or output default, so automatic cleanup cannot see their
activity.

## Happy sessions

Happy sessions are an optional integration, off until
[`agents.happy.enabled = true`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools).
While it is off, pickers and completion leave Happy out, and launches are
refused before any work starts.

`start --agent happy-claude` or `--agent happy-codex` starts a
[Happy](https://github.com/slopus/happy) session as Happy's own daemon would.
It runs `happy <agent> --happy-starting-mode remote --started-by daemon`, then
your arguments after `--`. The session thus registers with that daemon and
appears in the Happy app. With `--in <workspace>`, it starts in an existing
workspace.

The session runs detached from your terminal. Its stdin is `/dev/null`. Its
output goes to `<state>/workspaces/<id>/happy-<agent>-<time>.log`, which Shoal
deletes with the workspace.

The session runs through the tracked wrapper, in a background `shoal` process.
It gets the scope token and port variables, and counts as activity for idle
cleanup. `stop`, `rm`, PR cleanup and reconciliation treat it like any other
command.

Shoal returns once the daemon records the launch, printing the execution, PID
and log. `--json` adds `happy_session_id` and `happy_daemon_recorded`.

Without `~/.happy/daemon.state.json`, Shoal warns that the session will not
appear in the app until `happy daemon start` runs, and launches anyway.
`$HAPPY_HOME_DIR` overrides `~/.happy`.

### The first prompt

An `--issue` prompt reaches Claude as an argument. `happy codex` takes no
prompt argument, so Shoal delivers the prompt as the Happy app does:

1. With the login in `~/.happy/access.key`, Shoal creates the session on
   Happy's server, encrypted as happy-cli would encrypt it. The server is
   `$HAPPY_SERVER_URL`, else the one in Happy's settings file, else Happy's
   default.
2. It starts `happy codex` attached to that session, through Happy's own
   `HAPPY_RECONNECT_*` variables.
3. It waits up to 90 seconds for the session to report alive.
4. It posts the prompt with `curl`, passing the token on stdin.

When Happy's server cannot create the session, such as on a machine not logged
in to Happy, the launch fails before `happy codex` starts. When the session
does not receive the prompt, Shoal stops it, deletes its session on Happy's
server, and fails the launch. Either way, the workspace, queue entry or chat
stays as any other failed launch leaves it.

## Agent accounts

[`agent.git_profile`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent)
names one of the global [Git profiles](repo.md#git-profiles) for agents' Git
commands.

### Forge requests

Issue and PR changes, and other forge writes Shoal makes for a workspace or a
chat, use the agent account. The agent account is the host's
[`agent_token`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#forges),
with `agent_email` for a Bitbucket API token. Without an `agent_token`, Shoal
refuses the change and names `agent_token`. Set
`agent_token = { same_as = "user_token" }` for agents to make changes with your
login.

Changes you make from a registered checkout use your `user_token`.

Processes inside a workspace, and a chat's agent, read issues, PRs, reviewers,
labels, reactions and the forge's capabilities as the agent account. On a host
without an `agent_token`, they read with your `user_token`. Anyone else reads
with your own logins.

### Git over HTTPS

Git over HTTPS to a host with an `agent_token` also uses that token, as the
agent account. It does so for tracked agents in workspaces and chats, and for
the pushes Shoal runs for them: `shoal pr open` and approved pushes. It covers
fetches, pulls and pushes to every repository of that host.

Their Git settings clear your credential helpers for that host only, and add
Shoal's own. Shoal's helper asks the daemon for the token each time, so a
changed token applies at once. A new host applies from the agent's next start.

Git asks Shoal's helper before its first request to that host. With Git 2.46 or
newer, a login for that host in your `.netrc` thus goes unused. Older Git sends
that login first.

The token stays out of the agent's environment and Git config. Your helpers
neither answer for that host nor save the token. While the daemon cannot
answer, as during a restart, Git on that host fails instead of using your login
or asking for a password.

Your own terminal, `shoal merge` and the daemon's fetches keep your login. A
Git profile that sets `credential.helper`, or a credential helper for the whole
host, takes precedence. SSH remotes never ask a helper.

### The agent Git profile

The agent Git profile reaches tracked agents through Git's `GIT_CONFIG_COUNT`
variables, after any inherited entries. It thus overrides every Git config file
for the agent and its nested commands, in any repository. The worktree's config
and other executions keep their settings.

When the profile sets `user.name` or `user.email`, the agent drops the
inherited `GIT_AUTHOR_*` and `GIT_COMMITTER_*` variables for that field. Push
credentials follow profile settings such as `core.sshCommand`. An undefined
profile fails the launch before the agent starts.

Where the agent's [`push` permission](approvals.md#approval-for-agent-pushes)
at launch is `"propose"` or `"deny"`, the same variables carry the rewrites
that hold or refuse its pushes.

## Agent secrets

[`[secrets]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#secrets)
gives tracked agents values. A value is written in the config, read from the
environment, or fetched by a command from your secret store. Commands run
without input.

### Environment secrets

A launch reads each secret without `tools` before the agent starts, and adds
its value to the agent's environment. Nested commands inherit it. A command
that fails, prints nothing or prints NUL stops the launch.

### Tool-scoped secrets

A secret with `tools` never enters the agent's environment. The launch puts a
launcher for each listed program first on a private `PATH`. Each run of the
program through that launcher reads the value, a written one from the daemon.
The launcher then runs the next program of that name on `PATH` with the value
set, or fails with the command's error. No file of the launch holds a written
value.

Launchers are cooperative: an absolute program path or a reset `PATH` bypasses
them.

A launcher serves the config of its agent's launch. Once a secret's `value` or
`tools` change, its runs fail until the agent restarts.

Other executions fetch no secrets of their own. Hooks started beneath an agent
drop the inherited values and launchers.

### Secret approval

A tool-scoped secret can set `requires_approval = true` and
`approval_lifetime`, as a [protected resource](approvals.md#resource-approvals)
does. An environment secret cannot, because Shoal fetches it before the agent
can ask.

`shoal catalog secret` lists the configured secrets with their tools and
approval settings, without their values. The agent requests access with
`shoal acquire secret NAME --reason ...`, and the user decides with
`shoal approvals`.

Each run of a listed tool in a workspace asks the daemon about every secret it
serves, under the current policy. It fetches a gated value only once the
workspace holds a grant for the secret's current `value` and `tools`.
Otherwise the run fails, naming the pending or denied request, or the `acquire`
command when there is none. A run that cleared `SHOAL_SCOPE_TOKEN` fails too.

`shoal release secret NAME` cancels the request or ends a lease grant.

## Agent state

```sh
shoal internal agent-state working   # Also waiting (for the user mid-turn) or idle (turn finished)
```

An agent's hooks report its turn state with this command. Integrations read
the state, instead of each inferring it from the terminal.

### State hooks

Shoal's terminal Claude Code and Codex launches register the state hooks with
the [alert hooks](alerts.md#delivery-hooks). These launches include
`start --agent`, [chats](chats.md) and resumed sessions. Happy sessions do not
register them.

- A session that starts, resumes or clears reports `idle`, so a new agent shows
  `waiting` until its first turn.
- A turn that ends reports `idle` with `--ends-turn <integration>`, which also
  reads the agent's [prompt cache lifetime](#prompt-cache).
- A turn that an API error ends reports `idle` through
  `shoal internal turn-failed --integration <name>`, which also reads the
  failure the hook event names.

Neither agent has a hook for an interrupted turn. Claude Code shows `working`
until it notifies that its prompt waits for input. Codex shows `working` until
its next turn ends. Codex has no hook for a permission prompt, so it reports
`working` instead of `waiting`.

Other agents can call the command from their own hooks. A JSON event on stdin
with a `session_id`, as Claude Code and Codex hooks pass, names the session to
[resume](load.md#restore-commands). The command reads only that first event, and stops waiting
after two seconds without input.

### Subagents

Claude Code's and Codex's `SubagentStart` and `SubagentStop` hooks report each subagent, by the
`agent_id` the hook event names, through
`shoal internal subagent --integration <name>`. A session that starts, resumes
or clears ends the subagents of the session before.

A tracked agent that is idle while one of its subagents runs counts as working
wherever Shoal decides whether to stop an idle agent, since stopping it ends
the subagent. Such a subagent is, for example, a background agent or a
workflow's. [Overload protection](load.md) and [sleep](sleep.md#idle-sleep)
decide this way. The agent's status still shows `waiting`.

A subagent whose stop never arrives counts until the agent's session starts
again or the agent exits. Once the agent's [proxy](#retrying-proxy) has passed a
request of its subagents in the current session, the subagent counts only
until none has passed for an hour.

### State in lists

`list` and `status` show `working`, `needs input` for `waiting`, or `waiting`
for `idle`, since an idle agent waits for the user or for activity that
[wakes it](alerts.md#wake-an-idle-agent). `status --json` and `list --json`
give `agent_state` with `state` and the Unix-seconds `since` it began.

A state reported from a tracked execution shows only while that execution runs.
An agent whose process died, such as in a reboot, shows none, and the state ends
with the execution's record. A state reported from outside lasts until the next
report or the space's removal.

A chat's state ends when the chat stops or is restored. A report that arrives
while the chat is stopped is not recorded.

Each change and each end of a workspace's state is a
[space event](log.md#space-events). Repeating the current state is no change.

Agent state never notifies, records completion or affects cleanup.

### Prompt cache

An idle report that ends a turn starts the agent's prompt cache lifetime. Until
the agent works again, `status` shows `Prompt cache` with when the cache
expires. The JSON adds `prompt_cache_expires_at` in Unix seconds. The `cache`
[schedule](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
reads it to restore agents with a warm cache first.

The lifetime is the AI tool's
[`prompt_cache_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools),
else what the agent's own settings give, else the tool's default. The hook reads
Claude Code's settings from the environment it runs in, which is the agent's
own. The first of these that applies gives Claude Code's lifetime:

1. 5 minutes with `FORCE_PROMPT_CACHING_5M`;
2. `CLAUDE_CODE_PROMPT_CACHE_TTL`;
3. `promptCacheTtl` in its user settings;
4. an hour with `ENABLE_PROMPT_CACHING_1H`;
5. 5 minutes with an API key, an auth token, `apiKeyHelper` or another
   provider, such as Bedrock;
6. an hour on a claude.ai login, which Shoal assumes is within its usage
   limits.

### Reports the daemon misses

A space's own process keeps each report it sends in the state directory until
the daemon answers. A report the daemon does not receive, which the hooks would
discard, stays there, even when the hook's time limit ends the hook first.

The daemon looks for such reports at its start, each time a wrapper reattaches
after a restart, and every 15 seconds. For each, it records an
[alert](alerts.md) that names the space and the error. It then records each
report in the order its hook made them, unless a report made later was recorded
first.

### Who reports

A scoped caller reports only for its own space, which its scope names wherever
it runs. `--space` selects another space for an unscoped caller.

## Terminal status

Where an agent runs in a terminal that reads the
[Program Status Protocol](https://www.superlogical.com/rex/docs/build/program-status)
(OSC 7501), such as a Rex block, Shoal reports its space's status there. It
reports on a record of its own, `id=shoal` with `app=shoal` and the space's
name as its title. It leaves the agent's own root record to an agent that
reports itself, as Claude Code does.

Before the agent starts, Shoal asks the terminal whether it reads the
protocol, followed by a device attributes request. A terminal that answers only
that request, such as tmux, zmx or Zellij, receives nothing. Neither does an
agent that runs detached. Input typed during the query, before the agent
starts, may be lost.

The first row that holds decides the report:

| Space | Report | Message |
|---|---|---|
| A [thread](threads.md) of the space waits for the user | `blocked`, `kind=question` | the thread's number and the first line of its latest post |
| An [approval request](approvals.md#approval-requests) of the space waits for the user | `blocked`, `kind=permission` | the request's number and what it asks for |
| The agent's turn failed and Shoal does not [retry](#turn-retry) it | `error` | the failure |
| `shoal done` recorded completion, between the agent's turns | `done` | the summary |
| A [ready mark](completion.md#ready-for-review) stands, between the agent's turns | `done` | the marked item |
| Shoal stopped the agent and brings it back on its own | `idle` | why it stopped |
| The agent's hooks report `working`, for an agent that does not report itself | `working`, with the share of its [steps](progress.md) done | its status line |
| The hooks report `waiting`, likewise | `blocked`, `kind=permission` | |
| The hooks report `idle`, likewise | `idle` | |
| Nothing above | `clear` | |

A thread or request that a coordinating agent holds blocks nothing. The report
changes as the space does.

The wrapper reports the agent's exit itself, also while the daemon is away. An
exit with an error replaces the report with `error` and the exit code or
signal. A clean exit clears a report of the hooks' turn.
