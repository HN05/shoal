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
`shoal skill install` installs instructions for Codex, Claude Code, and
[configured AI tools](docs/reference.md#agent-skill-outside-project-repositories).

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

Use `shoal add my-project` to pick a new or existing branch, or pass it explicitly:

```sh
shoal add my-project fix-api --base feature/api  # Branch from feature/api
shoal add my-project --existing origin/feature/api --agent codex
shoal add my-project quick-fix --path ../quick-fix
shoal adopt my-project ../existing-worktree  # Take ownership, including automatic cleanup
shoal add my-project --issue 68           # Create from an issue; add --agent codex to start an agent
shoal add --issue https://github.com/owner/repo/issues/34  # URL finds the repository
shoal issue https://github.com/owner/repo/issues/34  # Paste an issue: finds the repository, starts the default agent
shoal issue 34                             # Use the current repository, or pick one
shoal issue 34 --repo my-project          # Select the repository explicitly
```

Use `--agent claude` for Claude Code, or `--agent happy-claude`/`happy-codex` for
a detached [Happy](https://github.com/slopus/happy) session that appears in the
Happy app. `shoal issue` takes the same `--agent`, or `default_agent = "codex"`
from the repository or global config. Edit `~/.config/shoal/issue-template.md`
to customize issue prompts and `agent-template.md` for general instructions;
put those files in a repository root to override them for that project.
New workspaces branch from the repository's default branch unless `--base REF`
is supplied to `add` or `issue`. With shell integration, `shoal add` enters the
workspace.

## Everyday commands

After [configuring a custom agent](docs/reference.md#agents), run
`shoal add my-project fix-api --agent pi` or `shoal issue <url>`.

```sh
shoal                         # Interactive workspace menu
shoal list                    # List workspaces
shoal status fix-login        # Workspace activity, changes, and resources
shoal config show fix-login   # Effective settings and the source of each value
shoal config set default_agent codex
shoal config set auto_cleanup.idle_minutes 30
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
shoal inspect fix-login       # Detailed workspace and execution records
shoal notifications           # Conflicts, finished agents, and removals you missed
shoal stop fix-login          # Stop managed commands; keep the workspace
shoal rm fix-login            # Remove the workspace
shoal pr 42                   # Remove when merged (also accepts a PR URL)
shoal pr merged               # Manually confirm merge and remove
```

Inside a workspace, omit its name. Use `--json` or `shoal <command> --help`.

To move changes between your workspace and the default branch:

```sh
shoal merge main              # Merge it into your branch; use your repo's branch name
shoal merge feature/api       # Merge another local or remote branch
shoal land                    # Merge your branch into the default branch; no remote needed
```

For local code review, configure `[commands] review = ["tuicr", "-r",
"{diff_base}..HEAD"]` and run `shoal review fix-login` or `shoal pr review <url>`,
then choose manual or agent review. See the
[command and review configuration](docs/reference.md#configured-commands) for uncommitted
review and exporting feedback to an agent.

CLI agents run in the terminal through Shoal; Happy sessions run detached, log
to a file Shoal names, and stop with `shoal stop`. The Codex shortcut disables
Codex's sandbox and approval prompts; use `shoal exec fix-login -- codex` for a
custom invocation. Shoal's resource scope is cooperative, not a filesystem sandbox.
For separate agent forge logins, configure executable wrappers:

```toml
[agent_auth]
fj = "~/bin/fj-agent"
gh = "~/bin/gh-agent"
```

See [agent forge authentication](docs/reference.md#agent-forge-authentication)
for wrapper setup.

## Share resources

From a managed workspace:

```sh
shoal port acquire web --reason "Development server"
shoal port
shoal port release web

shoal sim acquire --wait 60   # macOS; requires configured simulator profiles
shoal sim release

shoal resource acquire signing --wait 60
shoal resource release signing
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
```

Agents request protected resources with `acquire --reason "purpose"`. Review
requests with `shoal access`, then use `shoal access approve <id>` or
`shoal access deny <id>` from an unscoped terminal.

Select a named Git identity with `git_profile = "work"` in that file, or use
`shoal add my-project feature --git-profile work`. Define the profile
in global config; see [Git profiles](docs/reference.md#git-profiles).

The same file can name a `setup_cmd` and hooks around setup, removal, and
resource acquisition/release, for example to open a tmux session or prepare an
external device. See [setup and hooks](docs/reference.md#workspace-setup-and-hooks)
and [resource hooks](docs/reference.md#resource-hooks) for configuration and failure behavior.

## Cleanup

`shoal rm` removes a workspace and its resources, prompting when needed about
its branch and uncommitted work. `shoal stop` keeps the workspace.

Automatic cleanup removes idle, clean, pushed or landed workspaces after 10 minutes,
and forgets workspaces whose directory you deleted yourself, keeping the branch.
Disable it when using desktop agents whose activity Shoal cannot track. Set this
in a repository's `.shoal.toml`, or for every repository in
`~/.config/shoal/config.toml` followed by a daemon restart:

```toml
[auto_cleanup]
enabled = false
```

When something looks wrong, run `shoal doctor --all` for diagnostics or
`shoal doctor <name>` for a workspace; see
[cleanup](docs/reference.md#automatic-cleanup) and [recovery](docs/reference.md#recovery) for details.

## Update

```sh
brew update
brew upgrade hn05/tap/shoal
shoal daemon restart
```

For a main-channel install, use `brew upgrade --fetch-HEAD hn05/tap/shoal`. Skill
links update automatically; reload `source <(shoal shell init)` in open terminals.

## Development

See [design.md](design.md) for decisions and the [command reference](docs/reference.md) for behavior.
Create releases through **Actions → release** ([setup](docs/releases.md)).

Requires Rust and the [runtime dependencies](#runtime-dependencies).
Integration tests also use Bash, Zsh, and Python 3.

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
