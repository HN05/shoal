# Shoal

Local workspaces and resource allocation for coding agents. See [design.md](design.md)
for the full design and implementation sequence.

## Development

Requires Rust, Git, `lsof`, and Worktrunk (`wt`, tested with 0.77.0). Interactive menus
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
shoal repo add /path/to/repo --name my-project
shoal repo rename my-project new-name
shoal repo list
shoal add /path/to/repo --name fix-login
shoal cd fix-login          # Enter through the shell function
shoal exec fix-login -- cargo test
shoal claude fix-login -- --help
shoal codex fix-login -- --help
shoal inspect fix-login
shoal stop fix-login        # Stop commands; keep the worktree
shoal rm fix-login          # Delete redundant branch, otherwise choose what to keep
shoal rm fix-login --yes --keep-branch
shoal rm fix-login --yes --delete-branch
```

Bare `shoal` opens an interactive workspace list. `shoal --help` shows help;
without a terminal, bare `shoal` also shows help. The list offers:

| Key | Action |
| --- | --- |
| Enter | Enter the selected workspace |
| Ctrl-D | Delete, with confirmation when needed |
| Ctrl-E | Run Claude, Codex, or a custom shell command |
| Ctrl-A | Add a workspace |
| Ctrl-O | Inspect |
| Ctrl-S | Stop managed commands |
| Ctrl-F | Show workspace diff |

Each selection performs its action and returns to your shell. Entering a workspace
requires the shell integration below; without it, Shoal prints the path.

`shoal add` offers registered repositories
in most-recently-used order and prompts for a name. Repository lists and pickers
show readable repository names, including for managed clones. Pickers show just
the name, adding `(hostname)` for duplicate names (and the source if still
ambiguous). `repo list` includes each original path or URL.
Commands with omitted workspace
targets open an `fzf` picker; `rm`, `exec`, `claude`, and `codex` first look for a workspace
containing the current directory. Explicit targets bypass selection. Noninteractive
calls and `--json` never prompt; management commands support JSON output, while
executed commands retain their own stdin, stdout, stderr, and exit code.

Workspaces live at `<state-dir>/workspaces/<name>`. Names are unique, 1–64 ASCII
letters/digits/hyphens/underscores, starting with a letter or digit. Creation uses
the registered checkout's committed `HEAD`, or `--ref <git-ref>`, on a new branch
named `shoal/<name>-<unique-suffix>`. Uncommitted source files are not copied.
URL repositories are cloned once into `<state-dir>/repositories` and retained
for reuse. Registration does not fetch updates automatically.
Registration reuses an existing repository when its source URL or local checkout's
`origin` identifies the same remote, including HTTPS/SSH forms and optional `.git`
suffixes. Checkouts without `origin` are identified by their canonical local path.
Custom names can be set with `repo add --name` or `repo rename`, appear in pickers,
and work as selectors (`shoal add my-project --name fix-login`). Explicit names
are unique; naming an already registered URL updates its name without duplicating it.

### Diff

```sh
shoal diff fix-login
shoal diff                 # Current workspace, otherwise fzf
```

Shows changes from the fork point against the base branch recorded at creation.
Moving that base branch or rebasing onto it does not include its commits in your
diff. Git's fork-point detection uses the base branch's reflog, with merge-base
as a fallback. For workspaces created from a fixed commit, the captured commit
is used; older workspaces without base metadata use `main`.

Runs native `git diff`, preserving Git's configured pager and external diff
command. Includes committed, staged, and unstaged tracked changes; untracked
files follow normal Git behavior and do not appear. Missing/unrelated base refs
produce an error rather than guessing another branch.

### Port reservations

```sh
shoal port reserve web fix-login --reason "Frontend dev server"
shoal port reserve api fix-login --port 3001 --env API_PORT --reason "HTTP API"
shoal port list fix-login
shoal port list --all
shoal port release web fix-login
shoal exec fix-login -- sh -c 'my-server --port "$API_PORT"'
```

Each named TCP reservation belongs to a worktree. Omitting the workspace uses
the current workspace or opens `fzf`. Repeating a name returns the same port;
`--reason` can update its description. Changing the number or environment mapping
requires release first. Reservations persist across command exits, `stop`, and
daemon restarts. Successful manual or automatic worktree removal releases them;
failed removal keeps them reserved.

Subsequent `exec`, `claude`, and `codex` commands receive `SHOAL_PORT_<NAME>` by
default (`web` becomes `SHOAL_PORT_WEB`), or the variable supplied with `--env`.
An already-running process keeps its original environment. All management
commands support `--json`; `inspect` also includes reservations.
Nested Shoal executions clear the parent workspace's exported port variables
before applying the target workspace's reservations.

Automatic allocation defaults to TCP ports 49152–65535. Override the range in
global config with `[ports]`, `start = 49152`, and `end = 65535`. An explicit
`--port` may select any nonzero port the user can bind. Shoal checks IPv4/IPv6
availability and prevents duplicate allocations within the daemon, then releases
the probe socket so the application can bind. Reservations are cooperative:
unrelated processes can still take a port later. UDP allocation is not implemented.

Removal deletes the branch when the worktree is clean and its contents match
local `main` or its configured upstream. The comparison is between Git trees;
commit history can differ. Missing refs do not count as a match.

Otherwise, `fzf` offers **Abort**, **Delete worktree but keep branch**, and
**Delete worktree and branch**, with Abort selected by default. Both deletion
choices discard uncommitted/untracked files, so preserving the branch only saves
committed work. Noninteractive callers choose `--yes --keep-branch` or
`--yes --delete-branch`. `--yes` alone does not choose for differing/dirty work.

Running processes do not block manual removal. Connected Shoal commands are
stopped; external or disconnected processes are left alone. Automatic cleanup
still requires no active or unknown commands and no processes using the directory.
Ignored worktree files are removed; shared caches remain untouched. Git protects
branches checked out in another worktree; output reports the actual branch result.
Worktrunk hooks are disabled; repository setup scripts and repository config
parsing are not implemented yet. Restore dependencies with
an explicit command such as `shoal exec fix-login -- npm ci`.

### Automatic cleanup

Enabled by default: an idle, clean, fully pushed worktree is removed after
10 minutes. File changes (including ignored files), Git HEAD changes, and Shoal
command activity reset the timer. Running commands, processes with a working
directory in the worktree (including open shells), dirty files, unpushed commits,
and failed checks prevent cleanup. Shoal checks about every 30 seconds and starts
fresh timers after daemon restarts.

“Pushed” means commits are reachable from locally known remote branches; Shoal
does not fetch automatically. Automatic removal rechecks conditions and uses the
same cleanup path as manual removal. Matching branches are deleted; other branches
and shared caches are retained. Future
simulator leases will belong to the worktree and use this path too. Port
reservations are already released here.

Configure `~/.config/shoal/config.toml` (or `$XDG_CONFIG_HOME/shoal/config.toml`):

```toml
[auto_cleanup]
enabled = false # Default: true
idle_minutes = 10
```

Restart the daemon after changing this configuration. `lsof` must be available
for process checks; failed inspection retains the workspace.

### Shell navigation

`shoal setup` prints this line to add to `.bashrc`/`.zshrc`. You can also run it
in the current shell to load the Bash/Zsh function:

```sh
source <(shoal shell init)
```

With the function loaded, successful `add` enters the workspace. Removing the
workspace containing your current directory moves you to its registered repository
root (or home if that root is gone). JSON calls do not change directories. Shoal
does not edit your shell configuration automatically.

## Current scope

The CLI/daemon foundation, workspace lifecycle, and named TCP port reservations
are implemented, with SQLite persistence and connected command supervision. Simulators, lifecycle
polish, and filesystem restrictions follow. No Shoal filesystem sandbox is applied
yet. The public repository configuration format and schema remain undecided.

Normal command shutdown includes its process group. Detached processes and recovery
after abrupt wrapper/daemon termination still need the later lifecycle work.
Executions whose completion cannot be confirmed are recorded as unknown and block
automatic cleanup. Manual removal can proceed, but cannot stop disconnected processes. Avoid daemon restarts during active commands at this stage.

### Repo port defaults

Use `.shoal.toml` or `.shoal/config.toml` (not both):

```toml
[ports]
on_conflict = "suggest" # or "auto"

[ports.web]
port = 3000
env = "PORT"
reason = "Frontend dev server"
```

`shoal port reserve web` allocates on request. CLI flags override repo defaults.
A conflict suggests an available number; fzf offers acceptance, while `--json`
returns `reserved: false` and exits 2. Accept with `--port <suggested_port>`.
`--on-conflict auto` accepts reassignment directly. `shoal ports` shows the current
worktree's configured and reserved ports, including the actual numbers.

### Scoped workspace commands

Commands launched through `exec`, `claude`, and `codex` can inspect their own
worktree, execute there, and manage its resources. They cannot access other
worktrees, remove workspaces, alter repos, or administer the daemon. Nested
commands keep that scope. Run a cross-workspace orchestrator outside `shoal exec`.
Scope is cooperative; it does not restrict direct filesystem/Git operations.
