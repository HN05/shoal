# Shoal

Shoal gives each coding task its own Git worktree and coordinates shared ports,
Xcode simulators, and other resources. Run agents or commands in a workspace,
switch between tasks, and clean up when finished.
## Install

```sh
brew install hn05/tap/shoal
shoal install
shoal skill install
```

Use `brew install --HEAD hn05/tap/shoal` to track `main` instead of releases, or
download a prebuilt macOS or Linux binary from a [release](https://github.com/HN05/shoal/releases);
the macOS binaries are unsigned, so clear the download quarantine first with
`xattr -d com.apple.quarantine shoal`.
Homebrew installs the additional tools for workspace management and interactive
menus. For a downloaded binary, install
the [runtime dependencies](#runtime-dependencies) before running `shoal install`.
`shoal install` starts the per-user
daemon and writes missing config and prompt template defaults in `~/.config/shoal/`;
`shoal skill install` installs the worker and orchestrator skills into each
[AI tool's existing skill directory](docs/reference.md#agent-skill-outside-project-repositories);
`shoal skill install <tool>` creates one. Later `shoal` commands keep installed
skills current after upgrades.

For directory navigation and tab completion, add this to your `.zshrc` or
`.bashrc`, then run it in your current shell:

```sh
source <(shoal shell init)
```

### Runtime dependencies

Put these tools on `PATH` before running `shoal install`, which captures it for
the daemon. Prebuilt binaries do not require Rust.

| Tool | Needed for |
| --- | --- |
| Git 2.43+ (`git`) | Repository and branch operations |
| [Worktrunk](https://worktrunk.dev/) (`wt`, tested with 0.77.0) | Creating and removing worktrees |
| `lsof` | Checking whether a workspace is in use before cleanup |
| `ps` (included with macOS; typically `procps` or `procps-ng` on Linux) | Tracking and stopping processes |
| [fzf](https://github.com/junegunn/fzf) | Interactive menus and pickers |

Feature-specific tools are installed separately: an authenticated `gh` (GitHub)
or `fj` (Forgejo) for issues and PR watches; the agent or command you want to run;
and `happy` plus `curl` for Happy sessions. Simulator use requires macOS with
Xcode, `xcrun simctl`, and an installed simulator runtime.
Service installation uses launchd on macOS or a systemd user session on Linux;
without one, [run the daemon in the foreground](docs/reference.md#daemon).

## Create a workspace

Register a local repository or Git clone URL once, then create a task. Each
repository's workspaces (and its clone, for URLs) live under `~/shoal/<name>/`:

```sh
shoal repo add /path/to/project --name my-project
shoal add my-project fix-login
shoal codex fix-login --cli
```

Use `shoal add my-project` to pick a new or existing branch, an open issue or an open PR, or pass it explicitly:

```sh
shoal add my-project fix-api --base feature/api  # Branch from feature/api
shoal add my-project fix-api --base origin/feature/api  # Fetch the pushed branch first
shoal base set feature-api --workspace fix-api  # Record the workspace fix-api builds on
shoal add my-project --existing origin/feature/api --agent codex
shoal add my-project quick-fix --path ../quick-fix
shoal adopt ../existing-worktree             # Take ownership, including automatic cleanup
shoal adopt --copy ../existing-worktree      # Copy work into Shoal's default directory
shoal rename fix-login fix/login             # Rename its branch and workspace together
shoal add https://github.com/owner/repo/issues/68 # Create from an issue
shoal add https://github.com/owner/repo/issues/34  # Add an issue, PR, or branch link
shoal swarm add 34 --agents codex,claude      # Attempt issue 34 with both agents
shoal swarm pick issue-34-fix-codex           # Keep one attempt, remove the others
shoal link https://github.com/owner/repo/issues/34  # Link an issue to the current workspace
shoal unlink issue                         # Remove the linked issue
shoal watch pr 505                         # Watch an explicit PR
```

Inside Herdr, `add` opens a new tab after your choices; use `--here`
to run in the current pane. See [Herdr settings](docs/reference.md#agents).

Use `--agent claude` for Claude Code, `opencode`, `pi` or `grok` for those
tools, or `--agent happy-claude`/`happy-codex` for
a detached [Happy](https://github.com/slopus/happy) session that appears in the
Happy app. Interactive `shoal add` uses `--agent` or picks an installed agent.
Issue additions also use `default_agent = "codex"` from the repository or global
config. Edit `~/.config/shoal/issue-template.md`
to customize issue prompts and `agent-template.md` for general instructions;
put those files in a repository root to append project-specific guidance.
New workspaces branch from the repository's default branch unless `--base REF`
is supplied to `add`. With shell integration, `shoal add` enters the
workspace.

## Everyday commands

After [adding an AI tool](docs/reference.md#agents), run
`shoal add my-project fix-api --agent droid` or `shoal add <issue-url>`.

```sh
shoal                         # Interactive workspace menu
shoal repo                    # Interactive repository menu
shoal ls                      # List workspaces
shoal ls --ready              # PRs, issues and workspaces marked ready for review
shoal status fix-login        # Workspace activity, changes, resources, issue and PR state
shoal status pr 12            # Workspaces that link PR 12; also issue or resource
shoal config show fix-login   # Effective settings and the source of each value
shoal config set default_agent codex
shoal config set cleanup.auto.enabled true cleanup.auto.idle_minutes 30  # Saved together
shoal config set default_agent claude --repo my-project
shoal config unset default_agent  # Restore the default
shoal cd fix-login             # Enter a workspace; omit the name for a picker
shoal exec fix-login -- cargo test
shoal run check fix-login      # With [commands] check = ["cargo", "test"] in config
shoal run                     # List configured commands, arguments, and source layers
shoal claude fix-login         # Run Claude Code
shoal codex fix-login --app    # Open in the Codex desktop app
shoal happy claude fix-login   # Detached Happy session, visible in the Happy app
shoal diff fix-login           # Changes since the branch's fork point
shoal conflicts --workspace fix-login  # Whether the branch merges cleanly into main
shoal inspect fix-login       # Detailed workspace and execution records
shoal notifications           # Conflicts, finished agents, and removals you missed
shoal notify "PR #12 is ready to merge"  # Notify the user from a workspace
shoal message "Stop the dev server" --workspace fix-login  # Message the workspace's agents
shoal stop fix-login          # Stop agents and commands; save them for shoal resume
shoal stop --all              # Stop every workspace, for example before a reboot
shoal resume --all            # Restore stopped agents; report stopped commands
shoal done fix-login          # Mark finished and request safe cleanup
shoal done --keep fix-login   # Mark finished; keep for review
shoal done --cleanup fix-login # Override a configured keep default
shoal undone fix-login        # Withdraw a recorded done
shoal rm fix-login            # Remove the workspace
shoal link pr 42              # Link a PR (or paste its URL)
shoal link pr 43              # Link another; all must merge before cleanup after done
shoal watch                   # Wake on all linked item comments, checks, or closure
shoal view                    # Linked items with state, checks, comments, and reviews
shoal view pr 44 --no-comments  # One PR, linked or not, without its discussion
shoal ready                   # Mark linked items ready for review at HEAD
shoal unlink pr 43            # Cancel one linked PR
shoal unlink pr               # Cancel linked PRs
```

For workspace actions, omit the name to use your scoped or current workspace;
outside one, choose from the interactive picker or pass a target for `--json`
and noninteractive calls. Bare `shoal cd` always opens a picker.
Use `shoal <command> --help` for options.

To move changes between your workspace and the default branch:

```sh
shoal sync                    # Fetch and fast-forward the default branch
git rebase main               # Or git merge; use your repo's branch name
shoal land                    # Merge your branch into the default branch; no remote needed
shoal land --push             # Then push the default branch instead of opening a PR
```

For local code review, configure `[commands] review = ["tuicr", "-r",
"{diff_base}..HEAD"]` and run `shoal review fix-login` or `shoal review <pr-url>`,
then choose manual or agent review; `shoal review <issue-url>` has an agent refine
an issue before implementation. See the
[command and review configuration](docs/reference.md#configured-commands) for uncommitted
review and exporting feedback to an agent.

CLI agents run in the terminal through Shoal; Happy sessions run detached, log
to a file Shoal names, and stop with `shoal stop`. The Codex shortcut disables
Codex's sandbox and approval prompts; use `shoal exec fix-login -- codex` for a
custom invocation. Shoal's resource scope is cooperative, not a filesystem sandbox.
Apps that launch their own processes can request `shoal env fix-login --json`
and add the returned variables to each child's environment. Revoke its scope
with `shoal env fix-login --revoke <token>`. See
[workspace scope](docs/reference.md#scoped-workspace-commands) for token lifetime
and cleanup behavior.

For a separate agent account, configure forge executable wrappers and a
[Git profile](docs/reference.md#git-profiles) for the agent's commits:

```toml
[agent_auth]
fj = "~/bin/fj-agent"
gh = "~/bin/gh-agent"
git_profile = "agent"
```

See [agent accounts](docs/reference.md#agent-accounts)
for wrapper setup.

Memory overload protection stops tracked agents by default while retaining their
workspaces and leases; before that, Shoal messages running agents to reduce load. To opt out, run `shoal config set overload.memory.enabled
false`. See [overload protection](docs/reference.md#overload-protection)
for thresholds and timing. When disk space runs low, the daemon removes
workspaces idle cleanup would remove; if space stays critical, it stops tracked
executions. Agents restore once space returns; `shoal resume` restores the rest. Stop agents manually with `shoal stop`, then restore
a stopped session with `shoal resume`. Codex and Claude automatically continue
the workspace session after load recovers; `[agent_resume]` overrides their restore
command or enables recovery for other agents.

## Share resources

From a managed workspace:

```sh
shoal acquire port web --reason "Development server"
shoal leases                  # Every kind, with capacity
shoal release port web

shoal acquire sim --wait 60   # macOS; requires configured simulator profiles
shoal release sim

shoal acquire resource signing --wait 60
shoal release resource signing
```

Use the returned port or simulator UDID. Reservations and leases belong to the
workspace and survive command exit; release them when finished.

Install a packaged global config with `shoal config install <name>`;
`shoal config install --help` lists the available names.

Put project defaults in `.shoal.toml` or `.shoal/config.toml`. For example:

```toml
[ports.web]
port = 3000
env = "PORT"

[resources.signing]
capacity = 1
requires_approval = true
approval_lifetime = "lease"    # Or "workspace"

[resources.server]
kind = "repo"
repo = "saldoir-server"       # Registered repository name or remote URL
```

Acquire a related repository with `shoal acquire repo server`. Use its
checkout path in a project override, for example:

```sh
server=$(shoal --json acquire repo server | jq -r '.repository.path')
SALDOIR_SERVER_REPO="$server" Scripts/sync-contract.sh
shoal release repo server
```

Agents request protected resources with `acquire --reason "purpose"`. Review
requests with `shoal access`, then use `shoal access approve <id>` or
`shoal access deny <id>` from an unscoped terminal.

Select a named Git identity with `git_profile = "work"` in that file, or use
`shoal add my-project feature --git-profile work`. Define the profile
in global config; see [Git profiles](docs/reference.md#git-profiles).

The same file can name a `setup_cmd` and hooks around setup, assignment completion,
agent exits, removal, and resource acquisition/release, for example to open a tmux session or prepare an
external device. See [setup and hooks](docs/reference.md#workspace-setup-and-hooks)
and [resource hooks](docs/reference.md#resource-hooks) for configuration and failure behavior.

## Cleanup

`shoal rm` removes a workspace and its resources, prompting when needed about
its branch and uncommitted work. `shoal stop` keeps the workspace.

Automatic cleanup removes idle, clean, pushed or landed workspaces after 10 minutes,
and forgets workspaces whose directory you deleted yourself, keeping the branch.
Remove them now with `shoal cleanup`; `shoal cleanup --dry-run` lists them.
An app hosting an external session can hold its workspace while the session is open:

```sh
shoal hold acquire fix-login --name quay-thread-42 --reason "Quay thread"
shoal hold fix-login
shoal hold release fix-login --name quay-thread-42
```

Configure idle cleanup in a repository's `.shoal.toml`, or for every repository in
`~/.config/shoal/config.toml` followed by `shoal daemon reload`:

```toml
[cleanup.auto]
enabled = false
```

When something looks wrong, run `shoal doctor --all` for diagnostics or
`shoal doctor <name>` for a workspace; see
[cleanup](docs/reference.md#automatic-cleanup) and [recovery](docs/reference.md#recovery) for details.

## Update

```sh
brew update
brew upgrade hn05/tap/shoal
```

Managed daemons apply the upgrade once their clients and operations are idle. For
a main-channel install, use `brew upgrade --fetch-HEAD hn05/tap/shoal`; run
`shoal daemon restart` to apply an upgrade immediately; running agents and
commands reattach to the new daemon. Wrappers from releases before reattachment
are stopped instead; restore them with `shoal resume --all`. The next `shoal`
command updates installed skills; reload `source <(shoal shell init)` in open
terminals.

## Development

See [design.md](design.md) for decisions and the [command reference](docs/reference.md) for behavior.
Create releases through **Actions → release** ([setup](docs/releases.md)).

Requires Rust at least as new as `package.rust-version` in [Cargo.toml](Cargo.toml)
and the [runtime dependencies](#runtime-dependencies).
Integration tests also use Bash, Zsh, and Python 3.

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
