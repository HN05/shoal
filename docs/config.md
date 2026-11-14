# Shoal configuration

This document describes every configuration option. The
[command reference](reference.md) describes what Shoal does with them, and the
[usage guide](../README.md) how to start.

## Files and layers

Shoal reads TOML from these files, highest first, and resolves each option on
its own from the first file that sets it:

1. A repository's **local config**, `.shoal/config.toml` in the repository's own
   directory, beside its workspaces and `.checkout`
   (`~/shoal/my-project/.shoal/config.toml`). It is outside every Git tree, for
   a repository that won't accept a committed file or for settings only you
   want, and Shoal reads it in place on every request.
2. The **repository file** in a workspace's worktree, `.shoal.toml` or
   `.shoal/config.toml`; both together is an error. Where no workspace applies,
   such as for the seed build or a repository's agent limit, the registered
   checkout's file applies. A worktree file the current format rejects is
   ignored, with a warning in `shoal config show` and a notification.
3. The **global config**, `~/.config/shoal/config.toml` (or
   `$XDG_CONFIG_HOME/shoal/config.toml`), which `shoal daemon install` creates
   from [default.toml](../crates/shoal-core/configs/default.toml).
4. The **built-in default**.

The global file's `[machine]` table holds the settings that describe this
machine, and only the global file may set it. Everything outside it follows the
repository schema in every file, so a setting copies between them. An unknown
or misplaced key fails with its line and column, and an unknown value of a
typed option is an error.

A named entry, such as `[ports.web]`, `[resources.signing]`,
`[resource_pools.devices]`, `[demos.tui]` or `[secrets.TOKEN]`, replaces the one
below it as a unit, while each option of `[agents.<name>]` merges on its own.
An empty list in a higher layer replaces a lower one's, which turns inherited
entries off. In the local config, a relative hook path or `setup.command` names
a script in its `.shoal` directory; `setup.seed` stays relative to the worktree.

