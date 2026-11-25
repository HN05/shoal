# Terminal hosts

This file says how Shoal works in each terminal host it supports.

## Herdr

Inside Herdr (`HERDR_ENV=1`), interactive `shoal start` looks up
the issue and resolves repository, branch, and agent choices in the caller's pane,
then opens a tab in `HERDR_WORKSPACE_ID` and returns once the command is submitted
there. Setup and agent execution run in that tab, which reads the issue again and
is labeled `<repo>#<number>` for an issue (such as `shoal#375`) and with the
allocated workspace branch otherwise, unless `tab_name` supplies a template;
a [space's name](workspaces.md) replaces either.
`--here` keeps the command in the current pane; help and calls without a
terminal run in place, except that their agent starts detached as
[Agents](agents.md) describes.

[`[terminal.herdr]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#herdr) configures it. New tabs stay in the
background by default when launching an agent with an issue prompt or forwarded
arguments, and focus otherwise, so a workspace
shell or an agent waiting for its first prompt is ready to type into;
`terminal.focus` overrides this for every host's places alike, the
[mode's](modes.md) `focus` overrides that for its launches, and
`shoal start -f`/`--focus` focuses one launch and starts it at once, as `--now` does.
With `--json`, a start beside the terminal prints the planned `workspace` and
its `agent` once the host opens the launch's place. With `enabled = false` Shoal
treats a Herdr pane as a plain terminal. A `tab_name`'s `{branch}` is the
requested branch when the tab opens and the allocated branch after creation.
With `nested_zmx`, agents, chats and demos run in zmx sessions attached in
their panes, as [Agents](agents.md) describes, and Herdr reads no turn from an
agent without turn hooks there, as in a zmx session.

Failures before agent execution leave the tab open. Agent exits leave the tab
open so work can continue, and with shell integration its shell enters the
workspace at its next prompt; completion closes it once tracked executions resolve
or the agent finishes its turn, as its hooks report it or, for an agent without
them, as Herdr does, unless stopped work waits for `shoal resume` or the daemon
stayed stopped for more than two minutes while the tab was open. Removing its
workspace or chat closes it immediately, regardless of `terminal.close_when_done`, and daemon
startup and `shoal cleanup` close those whose workspace or chat is gone. Shoal closes
only the pane it opened the tab with, so a pane you split off keeps the tab
open, and a tab you opened stays open. A background tab `shoal resume` opens
closes the same way; a pane it reuses keeps its tab's behavior. `shoal create`
opens an interactive shell in the ready workspace and leaves the tab open until
that workspace is completed or removed.

With `[terminal] default_host = "herdr"`, launches from a terminal no host serves
open their tabs in the `workspace_name` workspace of the Herdr server the
terminal's `herdr` reaches, which must be running; Shoal reuses the first workspace with that label or creates
it. A focused start, a chat or an agent launch then attaches the terminal to
the Herdr session holding its tab, as `shoal attach` does, until you detach; a
background start says where it went. With `terminal.herdr.enabled = false` these
launches run in place, and `new_tab = false` keeps queued ones out of Herdr. A start queued from Herdr opens a background
tab in the workspace it was queued from, then falls back to the
`workspace_name` workspace on the same server, then to `terminal.default_host`.

## Rex

Inside a [Rex](https://www.superlogical.com/rex) terminal (`REX_BLOCK` is set),
`shoal start`, chats, background resumes and demos open a new window in the
Rex session that holds the block, placed and labelled as a Herdr tab would
be, and agents run in place in their block. With `placement = "split"`, a start
from your block other than a demo splits that block instead, opening to its
right in the same window; the new block takes the label and the window keeps
its name. Starts queued from Rex and starts from outside Rex still open
windows. Rex keeps sessions running without a connected app, so an agent keeps
running when the Mac app or the phone disconnects. Rex cannot set a new
terminal's environment: the new block starts with one short command, which
loads the worker and its variables from a private file and deletes it, so
nothing shows on screen or in shell history. Your login shell starts in the
block once the worker exits, as in a Herdr tab.

[`[terminal.rex]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#rex) configures it; with `enabled = false` Shoal
treats a Rex terminal as a plain terminal. With `nested_zmx`, agents, chats
and demos run in zmx sessions attached in their blocks, as [Agents](agents.md)
describes, so a terminal outside Rex, such as over ssh, attaches to them.

With `[terminal] default_host = "rex"`, launches from a terminal no host serves
open their windows in the `session_name` session on the local Rex server; Shoal reuses the oldest session with that label or creates it;
with `terminal.rex.enabled = false` these launches run in place, and `new_window = false`
keeps queued ones out of Rex.
Rex has no terminal client, so a focused start there selects the window in the
connected Mac Rex app and gives the terminal its shell back; without a
connected app it says to open Rex. A start queued from Rex opens a background
window in the session it was queued from, then falls back to the
`session_name` session on the same server, then to `terminal.default_host`.
Shoal finds `rex` in Rex's bin directory, on PATH, then in the Rex app, and
needs Rex API revision 7 or later.

`shoal attach` and `shoal status` find a workspace's or chat's Rex block by the
place its agent was recorded in: attach makes the block's window active inside
Rex and selects it in the Mac app from another terminal; a block that is gone
leaves nothing to attach. Shoal closes only the blocks it created, and never
their window's other blocks: completion and removal close them as they close a
Herdr tab, and daemon startup and `shoal cleanup` close those whose workspace
or chat is gone. A block you opened stays open, even after an agent ran there,
and one Shoal opened belongs to another workspace or chat once its agent
starts in it, and closes with that one instead. Rex reports
no agent turn state, so an agent without turn hooks closes its block once its
execution ends. Rex sends no push notifications to the phone, and a demo's
screen of a full-screen program shows only its current screen. A Rex app on
another device opens the `localhost` link a serving demo prints in its block
through a forward to the Rex server's machine.

## tmux

[tmux](https://github.com/tmux/tmux) is opt-in. With `terminal.tmux.enabled`,
inside a tmux pane (`TMUX` and `TMUX_PANE` are set) `shoal start`, chats,
background resumes and demos open a new window in the session the pane started
in, placed and labelled as a Herdr tab would be, and agents run in place in
their pane; `placement = "split"` splits your pane instead, as in Rex. The tmux
server keeps panes running without a client, so detaching (`prefix d`) or
closing the terminal leaves the agent running. The new window runs the agent
directly, with nothing typed on screen, then your login shell, which stays
after the worker exits and enters the workspace as a Herdr tab's shell does. A place of a host
that claims a terminal before tmux, such as a zmx session, keeps its own
behavior inside tmux. [`[terminal.tmux]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#tmux) configures it;
with `nested_zmx`, agents, chats and demos run in zmx sessions attached in
their panes, as [Agents](agents.md) describes.

With `[terminal] default_host = "tmux"`, which also turns `enabled` on, launches
from a terminal no host serves open their windows in the `session_name` session
on the default tmux server (`TMUX_TMPDIR` chooses its socket directory), which
Shoal starts when needed; Shoal reuses
the first session with that name, `.` and `:` replaced by `_`, or creates it
with a first window in your home directory. A
focused start, a chat or an agent launch then attaches the terminal to that
session until you detach. A start queued from tmux opens a background window
in the session it was queued from, then falls back to the `session_name`
session on the same server, then to `terminal.default_host`. Shoal needs tmux
3.3 or later.

`shoal attach` and `shoal status` find a workspace's or chat's pane by the place
its agent was recorded in, named after its window: attach selects the pane and
switches your tmux client to it, or attaches a client from outside tmux; a
pane on another tmux server is refused inside tmux. A window is renamed only
while the pane Shoal opened is alone in it. Shoal closes only the panes it
created, as it closes Rex blocks, and never their window's other panes, other
windows or sessions; a pane is recorded with its server's process and start
time, so a pane under the same ID after the server restarts is never Shoal's.
tmux reports no agent turn state, so an agent without turn hooks closes its
pane once its execution ends. Prompts and `shoal demo send` leave copy mode
first.

tmux draws panes through its own terminal emulator rather than passing output
through as zmx does, so terminal protocols work only as far as tmux supports
them: inline images (Kitty graphics, iTerm2 images) and OSC 9 and 777
notifications reach your terminal only with `set -g allow-passthrough on` and
a program that wraps them for tmux, and an image does not survive a redraw or a
reattach; OSC 52 clipboard writes from agents need `set -s set-clipboard on`,
since the default `external` forwards only tmux's own copies. Attaching redraws tmux's
own screen rather than replaying the output.

## Zellij

[Zellij](https://zellij.dev) is opt-in. With `terminal.zellij.enabled`, inside a
Zellij pane (`ZELLIJ_SESSION_NAME` and `ZELLIJ_PANE_ID` are set) `shoal start`,
chats, background resumes and demos open a new tab in your session, placed and
labelled as a Herdr tab would be, and agents run in place in their pane;
`placement = "split"` splits your pane instead. Each session's server keeps its
panes running without a client, so detaching (`Ctrl o d`) or closing the
terminal leaves the agent running. The new tab runs the agent directly, with
nothing typed on screen, then your login shell, which stays after the worker
exits and enters the workspace as a Herdr tab's shell does. A place of a host
that claims a terminal before Zellij, such as a tmux pane, keeps its own
behavior inside Zellij. [`[terminal.zellij]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#zellij) configures it;
with `nested_zmx`, agents, chats and demos run in zmx sessions attached in
their panes, as [Agents](agents.md) describes.

Zellij 0.45 drops a tab opened in a session no client shows, so a place for
such a session opens as the first pane of a background session of its own,
named after the place, up to 20 characters and suffixed with `-2`, `-3` while
another session, exited ones included, has the name, and its tab shows the
whole name. With `[terminal] default_host = "zellij"`, which also turns
`enabled` on, launches from a terminal no host serves open such sessions; a
focused start, a chat or an agent launch then attaches the terminal to its
session until you detach. A start queued from Zellij opens a background tab in
the session it was queued from while a client shows it, otherwise a session of
its own. `ZELLIJ_SOCKET_DIR` chooses the sessions Shoal reaches, as it does
for `zellij`, and from Zellij 0.46 so does the cache directory where Zellij
records session names, which `HOME`, or `XDG_CACHE_HOME` on Linux, sets: the
daemon reaches sessions started with its own. Shoal needs Zellij 0.45 or later.

`shoal attach` and `shoal status` find a workspace's or chat's pane by the place
its agent was recorded in, named after its tab: attach selects the tab and pane
and switches your Zellij client to its session, or attaches a client from
outside Zellij; a pane in another socket directory is refused inside Zellij. A tab is renamed only while the pane Shoal opened is alone in
it. Shoal closes only the panes it created, as it closes Rex blocks, and never
their tab's other panes or plugins, other tabs or sessions; it closes a tab
with the last pane there, and ends a session with the last pane there, as
Zellij does while a client shows the session. A pane is recorded with its
session and its server's process and start time, so a pane under the same ID
after the session restarts is never Shoal's. Zellij takes no condition with a
command, so Shoal checks the server just before each command on a recorded
pane. Zellij reports neither agent turn state nor the process in a pane's
foreground, so an agent without turn hooks closes its pane once its execution
ends, and a resume runs in a pane only while Zellij names your login shell
(`SHELL`), running no script or command, as what runs there.

Zellij draws panes through its own terminal emulator rather than passing output
through as zmx does, and handles terminal protocols itself: it forwards Kitty
graphics and Sixel images where your terminal supports them, OSC 9, 99 and 777
notifications as `host_notification_protocol` sets, and OSC 52 clipboard
writes; programs read no clipboard unless
`dangerously_enable_paste_buffer_read` is set. Other image protocols do not
reach your terminal, and attaching redraws Zellij's own screen rather than
replaying the output.

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

`--model` and `--effort` on `shoal start`, `contribute`, `review run` and
`repo setup` override the [configured model and effort](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) for
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
Trust entries and skills follow a `CLAUDE_CONFIG_DIR` or `CODEX_HOME` that a
tool's [`env`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools) sets. Unsetting a variable does not undo one
the agent's own settings file sets, such as the `env` of Claude's
`settings.json`, which a separate `CLAUDE_CONFIG_DIR` avoids.

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

`status` shows when an idle agent's [cache expires](agents.md#agent-state) from its
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
next launch. `shoal run <name>` for a `[commands]`
shortcut remains a plain command invocation without agent prompts or exit
alerts, expanding `{prompt}` to an empty string.

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
