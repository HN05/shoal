# Shoal configuration

This document describes each configuration option, its values and its
default. The [command reference](../../../../../../docs/reference/) says what
Shoal does with them, and the [usage guide](../../../../../../README.md) how to
start.

## Files and layers

Shoal reads TOML from these files, highest first. It resolves each option on
its own, from the first file that sets it:

1. A repository's **local config**: `.shoal/config.toml` in the repository's
   own directory, beside its workspaces and `.checkout`, such as
   `~/shoal/my-project/.shoal/config.toml`. It lies outside every Git tree.
   Use it for a repository that won't accept a committed file, or for
   settings only you want. Shoal reads it in place on every request, so an
   edit applies to every workspace at once. An invalid file fails each request
   until it is fixed. A
   [shared repository](../../../../../../docs/reference/hub.md) has the same
   local config on every machine of its hub.
2. The **repository file** in a workspace's worktree: `.shoal.toml` or
   `.shoal/config.toml`, never both. Where no workspace applies, such as for
   the seed build, the registered checkout's file applies. Shoal ignores a
   worktree file the current format rejects: `shoal config show` warns about
   it, and the daemon logs it and alerts you once per error while it runs.
   Resource leases are the exception, since the file may name their hooks.
   Acquiring or releasing one, and removing a workspace that holds one, fail
   until the file is fixed.
3. The **global config**: `~/.config/shoal/config.toml`, or
   `$XDG_CONFIG_HOME/shoal/config.toml`. `shoal daemon install` creates it
   from [default.toml](../../../../../shoal-core/configs/default.toml).
4. The **shared config**: `shared.toml` beside the global config, for the
   settings you want on every machine. It takes every key the global config
   takes, except `enabled`, `listen`, `url` and `connect` under
   `[machine.hub]`. It must be valid on its own. Paths, hooks and `{ cmd }`
   values in it resolve on each machine, except a `{ cmd }` whose `on` names
   the machine that runs it. Machines joined to a
   [hub](../../../../../../docs/reference/hub.md) keep it, and its mode and
   rule files in `shared/`, the same on each.
5. The **built-in default**.

The `[machine]` table holds the settings that describe this machine. Only the
global and shared configs set it, and their tables merge key by key. Every
other option follows the repository schema in every file, so a setting copies
between files. An unknown or misplaced key fails with its line and column. An
unknown value of a typed option is an error.

A named entry, such as `[ports.web]` or `[secrets.TOKEN]`, replaces the entry
of the same name below it as a whole. Each option of `[agents.<name>]` merges
on its own. An empty list in a higher layer replaces a lower layer's list,
which turns inherited entries off.

In the local config, a relative hook path or `setup.command` names a script in
its `.shoal` directory. `setup.seed` stays relative to the worktree.

The [`config` commands](../../../../../../docs/reference/config.md) show every
effective option with the layer that set it, edit each file and print this
document. The CLI reads agent settings on each command, and repository files
apply on the next request.

### Values

An option that takes a secret, such as a token or key, takes one of these
values:

- the secret as written;
- `{ env = "<variable>" }`, read from the environment of the process that uses
  it;
- `{ cmd = [...] }`, an argument array run without input, whose output is the
  value;
- `{ cmd = [...], on = "<machine>" }`, run on the machine of the
  [hub](../../../../../../docs/reference/hub.md) that `on` names, by name or
  ID. A secret set up there then serves every machine. This machine's daemon
  asks for the value each time, and keeps the last one only in memory, for
  while the named machine cannot be reached.

A secret written in `shared.toml` reaches every machine too, and sits in that
file on each.

A path that starts with `~/` names a path under the home directory.

Any option may instead take `{ same_as = "<key>" }`, which names another key
of the same table, such as `end = { same_as = "start" }` under `[ports]`. The
option takes that key's value after every layer, unless a higher layer sets
the option itself. Both keys take the same type, and the other key must have a
value. A cycle, a key without a value or a value of another type is an error
that names both keys.

## Machine

```toml
git_profile = "work"     # Top level: a Git profile for new worktrees; no default

[machine]
root_dir = "~/shoal"     # Parent of every repository's workspaces and URL clones
max_heavy_agents = "dynamic" # Or a number from 0
max_jobs = "dynamic"     # Or a number from 0
schedule = "balanced"    # "balanced", "speed" or "cache"
load = "balanced"        # "light", "balanced" or "full"
memory_limit = "24GiB"   # Or a share of RAM, such as "75%"; no default
keep_awake = "off"       # "off", "handover", "finish" or "on"
availability = "auto"    # "auto", "always_on" or "intermittent"
```