Mode instructions and rule text may also live in Markdown files; see
[modes and rules](#modes-and-rules).

`shoal config show` prints every effective option with the layer that set it,
`shoal config set` and `unset` edit the global file, or with `--repo` a
repository's local config, and `shoal config reference` prints this document;
see [inspect effective configuration](reference.md#inspect-effective-configuration).
`shoal config` edits apply to a running daemon at once. After editing the global
file by hand, run `shoal config reload`; an invalid file leaves the daemon's
previous settings in place. The CLI reads agent settings on each command, and
repository files apply on the next request.

Where an option takes a secret, such as a token or key, its value is the secret
as written, `{ env = "<variable>" }`, read from the environment of the process
that uses it, or `{ cmd = [...] }`, an argument array run without input whose
output is the value. Paths that start with `~/` name a path under the home
directory.

Any option's value may instead be `{ same_as = "<key>" }`, naming another key
of the same table, such as `end = { same_as = "start" }` under `[ports]`. The
option takes that key's value after every layer, unless a higher layer sets the
option itself. Both keys take the same type, and the other key must have a
value; a cycle, a key without a value or a value of another type is an error
naming both keys. A key in another table cannot be named.

## Machine

```toml
git_profile = "work"     # Top level: a Git profile for new worktrees; no default

[machine]
root_dir = "~/shoal"     # Parent of every repository's workspaces and URL clones
max_agents = "dynamic"   # Or a positive number, or "none"
max_jobs = "dynamic"     # Or a positive number, or "none"
schedule = "balanced"    # "balanced", "speed" or "cache"
load = "balanced"        # "light", "balanced" or "full"
keep_awake = false       # Keep the machine from sleeping while agents work
```

`root_dir` is absolute or starts with `~/`, outside Shoal's state directory,
every checkout and every repository directory; it applies to new registrations
after `shoal config reload`, and [`move root`](reference.md#move-repositories-to-a-new-root)
moves existing ones.

`max_agents` limits the agents that run at once on the machine, and `max_jobs`
the [CI jobs](reference.md#runner): `dynamic` starts them one at a time while
measured memory and disk leave room below the [warning thresholds](#overload),
a number is a fixed limit, and `none` admits everything. See the
[agent queue](reference.md#agent-queue). `schedule` decides what the queue
starts first within a priority: `balanced` work closest to finished,
`speed` the work with the longest chain of work waiting on it, and `cache`
restores of agents whose prompt cache has not expired; see
[queue order](reference.md#queue-order).

`load` says how hard Shoal may load the machine: `light` for a computer you
work on, `full` for one that only runs agents. It sets each CPU and memory
percentage under [`[machine.overload]`](#overload) that the file leaves out,
and so dynamic admission.

`keep_awake` keeps the machine from sleeping while an agent works or waits on
a watched PR; see [keeping the machine awake](reference.md#keeping-the-machine-awake).

`git_profile`, at the top level of any file, names the [Git profile](#git-profiles)
new worktrees get; `shoal start --git-profile` overrides it for one creation.

### Overload

```toml
[machine.overload]
poll_seconds = 2
cooldown_seconds = 5

[machine.overload.memory]
enabled = true                 # Opt out with false
used_percent = 95              # Linux only; 50–99
sustained_seconds = 0          # Stop on the first critical sample

[machine.overload.cpu]
enabled = false                # Opt in with true
used_percent = 90              # Aggregate busy time across all cores; 1–100
sustained_seconds = 300        # Five minutes

[machine.overload.disk]
enabled = true                 # Opt out with false
cleanup_free_gib = 5           # Remove idle cleanup candidates below this
stop_free_gib = 2              # Stop executions below this; 1 to cleanup_free_gib

[machine.overload.warning]
enabled = true                 # Message running agents before a stop
memory_used_percent = 85       # Linux; macOS warns at native warning pressure
cpu_used_percent = 80
cpu_sustained_seconds = 60
disk_free_gib = 10
repeat_minutes = 30            # Least time between repeats per signal and workspace

[machine.overload.recovery]
enabled = true                 # Applies only with a configured resume command
memory_used_percent = 80       # Linux; macOS requires normal pressure
cpu_used_percent = 75
sustained_seconds = 60
```

The percentages above are those of `load = "balanced"` without swap. Each
`load` sets these, and the memory warning level is also where dynamic admission
stops starting agents and CI jobs:

| `load` | CPU warning / stop / recovery | Memory warning / stop / recovery |
|---|---|---|
| `light` | 60 / 80 / 55 | 75 / 90 / 70, with swap 80 / 93 / 75 |
| `balanced` | 80 / 90 / 75 | 85 / 95 / 80, with swap 90 / 97 / 85 |
| `full` | 95 / 100 / 90 | 95 / 98 / 90, with swap 97 / 99 / 93 |

Memory percentages apply on Linux, where configured swap lets memory fill
further before the kernel kills processes; macOS uses its native pressure
levels. CPU usage is aggregate busy time across all cores, excluding I/O wait
and niced time. Durations are seconds, bounded to one day; polling, cooldown
and sustained CPU and recovery durations must be positive. Recovery thresholds
must be positive and below their stop thresholds; warning thresholds are not
checked against stop thresholds. What each threshold does is in
[overload protection](reference.md#overload-protection).

### Git profiles

```toml
[machine.git_profiles.work]
user.name = "Your Name"
user.email = "you@company.example"
user.signingKey = "your-key"
commit.gpgsign = true
```

Each profile names Git settings, strings, booleans or integers; omitted keys
keep Git's normal inheritance. Profiles cannot set repository layout or
extensions. The top-level `git_profile` selects one for new worktrees and
`agent.git_profile` one for agents' Git commands; see
[Git profiles](reference.md#git-profiles).

### Forges

```toml
[machine.forges."git.example.com"]
type = "forgejo"   # "github", "gitlab", "forgejo" (also Gitea) or "bitbucket"
user_token = { cmd = ["security", "find-generic-password", "-s", "forgejo", "-w"] }
agent_token = { cmd = ["op", "read", "op://dev/agent/token"] }

[machine.forges."bitbucket.org"]
user_token = { cmd = ["security", "find-generic-password", "-s", "bitbucket", "-w"] }
user_email = "you@example.com"   # An Atlassian API token's account; omit for an access token
```

`type` sets the API a host serves, which Shoal otherwise asks any host other
than `github.com`, `gitlab.com` and `bitbucket.org` once. `user_token` is your
login on the host, and `agent_token` the [agent account](reference.md#agent-accounts)'s,
which agents' changes need: `agent_token = { same_as = "user_token" }` makes
them with your login;
`user_email` and `agent_email` apply only to Bitbucket. A `{ cmd }` runs at most once
per operation that needs the token and for at most a minute, and its output is
never stored; an `{ env }` reads the daemon's environment for its own requests.
Without a token only public repositories are readable. See
[forges](reference.md#forges).

### Agent tools on this machine

```toml
[machine.agents.droid]
skill_dir = "~/.factory/skills"   # Where `shoal skill install` puts the skill

[machine.agents.claude]
proxy = "cliproxy"                # A [machine.proxies] entry its agents go through
```

`skill_dir` is absolute or starts with `~/`; see
[agent skill](reference.md#agent-skill-outside-project-repositories). A launch
profile inherits its base's `proxy`, and only Claude Code, Codex and their
launch profiles can take one.

### Proxies

```toml
[machine.proxies.cliproxy]
upstream = "http://127.0.0.1:8317"   # A gateway's server root; optional
key = { env = "CLIPROXY_API_KEY" }   # Optional; needs an upstream

[machine.proxies.retry]              # No upstream: only adds the retries
```

`upstream` is where Shoal's retrying proxy forwards an agent's API requests: a
gateway's server root, which Claude Code and Codex share. Without it, requests
go where they would without the proxy. A proxy that a Codex tool, or a launch
profile of one, names fails validation when its `upstream` ends in `/v1` or is
on `chatgpt.com`. With `key`, the proxy sends that key to the upstream as
`Authorization: Bearer` in place of the agent's own credentials; an `{ env }`
reads the environment the agent runs with, `[env]`, secrets and the tool's `env`
included. A `key` needs an `upstream`, since without one it would reach
Anthropic or OpenAI in place of the agent's login. See the
[proxy](reference.md#agents).

### Simulators

```toml
[machine.simulators]
max_booted = 2
max_devices = 4
idle_seconds = 120
allow_any = false       # true permits unconfigured --device requests with a --reason
default = "phone"       # Must name a profile; no default

[machine.simulators.profiles.phone]
device = "iPhone 17"
runtime = "iOS 26.5"    # Optional pin; otherwise the latest compatible installed runtime
requires_approval = false
approval_lifetime = "lease"
```

See [simulators](reference.md#simulators-macos) and
[simulator preferences](#simulator-preferences).

## Agents

### Agent

```toml
[agent]
default = "claude"       # The agent starts use without --agent; no default
model = "opus"           # For the default agent, over its tool's own
effort = "xhigh"         # For the default agent, over its tool's own
dirs = ["/tmp"]          # Directories every agent may read without a prompt
max_running = "dynamic"  # Or a positive number, or "none"
git_profile = "agent"    # A Git profile for agents' Git commands; no default
```

`default` names a built-in AI tool or one under `[agents]`. `dirs` entries are
absolute or start with `~/`, and an empty list gives none. `max_running` limits
one repository's agents within `machine.max_agents`; set globally, it applies to
each repository that sets none, and `dynamic` adds no repository limit.
`git_profile` names a [Git profile](#git-profiles) that reaches tracked agents
over every Git config file; see [agent accounts](reference.md#agent-accounts).

### AI tools

An `[agents.<name>]` table changes one of the
[AI tools Shoal includes](reference.md#agents) or adds another; a tool is an
agent when it is built in or has a `command` or `base`, and may take a built-in
or configured command's name, since agents are not commands.

```toml
[agents.claude]
command = ["claude"]

[agents.codex]
command = ["codex", "{args}"]
model = "gpt-5.5"
effort = "high"

[agents.droid]
command = ["droid", "{args}"]
resume_command = ["droid", "--continue"]             # Optional
instructions = ["--system-prompt", "{instructions}"] # Optional
resume = ["--resume", "{session}"]                    # Optional
resume_latest = ["--continue"]                        # Optional
fork = ["--resume", "{session}", "--fork-session"]    # Optional
add_dir = ["--add-dir", "{directory}"]                # Optional
model_args = ["--model", "{model}"]                   # Optional
effort_args = ["--reasoning-effort", "{effort}"]      # Optional
review = ["exec", "--schema", "{schema}", "{prompt}"] # Optional

[agents.claude-direct]
base = "claude"
env = { CLAUDE_CONFIG_DIR = "~/.claude-direct" }
env_remove = ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]

[agents.happy]
enabled = false         # Offer happy-claude and happy-codex
```

Each option merges on its own across layers, so a repository can replace
`command` and keep the global `resume`.

- `command` is the launcher, an argument array resolved through `PATH`, with
  the substitutions of [`[commands]`](#commands). `{prompt}` takes the rendered
  system text and the first message, separated by a blank line; without it,
  nonempty context becomes the first forwarded argument. Substitutions happen
  once and user arguments stay literal.
- `resume_command` restores a stopped agent's session without repeating its
  first prompt, with the same substitutions; `{prompt}` gives it the
  continuation prompt. Without one, `resume` restores session `{session}` and
  `resume_latest` the latest session in the workspace, built in for Claude and
  Codex; a tool with neither a `resume_command` nor these cannot be resumed.
- `instructions` passes the rendered system text apart from the first message,
  as `{instructions}`, or `{instructions_toml}` for a TOML string literal.
- `fork` starts a new session from a copy of session `{session}`, leaving that
  session unchanged; `{directory}` names the workspace it works in.
- `add_dir` gives an agent another working directory, `{directory}`, for each
  `agent.dirs` entry and the directories Shoal adds; an empty list gives none.
- `model` and `effort` choose the model and reasoning effort, passed unchanged
  through `model_args` and `effort_args`, which name `{model}` and `{effort}`;
  without them the tool uses its own defaults. The `[agent]` ones apply over
  these to the default agent, and `--model` and `--effort` over both.
- `review` runs a [local review](reference.md#local-reviews) once, without a
  session: the launcher's words before `{args}`, or all of them, then these
  arguments, which name the prompt `{prompt}` and the output schema as a file,
  `{schema}`, or its text, `{schema_json}`; `{output}` names a file the result
  goes to instead of standard output. A tool without one cannot review.
- `base` makes a tool a launch profile of another, which gives it its base's
  hooks, trust entries and waking, and each of its launcher, `skill_dir`,
  `instructions`, `resume`, `resume_latest`, `fork`, `add_dir`, `model`,
  `effort`, `model_args`, `effort_args`, `review`, `retry`,
  `prompt_cache_minutes` and `proxy` it does not set; a base may itself be a
  profile.
- `env` sets and `env_remove` removes variables for the tool's tracked agents
  and chats, after `[env]`, the agent account and secrets. A `CLAUDE_CONFIG_DIR`
  or `CODEX_HOME` set there must be absolute or start with `~/`, and cannot be
  removed.
- `enabled = false` turns a tool's agents off: pickers and completion leave them
  out, and launches, reviews and restores are refused until it is on again.
  Happy is off until `agents.happy.enabled = true`.
- `retry` resends a turn an API error ended; it defaults to `true` for Claude
  Code and its launch profiles and `false` for other tools. See
  [agents](reference.md#agents).
- `prompt_cache_minutes` sets how long the tool's prompt cache lives after its
  last request; without it, Claude Code's is what its environment and settings
  give, and every other tool's is 30 minutes.

What a tool needs of the machine goes under
[`[machine.agents.<name>]`](#agent-tools-on-this-machine).

### Desktop apps

```toml
[app]
default = "codex"   # "claude" (macOS), "codex" or "t3"; no default
```

`default` is the app `shoal open` opens without `--app`.

### Modes and rules

```toml
[modes.review-fixer]
kind = "workspace"       # "workspace" or "chat"
repository = true        # false: a chat without a repository
rules = ["work", "worker"]
instructions = "Fix the findings of the latest review."
focus = true             # Over terminal.focus for its launches; unset follows it
idle_wake_minutes = 60   # Unset or 0 never wakes its agent
weight = "heavy"         # "light": no agent slot in a workspace; default "heavy", "light" for the built-in chat modes

[modes.review-fixer.permissions]
push = "propose"

[rules.worker]
text = "Follow repository instructions in {workspace} at {path}."
```

A mode decides what an agent is told and the kind of space it starts in. `kind`
is `workspace` or `chat`, and `repository = false`, only in the global config
and with `kind = "chat"`, starts a chat without a repository. `rules` names the
rules the agent's system text carries, in order; `instructions` follow the
agent's subject in its first message. `focus` decides whether its launches take
the terminal; set it for a mode whose agent needs your input to start, such as
one without instructions. `idle_wake_minutes` is how long its agent may stay
idle before Shoal wakes it. A `light` mode's agents in a workspace start at
once, hold no [agent slot](reference.md#agent-queue) and are never stopped for
load, as a chat's agent never is whatever its weight; the built-in modes that
start chats are light, so one set to `kind = "workspace"` stays so.
`permissions` takes the keys of
[`[permissions]`](#permissions). `[rules.<name>]` sets a rule's `text`, which
any mode can list; a mode naming a rule no layer defines is refused.

[Modes and rules](reference.md#modes-and-rules) lists the modes and rules Shoal
ships. `worker` always starts in a workspace, and the built-in `setup` mode
cannot be configured.

A mode's instructions may also be written as `modes/<name>.md` and a rule's text
as `rules/<name>.md`, beside the global `config.toml`, beside the local config,
or in the repository root or its `.shoal/` directory; a file names its mode or
rule, which it adds when no table declares it. A file in more than one of a
layer's directories is an error naming both, and inline TOML wins over Markdown
at each level. A repository's text is appended after the global text with a
blank line between nonempty texts, and the local config's wins over the
worktree's; an empty repository value suppresses the repository's additions,
and an empty global value suppresses the built-in text. Texts are read at
launch. Rules and instructions may use `{workspace}`, `{branch}` and `{path}` of
the space, `{repo}` and `{head}` for a chat, `{workspace_number}` for a chat in
a workspace's worktree, `{updates}`, how the agent receives issue and PR
activity, `{issue.number}`, `{issue.title}` and `{issue.url}` of the subject's
issue, and `{relabel}`, the sentence that swaps the `[issue]` `plan_label` for
`planned_label`; unknown placeholders stay literal.

### Permissions

```toml
[permissions]
issue_open = "direct"                 # "propose" or "direct"
pr_open = "direct"                    # "propose" or "direct"
issue_other_repositories = "propose"  # "deny", "propose" or "direct"
merge = "propose"                     # "propose" or "direct"
push = "direct"                       # "deny", "propose" or "direct"
api_writes = "deny"                   # "deny", "propose" or "direct"
daemon = "deny"                       # "deny", "propose" or "direct"
admin = "propose"                     # "deny", "propose" or "direct"
limits = "propose"                    # "deny", "propose" or "direct"
user_holds = "propose"                # "deny", "propose" or "direct"
notifications = "deny"                # "deny" or "direct"
decide = "deny"                       # "deny" or "direct"
ci_approve = "propose"                # "deny", "propose" or "direct"
```

Permissions say what agents may do: `"direct"` at once, `"propose"` after an
approval, and `"deny"` never, in that order from widest to narrowest.
`issue_open` and `pr_open` cover issues and PRs in the space's repository,
`issue_other_repositories` issues in other repositories, `merge` `shoal merge`
and `unmerge`, and merging a PR, its automatic merge and deleting its branch, `push` pushes to the repository's remotes by `shoal pr open` or
`git push`, `api_writes` `shoal forge api` requests other than reads,
`daemon` installing, starting, stopping and restarting the daemon and
`shoal update` while a daemon of the same Shoal answers to decide it, `limits` starting agents regardless of `max_agents` and
memory, `user_holds` releasing the user's holds and removing the spaces the
user holds, `notifications` reading your notifications, Shoal's own log and
the inbox's daemon-wide problems, `decide` deciding the approval requests of
the spaces below as you would, `ci_approve` approving CI runs that wait for
approval, and `admin` the rest of administering Shoal: changing its
config, registering, renaming and removing repositories and their local
config and kept branches, `shoal cleanup`, `shoal setup --skip`,
`shoal contribute`, adopting worktrees, and restoring, copying and moving
workspaces. An agent sets a permission in config only to a value its space
holds, and leaves replacing the config and `config setup` and `repo setup`
to you.

`[permissions]` and `[modes.<name>.permissions]` take the same keys. A mode's
key resolves, highest first, from its `permissions` in any layer, then
`[permissions]` in any layer, then the built-in mode's default, then the key's
default above; `chat`, `plan` and `orchestrate` default
`issue_other_repositories` to `"direct"`, and `orchestrate` defaults `merge`,
`admin`, `limits`, `user_holds`, `notifications`, `decide` and `ci_approve` to
`"direct"` and `api_writes` and `daemon` to `"propose"`. A space's own permissions
are then narrowed to its parent's and set by `--permission`; see
[mode permissions](reference.md#mode-permissions).

### Secrets

```toml
[secrets.OPENAI_API_KEY]
command = ["security", "find-generic-password", "-s", "shoal-agent-openai", "-w"]

[secrets.AGENT_BROWSER_ENCRYPTION_KEY]
command = ["op", "read", "op://Agents/agent-browser/key"]
tools = ["agent-browser"]
requires_approval = false
approval_lifetime = "lease"   # Or "workspace"
```

Each entry is named after the variable that carries the value, and its `command`
is an argument array whose standard output, less one trailing line break, is
the value. Without `tools`, the value joins every tracked agent's environment;
with `tools`, only runs of those programs receive it. Only a tool-scoped secret
takes `requires_approval` and `approval_lifetime`, as a
[protected resource](#approvals) does. Shoal stores the commands, never the
values. See [agent secrets](reference.md#agent-secrets).

### Sleep

```toml
[sleep]
idle = false   # Put any idle agent to sleep once its prompt cache expires
```

See [sleep](reference.md#sleep).

### Messages

```toml
[messages]
settle_seconds = 5   # At most 60; 0 delivers each message at once
```

`settle_seconds` holds an agent's unread messages until none has arrived for
that long, and at most six times as long after the first; see
[agent messages](reference.md#agent-messages).

## Terminal

```toml
[terminal]
focus = true             # Unset: focus only launches waiting for input
close_when_done = true
default_host = "zmx"     # "zmx", "herdr", "rex", "tmux" or "zellij"
```

`focus` decides whether the places `shoal start` opens take the terminal:
unset, a workspace shell or an agent waiting for its first prompt takes it and
an agent with input starts in the background; a [mode's](#modes-and-rules)
`focus` overrides it for its launches. `close_when_done` closes the place Shoal
opened for a workspace once `done` keeps the workspace and the agent finishes
its turn. `default_host` is where launches from a terminal no host serves run:
detached zmx sessions, tabs on the local Herdr server, windows on the local Rex
server, windows on the default tmux server, or Zellij sessions of their own;
turning that host off runs them in place. Each host's table below overrides
`close_when_done` for its own places; see [agents](reference.md#agents).

### Herdr

```toml
[terminal.herdr]
enabled = true             # false treats a Herdr pane as a plain terminal
new_tab = true             # Open starts in a new tab inside Herdr
workspace_name = "{repo}"  # The workspace for tabs Shoal opens outside Herdr
tab_name = "{repo}: {branch}"  # Optional
close_when_done = true     # Optional; over terminal.close_when_done
```

`tab_name` supports `{repo}`, `{branch}`, `{issue_number}`, `{issue_title}` and
`{name}`, the space's name, which `{title}` also gives, or a literal name; issue
fields are empty without an issue, unknown placeholders stay literal and
inserted values are never expanded again. Without it a tab is labelled
`<repo>#<number>` for an issue, otherwise with its branch, and a space's name
replaces either. `workspace_name` takes only `{repo}`. See
[Herdr](reference.md#herdr).

### Rex

```toml
[terminal.rex]
enabled = true             # false treats a Rex terminal as a plain terminal
new_window = true          # Open windows beside the caller
placement = "window"       # "split" opens beside the caller's block instead
session_name = "{repo}"    # The session for windows Shoal opens outside one
window_name = "{repo}: {branch}"  # Optional; as tab_name
close_when_done = true     # Optional
```

`session_name` takes only `{repo}`. See [Rex](reference.md#rex).

### tmux

```toml
[terminal.tmux]
enabled = true             # Unset: only when default_host = "tmux"
new_window = true          # Open windows beside the caller
placement = "window"       # "split" opens beside the caller's pane instead
session_name = "{repo}"    # The session for windows Shoal opens outside tmux
window_name = "{repo}: {branch}"  # Optional; as tab_name
close_when_done = true     # Optional
```

`session_name` takes only `{repo}`. See [tmux](reference.md#tmux).

### Zellij

```toml
[terminal.zellij]
enabled = true             # Unset: only when default_host = "zellij"
new_tab = true             # Open tabs beside the caller
placement = "tab"          # "split" opens beside the caller's pane instead
tab_name = "{repo}: {branch}"  # Optional
close_when_done = true     # Optional
```

See [Zellij](reference.md#zellij).

## Workspaces

### Setup

```toml
[setup]
command = "scripts/setup.sh"             # Prepares a new worktree; must exit 0
copy_ignored = true                      # Copy the checkout's ignored files first
copy_ignored_exclude = ["node_modules/"] # Gitignore-style patterns not to copy; default []
presets = ["cargo", "bun"]               # Build tools to set up; default []
```

`command` is one executable path, run without shell parsing or `PATH` lookup,
relative to the worktree's root; it gates readiness. `copy_ignored` copies the checkout's ignored files
into a new worktree before it, limited by a `.worktreeinclude` in the checkout,
and `copy_ignored_exclude` leaves out what its patterns match.

`presets` names the build tools a repository uses: `cargo`, `npm`, `pnpm`,
`yarn`, `bun`, `go`, `xcode`, `zig` and `ccache`. The global list applies to
every repository that names none, and `presets = []` turns it off. Presets that
keep the same seed output cannot be combined, and a variable `[env]` sets takes
precedence over a preset's. See [build tool presets](reference.md#build-tool-presets)
and [setup and hooks](reference.md#workspace-setup-and-hooks).

### Seed builds

```toml
[setup.seed]
command = "scripts/seed.sh"  # In the seed worktree; builds the default branch
paths = ["target"]           # Build outputs, relative to the worktree
```

`command` is an executable inside the seed worktree, given as a relative path;
it replaces the presets' commands, and `paths` their outputs. See
[seed builds](reference.md#seed-builds).

### Hooks

```toml
[hooks]
pre_setup = "scripts/before.sh"       # Before tracked setup, without a terminal
post_setup = "scripts/attach.sh"      # After the workspace is ready
post_agent_exit = "scripts/exited.sh" # When a tracked agent exits
post_done = "scripts/done.sh"         # After assignment completion, before cleanup
post_ready = "scripts/ready.sh"       # After work is marked ready for review
pre_remove = "scripts/detach.sh"      # Before the worktree is removed
post_remove = "scripts/removed.sh"    # After removal, from the repository checkout
post_resource_acquire = "scripts/acquired.sh"  # After a permit is recorded
pre_resource_release = "scripts/release.sh"    # Before a permit is freed
```

Each value is one executable path, run directly without shell parsing or `PATH`
lookup; give scripts a shebang and put arguments and shell logic inside them.
Hooks that run while the worktree exists resolve relative paths against its
root, `post_remove` against the repository checkout. A repository value
replaces the global one. See [setup and hooks](reference.md#workspace-setup-and-hooks)
and [resource hooks](reference.md#resource-hooks).

### Environment

```toml
[env]
AGENT_BROWSER_SESSION = "{workspace}"
AGENT_BROWSER_PROFILE = "{workspace_path}/.agent-browser"
GOCACHE = "{cache.go}"
```

`[env]` adds variables to tracked executions, lifecycle hooks and exported
environments. Values substitute `{workspace}`, the name of the workspace's
directory, `{workspace_path}` (also `{path}`), `{repo}`, `{branch}`, reserved
`{port.NAME}` and [`{cache.NAME}`](#shared-caches) once; unknown fields stay
literal, inserted values are not expanded, and neither are inherited variables
or shell expressions. Names are ASCII identifiers, and `SHOAL_*` names are
reserved; values are strings without NUL, and an empty string sets an empty
value. Reserved port exports take precedence. See
[scoped workspace commands](reference.md#scoped-workspace-commands).

### Shared caches

```toml
[caches.go]
scope = "shared"  # Required; the only scope
```

Each `[caches.NAME]` is a directory every workspace of the repository shares,
which `{cache.NAME}` names in `[env]`; names are lowercase like port names. Use
it only for tools that lock or address their entries by content, never for
build output directories. See [shared caches](reference.md#shared-caches).

### Commands

```toml
[commands]
check = ["cargo", "test"]
edit = ["zed", "{path}"]
review = ["tuicr", "-r", "{diff_base}..HEAD"]
review-worktree = ["tuicr", "-w"]
```

Each entry is a shortcut `shoal run <name>` runs, an argument array whose
executable resolves through `PATH`, with the workspace as working directory and
without shell expansion. A higher layer replaces the whole array. `{workspace}`,
`{branch}`, `{path}` and `{diff_base}`, the fork point `shoal diff` compares
against, expand once; a standalone `{args}` inserts the forwarded arguments
there, otherwise they are appended. An entry may take an agent's name.
`edit` is what `shoal edit` runs, and `review` the manual review of
`shoal review run`. See [configured commands](reference.md#configured-commands).

### Demos

```toml
[demos.tui]
cmd = ["cargo", "run", "--", "inbox"]  # Run in the workspace, restarted when it exits
open = ["scripts/open-demo.sh"]        # Optional: what `shoal demo tui` runs instead

[demos.web]
preset = "web"                         # "server", "web" or "simulator"
cmd = ["npm", "run", "dev"]
scheme = "https"                       # Optional: the URL's scheme; http by default
ports = ["web"]                        # Optional: named ports reserved before it starts
```

Names follow port names, except the `shoal demo` subcommands. `preset` says how
the demo reaches you, and `ports` and `open` replace its parts. See
[demos](reference.md#demos).

### Ports

```toml
[ports]
start = 49152           # The automatic range
end = 65535
on_conflict = "suggest" # Or "auto"

[ports.web]
port = 3000
env = "PORT"
reason = "Frontend dev server"
on_conflict = "auto"    # Optional; over the table's
requires_approval = false
approval_lifetime = "lease"
```

`start`, `end` and `on_conflict` are keys of the table itself, so no port can
take those names. A named port's `port` is preferred, `env` names the variable
that carries it instead of `SHOAL_PORT_<NAME>`, and `on_conflict = "auto"`
takes a free port instead of suggesting one. See
[port reservations](reference.md#port-reservations).

## Issues, PRs and merges

### Issues

```toml
[issue]
required_labels = ["type/"]  # Default: []
plan_label = "needs-plan"    # Default: unset
planned_label = "planned"    # Default: unset
assign = true                # Default: true
area_prefix = "area/"        # Default: unset
```

`required_labels` names entries that every new issue's labels must each start
with, so `area/` takes any area label; see
[repository conventions](reference.md#repository-conventions). `plan_label`
marks issues that need a plan and `planned_label` those whose plan is settled;
see [planning issues](reference.md#planning-issues). `assign` adds the account
that links an issue as assigned to the issue's assignees on the forge; see
[linked items](reference.md#linked-items-and-cleanup). `area_prefix` is the
prefix of the labels that name an issue's areas, so the
[agent queue](reference.md#queue-order) starts new workspaces whose issues share
fewer areas with existing workspaces first; unset, areas order nothing.

```toml
[priority.labels]   # Default: none
urgent = 75
"low-priority" = 25
```

`[priority.labels]` gives forge labels a priority from 0 to 100: a space or
launch given none ranks in the [agent queue](reference.md#queue-order) with the
highest its issues' labels map to.

### PRs

```toml
[pr]
draft = false                       # Open new PRs as drafts
required_labels = ["area/", "type/"] # Default: []
undraft_on_ready = true             # `shoal ready` takes a draft PR out of draft
draft_on_unready = false            # `shoal ready --undo` returns it to draft
progress = true                     # Show the space's progress in its PR bodies
```

`required_labels` works as for issues. See [issue and PR actions](reference.md#issue-and-pr-actions),
[ready for review](reference.md#ready-for-review) and [progress](reference.md#progress).

### Reviews

```toml
[review]
agent = "codex"            # Default: agent.default
fallback_agent = "claude"  # Optional; reviews once more after a failure
post = true                # Post a PR review's findings on the PR
require_for_pr = false     # A new PR needs a finished, resolved review of HEAD
require_for_ready = false  # `shoal ready` needs one too
```

See [local reviews](reference.md#local-reviews).

### Merges

```toml
[merge]
enabled = true    # false turns `shoal merge` off
confirm = true    # Ask before a merge that pushes
push = false      # Push the default branch after merging
```

See [merge into the default branch](reference.md#merge-into-the-default-branch).

### Conflicts

```toml
[conflicts]
sensitive = ["src/version.rs", "migrations/**"] # Default: []
```

`sensitive` lists, as paths and globs relative to the repository root, the files
that hold a number each change picks for itself, such as a schema migration or
protocol version; see [conflicts](reference.md#conflicts).

### Runner

```toml
[runner]
enabled = false       # Run the repository's CI jobs on this machine
labels = ["shoal"]    # shoal-<os>-<arch> is always added
permit = "builds"     # Optional: a resource pool each job holds one permit from
```

See [runner](reference.md#runner).

## Cleanup

```toml
[cleanup]
grace_minutes = 10   # 0 removes at once

[cleanup.auto]
enabled = true       # Remove idle, clean, pushed or merged workspaces
idle_minutes = 10

[cleanup.merged]
enabled = true       # Remove merged workspaces once unused, done or not
idle_minutes = 240

[cleanup.pr]
enabled = true       # Remove completed workspaces once every watched PR has merged

[cleanup.done]
enabled = true       # Clean up after `shoal done`; false keeps the space for review

[cleanup.restore]
retention_days = 30  # How long removed workspaces stay restorable
archive_max_mb = 50  # Uncommitted and untracked files a checkpoint keeps
```

`grace_minutes` is how long every unattended removal except for low disk space
waits. Global changes apply on reload, repository ones on the next sweep. See
[automatic cleanup](reference.md#automatic-cleanup),
[assignment completion](reference.md#assignment-completion) and
[checkpoints and restore](reference.md#checkpoints-and-restore).

## Resources

### Resource pools

```toml
[resources.signing]            # Standalone mutex; raise capacity for a semaphore
capacity = 1
reason = "Signing service"
kind = "semaphore"             # Or "rwlock"
requires_approval = false
approval_lifetime = "lease"

[resource_pools.devices]
capacity = 2                   # Total permits across the pool

[resource_pools.devices.resources.alpha]
capacity = 1

[resource_pools.devices.resources.beta]
capacity = 2                   # Within the pool limit
```

Global resources are shared across repositories, a repository's across its
workspaces; a repository cannot redefine a global name. Capacities are 1–65535
and default to 1, a pool's to its members' total, and every pool needs members.
Names are lowercase letters, digits, `_` or `-`, starting with a letter, at most
64 characters. `kind = "rwlock"` allows shared readers or one writer and needs
capacity 1. See [cooperative resource pools](reference.md#cooperative-resource-pools).

### Related repositories

```toml
[resources.server]
kind = "repo"
repo = "saldoir-server"        # Registered name or ID, or a remote URL
```

A resource of kind `repo` is a registered repository a workspace may read; its
capacity must be 1. See [related repository paths](reference.md#related-repository-paths).

### Simulator preferences

```toml
[simulators]
preferred = ["phone"]          # Profiles to try, in order
requires_approval = false
approval_lifetime = "lease"
```

`preferred` orders the [simulator profiles](#simulators) `shoal acquire sim`
tries, globally or per repository; flags override it.

### Approvals

`requires_approval = true` on a resource, named port, simulator profile,
`[simulators]` or tool-scoped secret makes a scoped acquisition an approval
request. `approval_lifetime = "lease"`, the default, ends the approval on
release; `"workspace"` permits later acquisitions of the same member and
settings until the workspace is removed. A protected machine profile cannot be
relaxed by repository defaults. See [resource approvals](reference.md#resource-approvals).
