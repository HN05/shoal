# Shoal

Shoal runs coding agents side by side. Each task gets its own Git worktree and
branch, and Shoal shares ports, Xcode simulators and other resources between
them. You start agents, follow their work, review it and clean up when it is
done.

This guide covers installing Shoal, the first steps and the main workflows.
The [command reference](docs/reference/) says what each command does, and the
[configuration reference](crates/shoal-cli/src/cli/commands/configuration/reference.md)
describes each option. `shoal <command> --help` lists a command's flags.

## Install

```sh
brew install hn05/tap/shoal
shoal daemon install
shoal skill install
```

`shoal daemon install` starts the per-user daemon. It writes a config with the
defaults to `~/.config/shoal/` when there is none. `shoal skill install`
installs the `shoal` skill into each
[AI tool's existing skill directory](docs/reference/install.md#agent-skill);
`shoal skill install <tool>` creates the directory for one tool. Later `shoal`
commands keep installed skills current.

Homebrew also installs the tools for workspaces and interactive menus. It does
not install zmx 1.0.1 or later, the default terminal host. Install it with
`brew install neurosnap/tap/zmx`, or use
[another terminal host](docs/reference/terminals.md). When you upgrade zmx from
0.8, [stop the agents](docs/reference/agents.md#upgrade-zmx) running in its sessions
first; otherwise Shoal loses them.

For directory navigation and tab completion, add this line to your `.zshrc`
or `.bashrc`, and run it in your current shell:

```sh
source <(shoal shell init)
```

### Prebuilt binaries

Instead of Homebrew, you can download a macOS or Linux binary from a
[release](https://github.com/HN05/shoal/releases).

- `shoal-vX.Y.Z-<os>-<arch>` is the bare binary. Save it as `shoal` on your
  `PATH`, and make it executable with `chmod +x shoal`.
- On macOS, the archive ending in `-app.tar.gz` holds the same binary in
  `Shoal.app`. Put a link to `Shoal.app/Contents/MacOS/shoal` on your `PATH`.

Both are signed with Shoal's own certificate but not notarized. On macOS,
clear the quarantine with `xattr -dr com.apple.quarantine shoal`, or with
`Shoal.app`. macOS keeps the bare binary's privacy grants only while it stays
at the same path.

Install the [runtime dependencies](#runtime-dependencies) before you run
`shoal daemon install`.

### External volumes on macOS

When workspaces or `root_dir` are on an external volume, Shoal needs access to
it. Allow access to removable volumes when macOS asks, or grant Shoal Full Disk
Access or Removable Volumes under System Settings > Privacy & Security. Then
run `shoal daemon restart`. The grant survives upgrades. It covers the agents
Shoal starts and the commands they run.

### Runtime dependencies

Put these tools on `PATH` before you run `shoal daemon install`, which
captures `PATH` for the daemon. Prebuilt binaries need no Rust.

| Tool | Needed for |
| --- | --- |
| Git 2.43+ (`git`) | Repository and branch operations |
| [zmx](https://github.com/neurosnap/zmx) 1.0.1+, unless `terminal.default_host` names another host | Agent sessions that keep running after you detach |
| [fzf](https://github.com/junegunn/fzf) 0.63+ | Interactive menus and pickers |

Some features need more tools:

- Issues and PR watches need `curl`, and a
  [token](crates/shoal-cli/src/cli/commands/configuration/reference.md#forges)
  for private repositories.
- Agents need the AI tool you run, and Happy sessions need `happy` and `curl`.
- Simulators need macOS with Xcode, `xcrun simctl` and an installed simulator
  runtime.
- Containers need Docker or Podman with Compose.

The daemon runs as a launchd service on macOS, or a systemd user service on
Linux. Without either, [run the daemon in the foreground](docs/reference/daemon.md#run-in-the-foreground).

## First steps

Register a repository once, by its local path or a Git clone URL:

```sh
shoal repo add /path/to/project --name my-project
shoal repo setup my-project   # Have an agent write the repository's config
shoal config setup            # Have an agent write the global config for this machine
```

A repository's workspaces, and its clone for a URL, live under
`~/shoal/<name>/`. `shoal repo move <dir>` moves them all to a new root.

Start an agent on a new branch:

```sh
shoal start my-project --branch fix-login --agent claude
```

The agent works in its own worktree. Shoal opens it in a new tab or window of
your terminal host, or in a detached zmx session. Follow it from anywhere:

```sh
shoal                 # The inbox when something needs you, else your spaces
shoal list            # Workspaces, chats and queued agents
shoal attach 3        # Back to the session of space 3; Ctrl-\ detaches zmx
```

When the agent has finished, it runs `shoal done`. Shoal removes the
workspace once its work is merged or pushed, as [Clean up](#clean-up)
describes.

Every space has a number, which `shoal list` shows. Commands take that number,
or act on the space you are in. Outside a space, they open a picker on a
terminal. Commands that open, show or run something in a space take `-` for
the space they last acted on, as in `shoal attach -`.

## Start agents

```sh
shoal start my-project                     # Pick a branch, an issue or a PR
shoal start my-project --branch fix-api --base feature/api  # Stacked on feature/api
shoal start https://github.com/owner/repo/issues/34  # An issue, PR, CI run or branch link
shoal start --in 3 -a codex                # Workspace 3's own agent, on its issue
shoal start my-project --mode chat         # Discuss the repository in its checkout
shoal start --in 3 -m chat                 # A chat in workspace 3's worktree, beside its agent
shoal start --mode plan --issue            # Settle the plan of an issue that needs one
shoal start --mode orchestrate             # A master session that steers every repository
shoal contribute https://github.com/owner/repo/issues/68  # Fork when you cannot push
shoal create my-project --branch scratch   # A workspace to work in yourself, without an agent
```

New workspaces branch from the repository's default branch, unless `--base`
names another. See [workspaces](docs/reference/workspaces.md) for adopting an
existing worktree, copying a workspace and renaming.

### Agents

`--agent` picks the AI tool: `claude` for Claude Code, `codex`, `opencode`,
`pi` or `grok`, or `happy-claude` and `happy-codex` for a detached
[Happy](https://github.com/slopus/happy) session in the Happy app. Without
`--agent`, `shoal start` uses `agent.default`, or else asks on a terminal. See
[agents](docs/reference/agents.md#ai-tools) to add another tool.

Claude Code and Codex run with their own sandbox and approval settings. Shoal
turns on network access in Codex's sandbox, so that its commands reach the
daemon. Shoal's scope is cooperative, not a filesystem sandbox.

### Modes

A mode decides what an agent is told, and whether it gets a workspace or a
chat:

- `worker`, the default, implements its subject in a workspace.
- `chat` discusses the repository.
- `plan` settles an issue's plan with you.
- `coordinate` carries out an issue through agents for its sub-issues.
- `orchestrate` starts a master session: a chat without a repository that
  starts and steers work in every repository for you.

Configure your own modes under `[modes.<name>]` and `[rules.<name>]`. Add
instructions for every workspace agent in `~/.config/shoal/rules/worker.md`,
and for every chat in `rules/chat.md`. Agents read a repository's own rules
from its `AGENTS.md` or `CLAUDE.md`. The same rule files in a repository's
`.shoal/` directory add instructions that only Shoal's agents need. See
[modes and rules](docs/reference/modes.md).

### Terminal hosts

Inside a terminal host that your config enables, such as Herdr or
[Rex](https://www.superlogical.com/rex), `start` opens a new tab or window.
`--here` runs the agent in the current pane instead, and `--focus` switches
to the new place. From other terminals, launches go to `[terminal]
default_host`. With the default, zmx, an agent with a task runs in a detached
zmx session that `shoal attach` reaches. See
[terminal hosts](docs/reference/terminals.md).

Sessions keep running when you detach or close the terminal. Detach with
Ctrl-\ in zmx, or the host's own control elsewhere.

### The agent queue

Shoal starts agents and CI jobs one at a time while memory and disk leave
room. `machine.load` says how hard to push the machine: `"light"`,
`"balanced"` or `"full"`. `machine.max_heavy_agents` sets a fixed limit on
the agents of heavy modes, and `agent.max_heavy_running` a limit per
repository.

```sh
shoal queue                       # What waits, and what holds each entry
shoal queue start                 # Start a waiting entry now, regardless of the limits
shoal start --issue 43 --add-blocker 42   # Start on issue 43 once issue 42 is done
```

See the [agent queue](docs/reference/queue.md).

### Triggers

A trigger, `[triggers.<name>]`, starts an agent on its own, such as every
weekday at 09:00, or when a forge webhook says an issue was labelled.
`shoal trigger ls` lists them, and `shoal trigger run <name>` fires one now.
See [triggers](docs/reference/triggers.md).

## Follow and talk to agents

```sh
shoal inbox                   # Threads, approvals, agents needing input, failures, ready work
shoal status 3                # A space's activity, issue and PR state, changes and resources
shoal log 3                   # Its log; --follow keeps printing
shoal progress --space 3      # Its agent's status line and plan steps
shoal alert list              # Conflicts, finished agents and removals you missed
shoal send "Stop the dev server" --space 3      # Alert space 3's agent
shoal thread open "Which port?" --space 3       # Ask it a question; prints the thread's number
shoal send 3.2 "Use 8080"     # Post in thread 3.2
shoal thread resolve 3.2 answered
shoal files 3                 # The files its agent handed back
shoal demo                    # Open a demo an agent started for you to try
```

An agent runs `shoal ready` when it needs you: its work is ready for review,
or it has a question. The inbox lists it. An agent that asks for approval,
such as to merge or to use a protected resource, waits in the inbox too:

```sh
shoal approvals                     # Pending requests
shoal approvals --approve 3.2       # Space 3's request 2
shoal approvals --deny 3.2 --reason "Use the staging server"
```

See [alerts](docs/reference/alerts.md), [threads](docs/reference/threads.md)
and [approvals](docs/reference/approvals.md).

## Issues, PRs and CI

Agents open and follow their own issues and PRs. You can run the same commands
yourself, in a space or with a number:

```sh
shoal pr open                 # Push, then link the branch's open PR or open one
shoal view                    # Linked items with their state, comments, reviews and failed CI logs
shoal view pr 44 --check rust # One failed check's log
shoal ci rerun --check rust   # Rerun a failed check on the linked PR
shoal link 42 assigned        # Assign an issue or PR to this space
shoal issue ls --label bug flaky   # Search issues; pr ls too
shoal merge 42 -X rebase      # Merge a PR; --auto merges it once checks pass
```

Agents' issue and PR changes need an `agent_token` for the forge host. Use a
separate agent account's token, or `{ same_as = "user_token" }` to make them
with your login. A [Git profile](docs/reference/repo.md#git-profiles) sets the
author of the agent's commits:

```toml
[agent]
git_profile = "agent"

[machine.forges."git.example.com"]
user_token = { cmd = ["security", "find-generic-password", "-s", "forgejo", "-w"] }
agent_token = { cmd = ["op", "read", "op://dev/agent/token"] }  # Or { same_as = "user_token" }
```

See [agent accounts](docs/reference/agents.md#agent-accounts),
[issues and PRs](docs/reference/issues.md), [links](docs/reference/links.md)
and [CI](docs/reference/ci.md). Shoal can also run a repository's CI jobs on
this machine; see the [runner](docs/reference/ci.md#runner).

### Merge locally

To move a workspace's work into the default branch without a PR:

```sh
shoal repo sync               # Fetch and fast-forward the default branch
git rebase main               # Or git merge; use your repository's branch name
shoal merge                   # Merge the workspace's branch locally
shoal merge --push            # Then push the default branch, after asking
shoal unmerge                 # Undo the latest local merge while it is unpushed
```

See [branches](docs/reference/branches.md).

### Reviews

An agent can review a workspace's committed changes or a PR:

```sh
shoal review run 3            # A one-shot, read-only review; each finding is a thread
shoal review run <pr-url>     # Review a PR in the workspace that owns its branch
shoal view review             # The latest review in full
```

With a `[commands] review` command configured, `review run` asks whether to
run it or an agent; see the
[review command](docs/reference/run.md#review-commands). See
[local reviews](docs/reference/reviews.md#local-reviews) for the rest.

## Share resources

From a workspace:

```sh
shoal acquire port web --reason "Development server"
shoal acquire sim --queue          # macOS; needs configured simulator profiles
shoal acquire containers           # Needs a Compose file in [containers]
shoal acquire resource signing --queue
shoal leases                       # What you hold
shoal release port web
```

Use the port or simulator UDID that `acquire` returns. Reservations and leases
belong to the workspace and outlive the command that made them. Release them
when you are done. `shoal catalog` shows what each kind offers. See
[resources](docs/reference/resources.md).

## Configure a repository

Put a repository's settings in `.shoal.toml` or `.shoal/config.toml`, or for
settings only you want, in its [local config](docs/reference/config.md). For
example:

```toml
[setup]
command = "scripts/setup.sh"   # Prepares each new worktree
presets = ["cargo"]            # Sets up the build tools the repository uses

[ports.web]
port = 3000
env = "PORT"

[resources.signing]
capacity = 1
requires_approval = true

[env]
GOCACHE = "{cache.go}"

[caches.go]
scope = "shared"
```

- New workspaces get the checkout's ignored files, such as `.env`.
  `[setup] copy_ignored = false` turns that off.
- `[setup.seed]` keeps a build of the default branch that new workspaces
  start from, so they rebuild only what their branch changed.
- Hooks run around setup, completion, agent exits, removal and resource
  leases.
- `[secrets]` gives agents, or only some tools, values from your secret store.
- `git_profile = "work"` selects a named Git identity for new worktrees.

See [setup and hooks](docs/reference/setup.md),
[agent secrets](docs/reference/agents.md#agent-secrets) and the
[configuration reference](crates/shoal-cli/src/cli/commands/configuration/reference.md).

```sh
shoal config show 3           # Effective settings and where each comes from
shoal config set agent.default codex
shoal config set agent.default claude --repo my-project
shoal config install <name>   # Install a packaged global config
```

## Stop and resume

```sh
shoal stop 3                  # Stop the agents and commands of space 3 and below
shoal stop --interrupt 3      # Cancel its agent's turn; the agent keeps running
shoal resume 3                # Continue its stopped agent
shoal resume --all            # Queue the restore of every stopped agent
shoal restart --all --wait    # Restart each agent once its turn ends
```

An agent you stop stays down until `shoal resume`, and holds no queue slot.
Agents that a closed terminal, a logout or a reboot stopped come back through
the queue on their own.

When memory runs short, Shoal first warns running agents, then stops them and
keeps their workspaces and leases. When disk space runs low, it removes
workspaces that idle cleanup would remove, and then stops agents. Claude Code
and Codex agents come back once the machine recovers, and `shoal resume`
brings back the rest. See
[overload protection](docs/reference/load.md).

## Clean up

```sh
shoal done 3                  # Mark the work finished; Shoal cleans up once it is safe
shoal rm 3                    # Remove a space now, with the spaces below it
shoal cleanup                 # Pick leftover spaces, kept branches and caches to remove
shoal restore                 # Pick a removed space to open again
shoal restore 3 --checkpoint  # Return a workspace to one of its checkpoints
```

Automatic cleanup removes a workspace once it is idle, clean, and pushed or
merged. After `shoal done`, Shoal removes a workspace once its PRs have
merged. `shoal rm` asks
before it deletes uncommitted work. It deletes the branch too, unless the
branch has commits that are on no remote and not on the default branch.

Each removal keeps the workspace's HEAD and unsaved files in a checkpoint.
`shoal restore` opens a removed space again with its agents, for 30 days by
default. `shoal status` shows why Shoal keeps a space. See
[removal and cleanup](docs/reference/removal.md).

`shoal move 3 --ssh other` moves a space to another machine, where its agents
continue. See [moving spaces](docs/reference/move.md).

When something looks wrong, `shoal inbox` and `shoal alert list` show what
Shoal found and the command that fixes it. See
[recovery](docs/reference/daemon.md#recovery).

## Join your machines to a hub

One machine's daemon can be the hub that your other machines join. On that
machine:

```toml
# ~/.config/shoal/config.toml, then shoal config reload
[machine.hub]
enabled = true
url = "https://desk.example.ts.net"   # What a tunnel or proxy with TLS serves the listener at
```

```sh
shoal hub pair                       # On the hub: a code, with its QR code
shoal hub join '<code>'              # On the machine that joins
shoal hub join --connect "ssh desk shoal daemon connect"   # Or over SSH, without a code or proxy
shoal hub ls                         # Every machine and device, and whether it is connected
```

The listener stays on loopback until something with TLS is in front of it.
See [hub](docs/reference/hub.md).

## Update

```sh
brew update
brew upgrade hn05/tap/shoal
```

Update a downloaded binary or `Shoal.app` with `shoal update`. The daemon
applies an upgrade once it is idle; `shoal daemon restart` applies it at once.
Running agents and commands reattach to the new daemon. Reload
`source <(shoal shell init)` in open terminals. See
[installation](docs/reference/install.md).

## Development

[The design docs](docs/design/) explain decisions, the
[command reference](docs/reference/) behavior, and the
[configuration reference](crates/shoal-cli/src/cli/commands/configuration/reference.md)
every option. Create releases through **Actions → release**
([setup](docs/releases.md)).

Building needs Rust at least as new as `package.rust-version` in
[Cargo.toml](Cargo.toml), and the [runtime dependencies](#runtime-dependencies).
The tests run with [cargo-nextest](https://nexte.st). Integration tests also
use Bash, Zsh and Python 3.

Debug builds leave the user's state directory, config directory and
user-level files, such as installed skills, to the installed Shoal. Run them
with a temporary `--state-dir`. To change the config, also use a temporary
`HOME`, with `XDG_CONFIG_HOME` inside it if that is set. Tests in a temporary
`HOME` set `SHOAL_DEV_USER_FILES=1` to change user-level files.

```sh
cargo build
cargo nextest run
cargo test --doc
CARGO_BUILD_WARNINGS=deny cargo clippy --all-targets
cargo fmt --check
```
