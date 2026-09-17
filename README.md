# Shoal

Shoal gives each coding task its own Git worktree and coordinates shared ports,
Xcode simulators, and other resources. Run agents or commands in a workspace,
switch between tasks, and clean up when finished.

## Install

```sh
brew install hn05/tap/shoal
shoal setup
shoal skill install
```

Use `brew install --HEAD hn05/tap/shoal` to track `main` instead of releases.
Homebrew installs the runtime dependencies. `shoal setup` starts the per-user
daemon; `shoal skill install` installs instructions for Codex and Claude Code.

For directory navigation and tab completion, add this to your `.zshrc` or
`.bashrc`, then run it in your current shell:

```sh
source <(shoal shell init)
```

## Create a workspace

Register a local repository or Git clone URL once, then create a task:

```sh
shoal repo add /path/to/project --name my-project
shoal add my-project --name fix-login
shoal codex cli fix-login
```

Or create the workspace and start an agent in one command:

```sh
shoal add my-project --name fix-api --agent codex -- "Fix the API timeout"
```

Use `--agent claude` for Claude Code. New workspaces branch from the repository's
default branch. With shell integration, `shoal add` enters the workspace.

## Everyday commands

```sh
shoal                         # Interactive workspace menu
shoal list                    # List workspaces
shoal cd fix-login             # Enter a workspace; omit the name for a picker
shoal exec fix-login -- cargo test
shoal claude fix-login         # Run Claude Code
shoal codex app fix-login      # Open in the Codex desktop app
shoal diff fix-login           # Changes since the branch's fork point
shoal inspect fix-login       # Workspace details
shoal stop fix-login          # Stop managed commands; keep the workspace
shoal rm fix-login            # Remove the workspace
```

Inside a workspace, most commands can omit its name. Use `--json` for structured
output and `shoal <command> --help` for options.

To bring changes into your current workspace:

```sh
shoal pull                    # Refresh the repository's default branch
shoal merge main              # Merge it into your branch; use your repo's branch name
shoal merge feature/api       # Merge another local or remote branch
```

CLI agents run in the terminal through Shoal. The Codex shortcut disables Codex's
sandbox and approval prompts; use `shoal exec fix-login -- codex` for a custom
invocation. Shoal's resource scope is cooperative, not a filesystem sandbox.

## Share resources

From a managed workspace:

```sh
shoal port reserve web --reason "Development server"
shoal ports
shoal port release web

shoal sim acquire --wait 60   # macOS; requires configured simulator profiles
shoal sim release

shoal resource acquire signing --wait 60
shoal resource release signing
```

Use the returned port or simulator UDID. Reservations and leases belong to the
workspace and survive command exit; release them when finished.

Put project defaults in `.shoal.toml` or `.shoal/config.toml`. For example:

```toml
[ports.web]
port = 3000
env = "PORT"

[resources.signing]
capacity = 1
```

See the [command reference](docs/reference.md) for simulator profiles, shared
resource pools, workspace setup scripts, and configuration outside Git.

## Cleanup

`shoal rm` removes a workspace and its resources, prompting when needed about
its branch and uncommitted work. `shoal stop` keeps the workspace.

Automatic cleanup removes idle, clean, fully pushed workspaces after 10 minutes.
Disable it when using desktop agents whose activity Shoal cannot track. Set this
in `~/.config/shoal/config.toml`, then restart the daemon:

```toml
[auto_cleanup]
enabled = false
```

For a workspace that needs recovery, start with `shoal reconcile <name>`.
See [cleanup](docs/reference.md#automatic-cleanup) and
[recovery](docs/reference.md#recovery) for details.

## Update

```sh
brew update
brew upgrade hn05/tap/shoal
shoal daemon restart
```

For a main-channel install, use `brew upgrade --fetch-HEAD hn05/tap/shoal`.
Homebrew skill links update automatically. Reload `source <(shoal shell init)`
in existing terminals after upgrading.

## Development

See [design.md](design.md) for product decisions and the
[command reference](docs/reference.md) for detailed behavior.
Create releases through **Actions → release**; see [release setup](docs/releases.md).

Requires Rust, Git, `lsof`, and Worktrunk (`wt`, tested with 0.77.0). Interactive menus
require `fzf`. Install the runtime tools before running `shoal setup` so the
daemon captures a PATH that includes them. Integration tests also use Bash, Zsh, and Python 3.

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```
