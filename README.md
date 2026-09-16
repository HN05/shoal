# Shoal

Local workspaces and resource allocation for coding agents. See [design.md](design.md)
for the full design and implementation sequence.

## Development

Requires Rust, Git, and Worktrunk (`wt`, tested with 0.77.0). Interactive menus
require `fzf`. Install the runtime tools before running `shoal setup` so the
daemon captures a PATH that includes them. Integration tests also use Bash and Zsh.

```sh
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

## Daemon

```sh
cargo install --path .
shoal setup --dry-run       # Preview the OS service definition
shoal setup                 # Register and start the per-user service
shoal daemon status
shoal daemon stop
shoal daemon start
shoal daemon restart
```

macOS uses a launchd LaunchAgent in `~/Library/LaunchAgents`; it requires a GUI
login session. Linux uses a systemd user service. Minimal containers without a
user service manager can run the daemon in the foreground instead:

```sh
shoal --state-dir /tmp/shoal-dev daemon run
# In another terminal:
shoal --state-dir /tmp/shoal-dev --json daemon status
shoal --state-dir /tmp/shoal-dev daemon stop
```

State defaults to `~/.local/state/shoal`. `--state-dir` or `SHOAL_STATE_DIR` selects
an isolated instance for development. Service commands target the one registered
per-user service; its configured state directory must match. Runtime state and
the Unix socket are private to the user. Status returns exit code 1 when offline.

`setup` preserves the invoked executable's symlink path where possible. Use
`--executable /absolute/path/to/shoal` to explicitly select a stable installation
path. A running daemon must be restarted after upgrading its binary. macOS
daemon diagnostics go to `daemon.log` in the state directory; Linux uses the
user journal (`journalctl --user -u shoal.service`).

Service definition serialization and CLI-to-service control are tested with an
isolated service-manager fixture. Tests do not install persistent user services.
Native Linux service integration has not yet been exercised on a Linux host.

## Workspaces

```sh
shoal repo add /path/to/repo # Or a Git clone URL; register once
shoal repo list
shoal add /path/to/repo --name fix-login
shoal exec fix-login -- cargo test
shoal claude fix-login -- --help
shoal codex fix-login -- --help
shoal inspect fix-login
shoal stop fix-login        # Stop commands; keep the worktree
shoal rm fix-login          # Stop commands and remove a clean worktree
```

Bare `shoal` opens an `fzf` action menu. `shoal add` offers registered repositories
in most-recently-used order and prompts for a name. Commands with omitted workspace
targets open an `fzf` picker; `exec`, `claude`, and `codex` first look for a workspace
containing the current directory. Explicit targets bypass selection. Noninteractive
calls and `--json` never prompt; management commands support JSON output, while
executed commands retain their own stdin, stdout, stderr, and exit code.

Workspaces live at `<state-dir>/workspaces/<name>`. Names are unique, 1–64 ASCII
letters/digits/hyphens/underscores, starting with a letter or digit. Creation uses
the registered checkout's committed `HEAD`, or `--ref <git-ref>`, on a new branch
named `shoal/<name>-<unique-suffix>`. Uncommitted source files are not copied.
URL repositories are cloned once into `<state-dir>/repositories` and retained
for reuse. Registration does not fetch updates automatically.

Removal refuses tracked changes and untracked files, removes ignored files
inside the worktree, and retains Git branches. Shared caches outside the worktree
remain untouched. Worktrunk hooks are disabled; repository setup scripts and
Shoal configuration parsing are not implemented yet. Restore dependencies with
an explicit command such as `shoal exec fix-login -- npm ci`.

### Shell navigation

`shoal setup` prints a Bash/Zsh function to add to your shell configuration. You
can also load it in the current shell, or add this line to `.bashrc`/`.zshrc`:

```sh
source <(shoal shell init)
```

With the function loaded, successful `add` enters the workspace. Removing the
workspace containing your current directory moves you to its registered repository
root (or home if that root is gone). JSON calls do not change directories. Shoal
does not edit your shell configuration automatically.

## Current scope

The CLI/daemon foundation and initial workspace lifecycle are implemented, with
SQLite persistence and connected command supervision. Ports, simulators, lifecycle
polish, and filesystem restrictions follow. No Shoal filesystem sandbox is applied
yet. The public repository configuration format and schema remain undecided.

Normal command shutdown includes its process group. Detached processes and recovery
after abrupt wrapper/daemon termination still need the later lifecycle work.
Executions whose completion cannot be confirmed are recorded as unknown and block
workspace removal pending manual reconciliation; no reconciliation command exists
yet. Avoid daemon restarts during active commands at this stage.