`root_dir` is the directory that holds every repository's directory. It is
absolute or starts with `~/`. It lies outside Shoal's state directory, every
checkout and every repository directory. A change applies to new
registrations; [`repo move`](../../../../../../docs/reference/repo.md#move-repositories-to-a-new-root)
moves existing ones.

`max_heavy_agents` limits how many agents of
[`heavy` modes](#modes-and-rules) run at once on the machine. `max_jobs`
limits the [CI jobs](../../../../../../docs/reference/ci.md#runner). For both,
`dynamic` starts one at a time while memory and disk stay below the
[warning thresholds](#overload). A number is a fixed limit, and 0 starts none.
See the [agent queue](../../../../../../docs/reference/queue.md).

`schedule` picks what the queue starts first within a priority:

- `balanced`: the work closest to finished.
- `speed`: the work with the longest chain of work waiting on it.
- `cache`: restores of agents whose prompt cache has not expired.

See [queue order](../../../../../../docs/reference/queue.md#queue-order).

`load` says how hard Shoal may load the machine: `light` for a computer you
work on, `full` for one that only runs agents. It sets each CPU and memory
percentage under [`[machine.overload]`](#overload) that the file leaves out.

`memory_limit` caps the memory that Shoal's
[work](../../../../../../docs/reference/load.md#work-slice) holds together. It
takes a size in `B`, `KiB`, `MiB`, `GiB` or `TiB`, or a percentage of RAM.
Without it, work may use the whole machine. On Linux the kernel enforces it;
elsewhere only [memory protection](#overload) does.

`keep_awake` picks what the machine does once you walk away from it:

- `off`: it sleeps as usual.
- `handover`: it admits nothing new, hands work over to other machines of the
  hub, then sleeps.
- `finish`: it admits nothing new, lets running turns end, then sleeps.
- `on`: it carries on, and stays awake while an agent works or waits on a
  watched PR.

See [keeping the machine awake](../../../../../../docs/reference/machine.md#keeping-the-machine-awake).

`availability` tells the [hub](../../../../../../docs/reference/hub.md)
whether this machine stays reachable: `always_on`, `intermittent`, or `auto`
for the hub's guess. It does nothing without a hub.

`git_profile`, at the top level of any file, names the
[Git profile](#git-profiles) new worktrees get. `shoal start --git-profile`
overrides it for one creation.

### Overload

```toml
[machine.overload]
poll_seconds = 2
cooldown_seconds = 5

[machine.overload.memory]
enabled = true                 # Opt out with false
used_percent = 95              # RAM and swap in use; Linux only; 50–99
stall_percent = 10             # Full memory stall; Linux only; 1–99
sustained_seconds = 10         # How long the stall must hold

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
memory_stall_percent = 10      # Some memory stall; Linux; at most the stop's
cpu_used_percent = 80
cpu_sustained_seconds = 60
disk_free_gib = 10
repeat_minutes = 30            # Least time between repeats per signal and workspace

[machine.overload.recovery]
enabled = true                 # Applies only with a configured resume command
memory_used_percent = 80       # Linux; macOS requires normal pressure
memory_stall_percent = 2       # Some memory stall over a minute; Linux
cpu_used_percent = 75
sustained_seconds = 60
```

The percentages above are those of `load = "balanced"`. Each `load` sets these
levels:

| `load` | CPU warning / stop / recovery | Memory used warning / stop / recovery | Memory stall warning / stop / recovery |
|---|---|---|---|
| `light` | 60 / 80 / 55 | 75 / 90 / 70 | 5 / 5 / 1 |
| `balanced` | 80 / 90 / 75 | 85 / 95 / 80 | 10 / 10 / 2 |
| `full` | 95 / 100 / 90 | 95 / 98 / 90 | 20 / 20 / 5 |

The memory warning levels are also where dynamic admission stops starting
agents and CI jobs.

Memory percentages apply on Linux; macOS uses its native pressure levels.

- Memory used is the share of RAM and swap together in use, counting
  reclaimable cache as free.
- Memory stall is the share of time tasks wait for memory, from the kernel's
  pressure stall information (`/proc/pressure/memory`). A kernel without it
  is measured by memory used alone.
- CPU usage is busy time across all cores, without I/O wait and niced time.
  Job admission counts niced time, in which jobs run, as busy.

Durations are seconds, at most one day. Polling, cooldown, and the sustained
CPU and recovery durations must be positive. Each recovery threshold must be
positive and below its stop threshold. The memory stall warning must be at
most its stop. [Overload protection](../../../../../../docs/reference/load.md)
says what each threshold measures and does.

### Git profiles

```toml
[machine.git_profiles.work]
user.name = "Your Name"
user.email = "you@company.example"
user.signingKey = "your-key"
commit.gpgsign = true
```

A profile names Git settings: strings, booleans or integers. Omitted keys keep
Git's normal inheritance. A profile cannot set repository layout or
extensions. The top-level `git_profile` selects one for new worktrees, and
`agent.git_profile` one for agents' Git commands. See
[Git profiles](../../../../../../docs/reference/repo.md#git-profiles).

### Forges

```toml
[machine.forges."git.example.com"]
type = "forgejo"   # "github", "gitlab", "forgejo" (also Gitea) or "bitbucket"
user_token = { cmd = ["security", "find-generic-password", "-s", "forgejo", "-w"] }
agent_token = { cmd = ["op", "read", "op://dev/agent/token"] }
review_token = { cmd = ["op", "read", "op://dev/reviewer/token"] }

[machine.forges."bitbucket.org"]
user_token = { cmd = ["security", "find-generic-password", "-s", "bitbucket", "-w"] }
user_email = "you@example.com"   # An Atlassian API token's account; omit for an access token
```

`type` sets the API a host serves. Without it, Shoal asks each host once,
except `github.com`, `gitlab.com` and `bitbucket.org`.

Each token takes the [value forms](#values) of a secret:

- `user_token` is your login on the host. Without a token, only public
  repositories are readable.
- `agent_token` is the
  [agent account](../../../../../../docs/reference/agents.md#agent-accounts)'s
  login, which agents' changes need. `{ same_as = "user_token" }` makes them
  with your login.
- `review_token` is the login agents approve and request changes on PRs
  with, as their [`pr_review` permission](#permissions) allows. It must not be
  the account that opened the PR, which forges refuse, so
  `{ same_as = "agent_token" }` suits only PRs agents did not open.

A token's `{ cmd }` runs at most once per operation that needs the token, for
at most a minute, and Shoal never stores its output. Shoal drops a token the
forge refuses, instead of keeping it while the machine `on` names is away.
`user_email`,
`agent_email` and `review_email` apply only to Bitbucket. See
[forges](../../../../../../docs/reference/issues.md#forges).

### Agent tools on this machine

```toml
[machine.agents.droid]
skill_dir = "~/.factory/skills"   # Where `shoal skill install` puts the skill

[machine.agents.claude]
proxy = "cliproxy"                # A [machine.proxies] entry its agents go through
```

`skill_dir` is absolute or starts with `~/`. See
[agent skill](../../../../../../docs/reference/install.md#agent-skill).
`proxy` names a [proxy](#proxies). Only Claude Code, Codex and their launch
profiles take one, and a launch profile inherits its base's.

### Proxies

```toml
[machine.proxies.cliproxy]
upstream = "http://127.0.0.1:8317"   # A gateway's server root; optional
key = { env = "CLIPROXY_API_KEY" }   # Optional; needs an upstream

[machine.proxies.retry]              # No upstream: only adds the retries
```

A proxy is Shoal's retrying proxy for an agent's API requests.

- `upstream` is where it forwards them: a gateway's server root, which Claude
  Code and Codex share. Without it, requests go where they would without the
  proxy. For a Codex tool, `upstream` must not end in `/v1` or be on
  `chatgpt.com`.
- `key` is sent to the upstream as `Authorization: Bearer`, in place of the
  agent's own credentials. An `{ env }` reads the environment the agent runs
  with. A `key` needs an `upstream`.

See the [proxy](../../../../../../docs/reference/agents.md).

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

See [simulators](../../../../../../docs/reference/resources.md#simulators-macos),
[simulator preferences](#simulator-preferences) and [approvals](#approvals).

### Webhooks

```toml
[machine.webhooks]
listen = "127.0.0.1:7473"   # No listener without it
```

`listen` is the address on which the daemon serves the routes of
[webhook triggers](#triggers). Shoal does not expose it to the internet: put a
tunnel or reverse proxy in front of it, such as Tailscale Funnel, cloudflared
or Caddy. See [webhooks](../../../../../../docs/reference/triggers.md#webhooks).

### Hub

```toml
[machine.hub]
enabled = true                           # This daemon takes the hub role
listen = "127.0.0.1:7474"                # The default
url = "https://shoal.example.ts.net"     # Default: http:// and the listen address
connect = ["ssh", "desk", "shoal", "daemon", "connect"]   # Overrides a joined machine's route
auto_update = true                       # The default
escalate_after_minutes = 30              # The default
default_machine = "desk"                 # Where a device's start goes when it names none
placement = "balanced"                   # The default; or "local" or "external"

[machine.hub.forward]
"/quay/" = "127.0.0.1:8080"              # A path prefix and the local server it reaches
```

These options configure the [hub](../../../../../../docs/reference/hub.md).

- `enabled` makes this daemon the hub that your other machines and devices
  join.
- `listen` is the address the hub serves HTTP and WebSocket on. Shoal handles
  no TLS. On loopback, the default, nothing reaches the hub until a tunnel or
  reverse proxy is in front of it, such as Tailscale Serve, cloudflared or
  Caddy.
- `url` is the address members reach the hub at.
- `connect`, on a joined machine, is a command whose standard input and
  output link the daemon to the hub. It replaces the URL the machine paired
  at. The command ends in `shoal daemon connect` on the hub, such as over
  `ssh` or `docker exec`.
- `auto_update`, on a joined machine, installs the hub's release when the hub
  refuses this machine for running another version.
- `escalate_after_minutes` is how long a thread or approval request waits for
  a coordinator whose machine is offline. Then it passes to the next space
  above with an agent on a connected machine, or to you.
- `default_machine`, on the hub, names the machine, by name or ID, that a
  device's start goes to when it names none.
- `placement`, on the hub, picks where a detached start that names no machine
  goes, as described below.
- `forward` maps path prefixes, each starting and ending with `/`, to local
  addresses, such as an app's own server. See
  [path forwarding](../../../../../../docs/reference/hub.md#path-forwarding).

Only a start that runs its agent detached is placed, such as a background
chat, a child an agent starts or a trigger's launch. A start that runs in your
terminal stays on its machine, and `--machine` always picks the machine.
`placement` takes these values:

- `local` keeps every start on the machine it was made on.
- `balanced` sends lasting work to an `always_on` machine with room. The
  machine must have the start's repository registered, matched by origin, and
  its agent tool. A heavy start goes to the machine whose admission would
  start a heavy agent now, with the most spare memory. A light start goes
  first to a machine with `max_heavy_agents = 0`, then to one where no heavy
  work waits. Without such a machine, the start stays where it was made.
  Placement never clones a repository; the start reports the machines it
  skipped and why.
- `external` asks the app registered as the placer. It falls back to
  `balanced` when no app is registered, or the app does not answer within two
  seconds.

### App store

```toml
[machine.app_store]
max_entry_mib = 16    # The default; at most 128
max_app_mib = 256     # The default
```

These are the limits of the [app store](../../../../../../docs/reference/apps.md)
this daemon keeps. `max_entry_mib` limits one entry, and `max_app_mib` all of
one app's entries together. Shoal refuses a write past either. On a joined
machine, the limits cover only its own entries; the hub's limits cover the
shared ones.

### Container runtime

```toml
[machine.containers]
runtime = "docker"      # Or "podman"; default: the first of them on PATH
```

`runtime` is the engine whose `compose` runs every workspace's
[containers](#containers).

## Agents

### Agent

```toml
[agent]
default = "claude"       # The agent starts use without --agent; no default
model = "opus"           # For the default agent, over its tool's own
effort = "xhigh"         # For the default agent, over its tool's own
dirs = ["/tmp"]          # Directories every agent may read without a prompt
max_heavy_running = "dynamic" # Or a positive number, or "none"
git_profile = "agent"    # A Git profile for agents' Git commands; no default
```

- `default` names a built-in AI tool or one under `[agents]`.
- `dirs` entries are absolute or start with `~/`. An empty list gives none.
- `max_heavy_running` limits one repository's heavy agents, within
  `machine.max_heavy_agents`. Set globally, it applies to each repository that
  sets none. `dynamic` adds no repository limit.
- `git_profile` names a [Git profile](#git-profiles) for tracked agents' Git
  commands, over every Git config file. See
  [agent accounts](../../../../../../docs/reference/agents.md#agent-accounts).

### AI tools

An `[agents.<name>]` table changes one of the
[AI tools Shoal includes](../../../../../../docs/reference/agents.md), or adds
another. A tool is an agent when it is built in or has a `command` or `base`.
It may share a name with a [command](#commands). A tool's name, in `[agents]`
and `[machine.agents]` alike, takes ASCII letters, digits, `-` and `_` and
starts with a letter or digit. An agent's name takes no uppercase letters and
starts with a lowercase one. No tool is named `all`, which
`shoal skill install` takes for every tool.

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
review = ["exec", "--allow", "shoal", "{prompt}"]     # Optional
review_resume = ["exec", "--resume", "{session}", "--allow", "shoal", "{prompt}"] # Optional
gate = ["exec", "--allow", "shoal", "--schema", "{schema}", "{prompt}"] # Optional
gate_resume = ["exec", "--resume", "{session}", "--allow", "shoal", "--schema", "{schema}", "{prompt}"] # Optional

[agents.claude-direct]
base = "claude"
env = { CLAUDE_CONFIG_DIR = "~/.claude-direct" }
env_remove = ["ANTHROPIC_BASE_URL", "ANTHROPIC_AUTH_TOKEN", "ANTHROPIC_API_KEY"]

[agents.happy]
enabled = false         # Offer happy-claude and happy-codex
```

Each option merges on its own across layers, so a repository can replace
`command` and keep the global `resume`. Placeholders are substituted once, and
user arguments stay literal.

- `command` is the launcher: an argument array resolved through `PATH`, with
  the placeholders of [`[commands]`](#commands). `{prompt}` takes the system
  text and the first message, separated by a blank line. Without `{prompt}`,
  nonempty text becomes the first forwarded argument.
- `resume_command` restores a stopped agent's session without repeating its
  first prompt. `{prompt}` gives it what the restored agent is told. An
  argument that is exactly `{prompt}` is left out when the agent is told
  nothing.
- `resume` restores session `{session}`, and `resume_latest` the latest
  session in the workspace, when there is no `resume_command`. Both are built
  in for Claude Code and Codex. A tool without any of the three cannot be
  resumed.
- `instructions` passes the system text apart from the first message, as
  `{instructions}`, or `{instructions_toml}` for a TOML string literal.
- `fork` starts a new session from a copy of session `{session}`.
  `{directory}` names the workspace it works in.
- `add_dir` gives an agent another working directory, `{directory}`, for each
  `agent.dirs` entry and each directory Shoal adds. An empty list gives none.
- `model` and `effort` choose the model and reasoning effort. `model_args` and
  `effort_args` pass them, naming `{model}` and `{effort}`. Without them, the
  tool uses its own defaults. The `[agent]` values apply over these to the
  default agent, and `--model` and `--effort` over both.
- `review` runs a
  [local review](../../../../../../docs/reference/reviews.md#local-reviews), or
  the reviewer's answer in a finding's thread. Its arguments follow the
  launcher's words before `{args}`, or all of them, and name the prompt
  `{prompt}`. `{output}` names a file for the result instead of standard
  output. The run is read-only but can run `shoal`. A tool without `review`
  cannot review.
- `review_resume` runs a review or answer again in session `{session}`, which
  an earlier run reported.
- `gate` runs the [gate](../../../../../../docs/reference/approvals.md#the-gate)
  once, with `review`'s placeholders and the schema of its result, as a file,
  `{schema}`, or as text, `{schema_json}`. A tool without `gate` cannot be the
  gate.
- `gate_resume` runs the gate again in session `{session}`, which an earlier
  run reported.
- A tool without `review_resume` or `gate_resume`, or whose runs report no
  session, starts a new session each time.
- `base` makes a tool a launch profile of another. The profile takes its
  base's hooks, trust entries and waking. It also takes each of these that it
  does not set: the launcher, `skill_dir`, `instructions`, `resume`,
  `resume_latest`, `fork`, `add_dir`, `model`, `effort`, `model_args`,
  `effort_args`, `review`, `review_resume`, `gate`, `gate_resume`, `retry`,
  `prompt_cache_minutes` and `proxy`. A base may itself be a profile.
- `env` sets and `env_remove` removes variables for the tool's tracked agents
  and chats, after `[env]`, the agent account and secrets. A
  `CLAUDE_CONFIG_DIR` or `CODEX_HOME` set there is absolute or starts with
  `~/`, and cannot be removed.
- `enabled = false` turns a tool's agents off. Pickers leave them out, and
  launches, reviews and restores are refused. Happy is off until
  `agents.happy.enabled = true`.
- `retry` resends a turn that an API error ended. It defaults to `true` for
  Claude Code and its launch profiles, and `false` for other tools.
- `prompt_cache_minutes` sets how long the tool's prompt cache lives after its
  last request. Without it, Claude Code's comes from its environment and
  settings, Codex's is 45 minutes, and every other tool's is 30 minutes.

What a tool needs of the machine goes under
[`[machine.agents.<name>]`](#agent-tools-on-this-machine). See
[agents](../../../../../../docs/reference/agents.md).

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
weight = "heavy"         # "light": no heavy slot; default "heavy", "light" for the built-in chat modes
label = "needs-fixes"    # Default: unset

[modes.review-fixer.permissions]
push = "propose"

[rules.worker]
text = "Follow repository instructions in {workspace} at {path}."
```

A mode decides what an agent is told and the kind of space it starts in.
[Modes and rules](../../../../../../docs/reference/modes.md) lists the modes
and rules Shoal ships. `worker` always starts in a workspace, and the built-in
`setup` mode cannot be configured.

- `kind` is `workspace` or `chat`.
- `repository = false` starts a chat without a repository. Only the global
  config sets it, with `kind = "chat"`.
- `rules` names the rules the agent's system text carries, in order. A mode
  that names a rule no layer defines is refused.
- `instructions` follow the agent's subject in its first message.
- `focus` decides whether the mode's launches take the terminal. Set it for a
  mode whose agent needs your input to start, such as one without
  instructions.
- `idle_wake_minutes` is how long the agent may stay idle before Shoal wakes
  it.
- `weight` decides how the mode's agents take the machine's capacity, in a
  workspace or a chat. A `light` agent starts at once, holds no
  [heavy slot](../../../../../../docs/reference/queue.md), and is never
  stopped for load or on idle. A `heavy` agent waits for a heavy slot. The
  built-in chat modes are light, also when set to `kind = "workspace"`. A chat
  without a repository takes its mode's weight from the global config.
- `label` is a forge label. An issue or PR that carries it starts in this mode
  when its launch names none. No two modes may have the same label, and the
  built-in modes have none.
- `permissions` takes the keys of [`[permissions]`](#permissions).

`[rules.<name>]` sets a rule's `text`, which any mode can list.

#### Text in Markdown files

A mode's instructions may also live in `modes/<name>.md`, and a rule's text in
`rules/<name>.md`. These directories go beside the global `config.toml`, in
`shared/` beside the shared config, beside the local config, or in the
repository root or its `.shoal/` directory. A file adds its mode or rule when
no table declares it.

- A file in more than one of a layer's directories is an error that names
  both.
- Inline TOML wins over Markdown in each file's layer.
- The global config's text wins over the shared config's.
- A repository's text follows the global text, after a blank line. The local
  config's text wins over the worktree's.
- An empty repository value suppresses the repository's text, and an empty
  global value the built-in text.

Shoal reads the texts at launch.

#### Placeholders

Rules and instructions may use these placeholders. Unknown ones stay literal.

- `{workspace}`, `{branch}` and `{path}` of the space.
- `{head}`: what a workspace has checked out, such as `on branch fix` or
  `at commit 3f2a9c1 (PR #8)`.
- `{repo}`, for a chat.
- `{workspace_number}`, for a chat in a workspace's worktree.
- `{updates}`: how the agent receives issue and PR activity.
- `{issue.number}`, `{issue.title}` and `{issue.url}` of the subject's issue.
- `{relabel}`: a sentence that has the agent swap its own mode's `label` for
  that of the mode that should carry out its plan, naming each mode with a
  label.

### Permissions

```toml
[permissions]
issue_open = "direct"                 # "propose", "gate" or "direct"
pr_open = "direct"                    # "propose", "gate" or "direct"
issue_other_repositories = "propose"  # "deny", "propose", "gate" or "direct"
merge = "propose"                     # "propose", "gate" or "direct"
push = "direct"                       # "deny", "propose", "gate" or "direct"
api_writes = "deny"                   # "deny", "propose" or "direct"
daemon = "deny"                       # "deny", "propose" or "direct"
admin = "propose"                     # "deny", "propose" or "direct"
limits = "propose"                    # "deny", "propose" or "direct"
user_holds = "propose"                # "deny", "propose" or "direct"
notifications = "deny"                # "deny" or "direct"
decide = "deny"                       # "deny" or "direct"
ci_approve = "propose"                # "deny", "propose" or "direct"
pr_review = "propose"                 # "deny", "propose" or "direct"
triggers = "propose"                  # "deny", "propose" or "direct"
```

Permissions say what agents may do. From widest to narrowest:

- `"direct"`: at once.
- `"gate"`: after the [gate](#gate) allows it. No built-in mode sets it.
- `"propose"`: after an approval.
- `"deny"`: never.

Each key governs these actions:

- `issue_open` and `pr_open`: opening issues and PRs in the space's
  repository.
- `issue_other_repositories`: opening issues in other repositories.
- `merge`: `shoal merge` of a PR or a branch, its automatic merge, and
  `unmerge`.
- `push`: pushes to the repository's remotes, by `shoal pr open` or
  `git push`, and deleting branches on the forge with `shoal branch delete`.
- `api_writes`: `shoal forge api` requests other than reads.
- `daemon`: installing, starting, stopping and restarting the daemon, and
  `shoal update`.
- `limits`: starting agents regardless of `max_heavy_agents` and memory.
- `user_holds`: releasing your holds, and removing the spaces you hold.
- `notifications`: reading your alerts, Shoal's own log and the inbox's
  daemon-wide problems.
- `decide`: deciding the approval requests of the spaces below, as you would.
- `ci_approve`: approving CI runs that wait for approval.
- `pr_review`: approving and requesting changes on PRs, as the host's
  [`review_token`](#forges).
- `triggers`: pausing, resuming and running [triggers](#triggers).
- `admin`: the rest of administering Shoal. That covers changing its config;
  registering, renaming and removing repositories, their local config and
  kept branches; `shoal cleanup`, `shoal setup --skip` and
  `shoal contribute`; adopting worktrees; restoring, copying and moving
  workspaces; and `shoal config setup` and `shoal repo setup`.

An agent sets a permission in config only to a value its space holds. It
changes a trigger only where its `triggers` is `"direct"`. Replacing the
config is yours.

`[permissions]` and `[modes.<name>.permissions]` take the same keys. A mode's
key resolves from the first of these that sets it:

1. the mode's `permissions`, in any layer;
2. `[permissions]`, in any layer;
3. the built-in mode's default;
4. the key's default above.

The built-in modes change these defaults:

- `chat`, `plan` and `orchestrate` set `issue_other_repositories` to
  `"direct"`.
- `orchestrate` sets `merge`, `admin`, `limits`, `user_holds`,
  `notifications`, `decide`, `ci_approve` and `pr_review` to `"direct"`, and
  `api_writes` and `daemon` to `"propose"`.

See [mode permissions](../../../../../../docs/reference/modes.md#mode-permissions)
for how a space's permissions follow from its mode's.

### Gate

```toml
[gate]
agent = "claude"          # Default: agent.default
fallback_agent = "codex"  # Optional
instructions = "Follow CONTRIBUTING.md."
escalate = ["merging into a release branch"]

[gate.push]
instructions = "Tags are pushed only by the release workflow."
escalate = ["a push to a tag"]
```

The gate is the agent that decides a request under a permission of
`"gate"`. See [the gate](../../../../../../docs/reference/approvals.md#the-gate).
Shoal reads this table from the repository's checkout and the layers beside
it, never from the requesting workspace's worktree.

- `agent` is the AI tool that runs the gate.
- `fallback_agent` decides once more when the first agent fails.
- `instructions` follow the gate's built-in instructions.
- `escalate` lists the cases the gate passes to you, each quoted word for word
  in its verdict. With none, the gate decides every request itself.

`[gate.<key>]` adds `instructions` and `escalate` for the requests one key
governs. It takes the keys that allow `"gate"`: `issue_open`, `pr_open`,
`issue_other_repositories`, `merge` and `push`.

### Secrets

```toml
[secrets.OPENAI_API_KEY]
value = { cmd = ["security", "find-generic-password", "-s", "shoal-agent-openai", "-w"] }

[secrets.AGENT_BROWSER_ENCRYPTION_KEY]
value = { cmd = ["op", "read", "op://Agents/agent-browser/key"] }
tools = ["agent-browser"]
requires_approval = false
approval_lifetime = "lease"   # Or "workspace"
```

Each entry is named after the variable that carries the value.

- `value` takes the [value forms](#values) of a secret. A `{ cmd }` value is
  its standard output, less one trailing line break.
- `tools` limits the secret to runs of those programs. Without it, the value
  joins every tracked agent's environment.
- `requires_approval` and `approval_lifetime` apply only to a secret with
  `tools`, as for a [protected resource](#approvals).

Shoal's state records where a value comes from, and a written value as its
digest, never the value itself. See
[agent secrets](../../../../../../docs/reference/agents.md#agent-secrets).

### Sleep

```toml
[sleep]
enabled = true          # Put idle agents to sleep once you are away from them
max_wait_minutes = 30   # The longest an idle agent you are engaged with waits for you
keep_leases_minutes = 30  # How long an asleep agent keeps its simulators and containers; 0 stops them as it sleeps
```

See [sleep](../../../../../../docs/reference/sleep.md) and
[leases while asleep](../../../../../../docs/reference/resources.md#leases-while-asleep).

### Alerts

```toml
[alerts]
settle_seconds = 5   # At most 60; 0 delivers each alert at once
```

`settle_seconds` holds an agent's unread alerts until none has arrived for
that long. See [alerts](../../../../../../docs/reference/alerts.md).

### Triggers

```toml
[triggers.triage]
on = "timer"
at = "09:00"                      # Or ["09:00", "14:00"]; or every = "1h"
days = ["mon", "tue", "wed", "thu", "fri"]  # Every day when omitted
timezone = "Europe/Oslo"          # The local time zone when omitted
catch_up = "2h"                   # One interval for every, 24h for at
mode = "chat"
agent = "claude"
name = "Morning triage"
task = "Triage the issues opened since {{ previous_at }}."
overlap = "skip"                  # Or "queue" or "parallel"
filter = [{ path = "trigger", equals = "triage" }]
key = "{{ scheduled_at }}"
key_window = "10m"

[triggers.labelled]
on = "webhook"
secret = "FORGEJO_HOOK"           # A [secrets] entry; none accepts any request
signature = "forgejo"             # Or "github", or "hmac-sha256" with signature_header
issue = "{{ payload.issue.number }}"
filter = [
  { path = "headers.x-forgejo-event", equals = "issues" },
  { path = "payload.issue.labels[].name", equals = "agent" },
]
```

A trigger starts an agent when its source fires, as `shoal start` would. See
[triggers](../../../../../../docs/reference/triggers.md).

A repository's trigger starts in that repository. A
[contribution](../../../../../../docs/reference/repo.md#contributions)'s
triggers come from its local config alone. A global trigger starts in the
registered repository its `repository` names. Without `repository`, it starts
in a chat without a repository. Only the global file sets `repository`.

A higher layer's trigger replaces a lower layer's of the same name. The
global file's triggers never reach a repository's. Names follow port names.

#### Sources

`on` names the source.

- `timer` fires `every` interval after its last firing, or `at` each time of
  day, as `HH:MM`. `days` takes `mon` to `sun`.
- `webhook` fires for each request to its route on the
  [webhook listener](#webhooks).

A timer's last firing outlives restarts. A time a daylight-saving change skips
fires at the next minute that exists, and a time it repeats fires once. Shoal
starts counting a trigger's times when it first reads the trigger, so a new
trigger never catches up on earlier times.

A webhook trigger's `secret` names a [secret](#secrets). A request must then
carry the HMAC-SHA256 of its body under that secret's value, in hex, where
`signature` says:

- `forgejo`: in `X-Forgejo-Signature`.
- `github`: in `X-Hub-Signature-256`, after `sha256=`.
- `hmac-sha256`: in the header `signature_header` names.

A repository's trigger takes its secret from that repository's local config,
and a global trigger from the global config. The secret reaches no agent.
Without `secret`, any process that reaches the listener fires the trigger,
this machine's agents included. Such a firing starts a space as you, whatever
that agent's own `triggers` permission.

Lengths of time, such as `every`, `catch_up` and `key_window`, are whole
numbers each followed by `s`, `m`, `h`, `d` or `w`, largest first, as in
`1h30m`.

`catch_up` covers the times a timer missed while the daemon was stopped or the
machine slept. On its return, the newest missed time fires once when it is at
most `catch_up` old. The times before it, or every missed time when the newest
is older, are recorded as one skipped run.

#### The launch

The launch takes the keys of `shoal start`'s flags: `agent`, `mode`, `model`,
`effort`, `branch`, `base`, `issue`, `pr` (a PR URL), `name`, `priority`,
`permission` (a table of `KEY = VALUE`, as `--permission` gives them) and `in`
(a workspace's ID). `task` is the text after `--`.

Each of these keys, and `key`, is a template. `{{ path }}` looks up a value of
the firing's input document, such as `{{ payload.labels.0.name }}`. A string
or number becomes text, and an object or array JSON.

A timer's input has:

- `scheduled_at` and `fired_at`;
- `previous_at`: the last firing, or before the first, when Shoal first read
  the trigger;
- `trigger`: its name.

Times are RFC 3339, in the trigger's time zone.

A webhook's input has:

- `payload`: the JSON body, or a form-encoded body's fields;
- `body`: the body's text;
- `headers`, with lowercase names;
- `query`, `received_at` and `trigger`.

#### Runs

`overlap` decides a firing that would start a new space while the trigger is
busy. A trigger is busy while the agent of its last started run runs or is
queued, including a new space's launch that still waits in the agent queue,
and while a queued run of the trigger waits. A launch with `in` starts no new
space and never meets `overlap`:

- `skip` records the run as skipped.
- `queue` records the run as queued and launches it once that agent stops. It
  skips every further firing while one run waits.
- `parallel` launches at once.

`filter` lists conditions on the input. Each is a `path` with one of `equals`,
`not_equals`, `in`, `starts_with` or `exists`. `[]` after a key, as in
`payload.labels[].name`, matches when any element does.

`key` names what a firing is about. A run whose key matches that of a run the
trigger started within `key_window` is recorded as a duplicate. The keys
outlive restarts until their window ends. A webhook trigger without `key` takes
the request's `X-Forgejo-Delivery`, `X-Gitea-Delivery` or `X-GitHub-Delivery`
header. Any other trigger without one never drops a firing as a duplicate.

## Terminal

```toml
[terminal]
focus = true             # Unset: focus only launches waiting for input
close_when_done = true
nested_zmx = false       # true runs agents and demos in zmx inside host places
default_host = "zmx"     # "zmx", "herdr", "rex", "tmux" or "zellij"
```

- `focus` decides whether the places `shoal start` opens take the terminal.
  Unset, a workspace shell or an agent waiting for its first prompt takes it,
  and an agent with input starts in the background. A
  [mode's](#modes-and-rules) `focus` overrides it for its launches.
- `close_when_done` closes the place Shoal opened for a workspace once `done`
  keeps the workspace and the agent finishes its turn.
- `nested_zmx` runs agents and demos in a zmx session inside the host's pane,
  block or tab. `shoal attach` then reaches them from any other terminal. It
  needs zmx 1.0.1 or later.
- `default_host` is where launches run from a terminal no host serves: in
  detached zmx sessions, or as tabs, windows or sessions of the local Herdr,
  Rex, tmux or Zellij server. Turning that host off runs them in place.

Each host's table below overrides `close_when_done` and `nested_zmx` for its
own places. See [terminals](../../../../../../docs/reference/terminals.md).

### Herdr

```toml
[terminal.herdr]
enabled = true             # false treats a Herdr pane as a plain terminal
new_tab = true             # Open starts in a new tab inside Herdr
workspace_name = "{repo}"  # The workspace for tabs Shoal opens outside Herdr
tab_name = "{repo}: {branch}"  # Optional
close_when_done = true     # Optional; over terminal.close_when_done
nested_zmx = true          # Optional; over terminal.nested_zmx
```

`tab_name` names the tabs Shoal opens. It takes a literal name or these
placeholders: `{repo}`, `{branch}`, `{issue_number}`, `{issue_title}`, and
`{name}` or `{title}`, the space's name. Issue fields are empty without an
issue. Unknown placeholders stay literal, and inserted values are never
expanded again. Without `tab_name`, a tab is labelled with the space's name,
else `<repo>#<number>` for an issue, else its branch.

`workspace_name` takes only `{repo}`. See
[Herdr](../../../../../../docs/reference/terminals.md#herdr).

### Rex

```toml
[terminal.rex]
enabled = true             # false treats a Rex terminal as a plain terminal
new_window = true          # Open windows beside the caller
placement = "window"       # "split" opens beside the caller's block instead
session_name = "{repo}"    # The session for windows Shoal opens outside one
window_name = "{repo}: {branch}"  # Optional; as tab_name
close_when_done = true     # Optional
nested_zmx = true          # Optional
```

`session_name` takes only `{repo}`. See
[Rex](../../../../../../docs/reference/terminals.md#rex).

### tmux

```toml
[terminal.tmux]
enabled = true             # Unset: only when default_host = "tmux"
new_window = true          # Open windows beside the caller
placement = "window"       # "split" opens beside the caller's pane instead
session_name = "{repo}"    # The session for windows Shoal opens outside tmux
window_name = "{repo}: {branch}"  # Optional; as tab_name
close_when_done = true     # Optional
nested_zmx = true          # Optional
```

`session_name` takes only `{repo}`. See
[tmux](../../../../../../docs/reference/terminals.md#tmux).

### Zellij

```toml
[terminal.zellij]
enabled = true             # Unset: only when default_host = "zellij"
new_tab = true             # Open tabs beside the caller
placement = "tab"          # "split" opens beside the caller's pane instead
tab_name = "{repo}: {branch}"  # Optional
close_when_done = true     # Optional
nested_zmx = true          # Optional
```

See [Zellij](../../../../../../docs/reference/terminals.md#zellij).

## Workspaces

### Setup

```toml
[setup]
command = "scripts/setup.sh"             # Prepares a new worktree; must exit 0
copy_ignored = true                      # Copy the checkout's ignored files first
copy_ignored_exclude = ["node_modules/"] # Gitignore-style patterns not to copy; default []
presets = ["cargo", "bun"]               # Build tools to set up; default []
```

- `command` is one executable path, relative to the worktree's root. Shoal
  runs it without shell parsing or `PATH` lookup. The workspace is ready only
  once it exits 0.
- `copy_ignored` copies the checkout's ignored files into a new worktree
  before `command` runs. A `.worktreeinclude` in the checkout limits what it
  copies.
- `copy_ignored_exclude` leaves out what its patterns match.
- `presets` names the build tools a repository uses: `cargo`, `npm`, `pnpm`,
  `yarn`, `bun`, `go`, `xcode`, `zig` and `ccache`. The global list applies
  to every repository that names none, and `presets = []` turns it off.
  Presets that keep the same seed output cannot be combined. A variable
  `[env]` sets wins over a preset's, and an [`[mcp]`](#mcp-servers) table
  over a preset's server.

See [setup and hooks](../../../../../../docs/reference/setup.md) and
[build tool presets](../../../../../../docs/reference/setup.md#build-tool-presets).

### Seed builds

```toml
[setup.seed]
command = "scripts/seed.sh"  # In the seed worktree; builds the default branch
paths = ["target"]           # Build outputs, relative to the worktree
```

`command` is an executable inside the seed worktree, given as a relative
path. It replaces the presets' commands, and `paths` replaces their outputs.
See [seed builds](../../../../../../docs/reference/setup.md#seed-builds).

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

Each value is one executable path, run without shell parsing or `PATH`
lookup. Give scripts a shebang, and put arguments and shell logic inside them.
A relative path resolves against the worktree's root, or for `post_remove`
against the repository checkout. A repository value replaces the global one.
See [setup and hooks](../../../../../../docs/reference/setup.md) and
[resource hooks](../../../../../../docs/reference/resources.md#resource-hooks).

### Environment

```toml
[env]
AGENT_BROWSER_SESSION = "{workspace}"
AGENT_BROWSER_PROFILE = "{workspace_path}/.agent-browser"
GOCACHE = "{cache.go}"
```

`[env]` adds variables to tracked executions, lifecycle hooks and exported
environments, of a chat as of a workspace. A chat without a repository takes
the global config's.

Values may use these placeholders, substituted once:

- `{workspace}`: the name of the workspace's directory;
- `{workspace_path}`;
- `{path}`: the directory the space works in;
- `{repo}` and `{branch}`;
- `{port.NAME}`: a reserved port;
- [`{cache.NAME}`](#shared-caches).

Unknown placeholders stay literal. Shoal never expands inserted values,
inherited variables or shell expressions. A chat leaves a variable unset when
its value uses a placeholder only a workspace has: `{workspace}`,
`{workspace_path}`, `{branch}` or a port. A chat without a repository also
leaves one unset that uses `{repo}` or a cache.

Names are ASCII identifiers, and `SHOAL_*` names are reserved. Values are
strings without NUL; an empty string sets an empty value. Reserved port
variables take precedence. See
[scoped workspace commands](../../../../../../docs/reference/modes.md#scoped-workspace-commands).

### Shared caches

```toml
[caches.go]
scope = "shared"  # Required; the only scope
```

Each `[caches.NAME]` is a directory that every workspace of the repository
shares, and `{cache.NAME}` names it in `[env]`. Names are lowercase, like port
names. Use a cache only for tools that lock their entries or address them by
content, never for build output. See
[shared caches](../../../../../../docs/reference/setup.md#shared-caches).

### MCP servers

```toml
[mcp.docs]
command = "docs-mcp"         # The server's program; required for a new server
args = ["--stdio"]           # Its arguments; default []
env = { DOCS_TOKEN = "..." } # Variables it runs with
enabled = true               # Whether agents start it; default true

[mcp.xcode]
enabled = false              # Keeps the xcode preset's caches without its server
```

Each `[mcp.NAME]` is a stdio MCP server that the agents of the repository's
workspaces and chats start, beside their own. A chat without a repository
reads the global config.

[Presets](#setup) bring servers of their own; `xcode` brings `xcode`, which
runs `xcrun mcpbridge`. A table with `command` defines its server whole and
replaces a preset's server of the same name. A table without `command` changes
the preset's `args`, `env` or `enabled`, and is refused when no preset brings
a server of that name.

Layers merge each server field by field, and `env` per variable. Names are
lowercase, like port names. See
[build tool presets](../../../../../../docs/reference/setup.md#build-tool-presets).

### Commands

```toml
[commands]
check = ["cargo", "test"]
edit = ["zed", "{path}"]
review = ["tuicr", "-r", "{diff_base}..HEAD"]
review-worktree = ["tuicr", "-w"]
```

Each entry is a shortcut that `shoal run <name>` runs. It is an argument
array whose executable resolves through `PATH`. It runs in the workspace,
without shell expansion. A higher layer replaces the whole array.

These placeholders expand once: `{workspace}`, `{branch}`, `{path}`, and
`{diff_base}`, the fork point `shoal diff` compares against. A standalone
`{args}` inserts the forwarded arguments there; otherwise they are appended.

`edit` is what `shoal edit` runs, and `review` the manual review of
`shoal review run`. An entry may share a name with an agent. See
[configured commands](../../../../../../docs/reference/run.md).

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

[demos.app]
preset = "simulator"
cmd = ["scripts/run-app.sh"]
viewer = ["serve-sim", "--port", "{port}", "{udid}"]  # Optional: the default; false turns it off
```

Names follow port names, except the names of `shoal demo` subcommands.

- `preset` says how the demo reaches you. `ports` and `open` replace the
  preset's parts.
- `viewer`, for a simulator demo, shows its device on another machine. It
  starts when someone opens the demo there. `{udid}` is the device's UDID and
  `{port}` the port the viewer serves on.

See [demos](../../../../../../docs/reference/demos.md).

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
take those names.

- `port` is the named port's preferred port.
- `env` names the variable that carries the port, in place of
  `SHOAL_PORT_<NAME>`.
- `on_conflict = "auto"` takes a free port instead of suggesting one.

See [port reservations](../../../../../../docs/reference/resources.md#port-reservations)
and [approvals](#approvals).

## Issues, PRs and merges

### Issues

```toml
[issue]
required_labels = ["type/"]  # Default: []
assign = "agent"             # "agent", "user", "both" or "none"
area_prefix = "area/"        # Default: unset
```

- `required_labels` lists prefixes. Every new issue needs a label that starts
  with each of them, so `area/` takes any area label. See
  [repository conventions](../../../../../../docs/reference/issues.md#repository-conventions).
- `assign` says whose accounts join the assignees of an issue a space links
  as assigned. `agent` adds the agent account while an agent works the issue,
  `user` adds your own login for every assigned link, `both` does both, and
  `none` adds no one.
  See [linked items](../../../../../../docs/reference/links.md).
- `area_prefix` is the prefix of the labels that name an issue's areas. The
  [agent queue](../../../../../../docs/reference/queue.md#queue-order) starts
  new workspaces whose issues share fewer areas with existing workspaces
  first. Unset, areas order nothing.

```toml
[priority.labels]   # Default: none
urgent = 75
"low-priority" = 25
```

`[priority.labels]` gives forge labels a priority from 0 to 100. A space or
launch given no priority takes the highest that its issues' labels map to.
See [queue order](../../../../../../docs/reference/queue.md#queue-order).

### PRs

```toml
[pr]
draft = false                       # Open new PRs as drafts
required_labels = ["area/", "type/"] # Default: []
undraft_on_ready = true             # `shoal ready` takes a draft PR out of draft
draft_on_unready = false            # `shoal ready --undo` returns it to draft
progress = false                    # Show the space's progress in its draft PR bodies
watch_merge_runs = false            # After a PR merges or `shoal merge` pushes, watch the default branch's CI runs for its commit
```

- `required_labels` works as for issues.
- `progress` shows each agent's progress on its draft PRs, until they leave
  draft.
- `watch_merge_runs` links to a space the default branch's CI runs for each
  merge of its work, and reports how they end. That covers a PR it watches or
  opened that merges into the default branch, and a local `shoal merge` that
  pushes its branch.

See [issue and PR actions](../../../../../../docs/reference/issues.md#issue-and-pr-actions),
[ready for review](../../../../../../docs/reference/completion.md#ready-for-review)
and [progress](../../../../../../docs/reference/progress.md).

### Reviews

```toml
[review]
agent = "codex"            # Default: agent.default
fallback_agent = "claude"  # Optional; reviews once more after a failure
post = true                # Post a PR review's findings on the PR
require_for_pr = "off"     # A new PR needs a finished, resolved review of HEAD
require_for_ready = "off"  # `shoal ready` needs one too
require_for_merge = "off"  # `shoal merge` needs one of what merges, a PR or a branch
```

Each `require_for_` key takes `"off"`; `"agents"`, which holds agents and the
commands they run; or `"all"`, which also holds your own. See
[local reviews](../../../../../../docs/reference/reviews.md#local-reviews).

### Merges

```toml
[merge]
enabled = true    # false turns local merges off; PRs still merge
confirm = true    # Ask before a local merge that pushes
push = false      # Push the default branch after a local merge
method = "squash" # "merge", "rebase" or "squash"; no default
```

- `enabled = false` turns local merges off. It leaves PR merges and
  `shoal absorb` alone.
- `method` sets how a local merge, a PR merge and `shoal absorb` merge, unless
  `-X` says otherwise. Without it, a PR merges by the forge's default, and the
  others fast-forward or create a merge commit.

See [merge into the default branch](../../../../../../docs/reference/branches.md#merge-into-the-default-branch).

### Conflicts

```toml
[conflicts]
sensitive = ["src/version.rs", "migrations/**"] # Default: []
```

`sensitive` lists the files that hold a number each change picks for itself,
such as a schema migration or a protocol version. It takes paths and globs
relative to the repository root. See
[conflicts](../../../../../../docs/reference/branches.md#conflicts).

### Runner

```toml
[runner]
enabled = false       # Run the repository's CI jobs on this machine
labels = ["shoal"]    # shoal-<os>-<arch> is always added
permit = "builds"     # Optional: a resource pool each job holds one permit from
```

See [runner](../../../../../../docs/reference/ci.md#runner).

## Cleanup

```toml
[cleanup]
grace_minutes = 10   # 0 removes at once

[cleanup.auto]
enabled = true       # Remove idle, clean, pushed or merged workspaces
idle_minutes = 10

[cleanup.merged]
enabled = false      # Remove merged workspaces once unused, done or not
idle_minutes = 240

[cleanup.pr]
enabled = true       # Remove completed workspaces once every watched PR has merged

[cleanup.done]
enabled = true       # Clean up after `shoal done`; false keeps the space for review

[cleanup.restore]
retention_days = 30  # How long restores, checkpoints and alerts stay
archive_max_mb = 50  # Unsaved files an agent's exit or stop checkpoint keeps
```

- `grace_minutes` is how long every unattended removal waits, except one for
  low disk space.
- `retention_days` is how long Shoal keeps a removed space restorable, with
  its log and files. It is also how long Shoal keeps checkpoints, copies that
  `shoal files` fetched from another machine, and your alerts, read or not.
  The repository of the space each is about sets it; an alert about no space,
  or about a chat without a repository, takes the global setting.

Global changes apply on reload, and repository ones on the next sweep. See
[automatic cleanup](../../../../../../docs/reference/removal.md#automatic-cleanup),
[assignment completion](../../../../../../docs/reference/completion.md#assignment-completion)
and [checkpoints and restore](../../../../../../docs/reference/removal.md#checkpoints-and-restore).

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

Global resources are shared across repositories, and a repository's across
its workspaces. A repository cannot redefine a global name.

- `capacity` is 1–65535 and defaults to 1. A pool's defaults to its members'
  total. Every pool needs members.
- `kind = "rwlock"` allows shared readers or one writer, and needs capacity 1.

Names are lowercase letters, digits, `_` or `-`, start with a letter, and have
at most 64 characters. See
[cooperative resource pools](../../../../../../docs/reference/resources.md#cooperative-resource-pools)
and [approvals](#approvals).

### Related repositories

```toml
[resources.server]
kind = "repo"
repo = "saldoir-server"        # Registered name or ID, or a remote URL
```

A resource of kind `repo` is a registered repository that a workspace may
read. Its capacity must be 1. See
[related repository paths](../../../../../../docs/reference/resources.md#related-repository-paths).

### Simulator preferences

```toml
[simulators]
preferred = ["phone"]          # Profiles to try, in order
requires_approval = false
approval_lifetime = "lease"
```

`preferred` orders the [simulator profiles](#simulators) that
`shoal acquire sim` tries, globally or per repository. Flags override it.

### Containers

```toml
[containers]
compose = "compose.yaml"       # Relative to the worktree root
ports = ["db", "cache"]        # Named ports reserved before the project starts
start = "acquire"              # Or "setup"
```

- `compose` gives each workspace its own Compose project from that file.
  Without it, the repository's workspaces have no containers.
- `ports` lists named ports to reserve, as `shoal acquire port` does, so their
  `[ports.<name>]` tables apply. The project reads each from its variable,
  such as `${SHOAL_PORT_DB}`.
- `start = "setup"` starts the project once setup readies the workspace,
  before its post-setup hook and agent. With `"acquire"`, it starts on
  `shoal acquire containers`.

See [containers](../../../../../../docs/reference/resources.md#containers).

### Approvals

`requires_approval = true` makes a scoped acquisition an approval request. It
applies to a resource, a named port, a simulator profile, `[simulators]` or a
secret with `tools`.

`approval_lifetime` says how long an approval lasts:

- `"lease"`, the default: until release.
- `"workspace"`: later acquisitions of the same member and settings need no
  new approval until the workspace is removed.

Repository defaults cannot relax a protected machine profile. See
[resource approvals](../../../../../../docs/reference/approvals.md#resource-approvals).

## Apps

```toml
[apps.quay]
board = "main"
[apps.quay.columns]
todo = "grey"
```

`[apps.<name>]` holds the settings of an app built on Shoal, such as Quay. Any
file takes it: a repository's tracked file or local config for that
repository, or the global config for every repository. Shoal checks only that
it is a TOML table. It layers each one as it layers its own options: a table
merges key by key with the one below it, and any other value replaces what is
below. The app reads the result for a space, a repository or the global
config alone. See [apps](../../../../../../docs/reference/apps.md).
