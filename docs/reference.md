# Shoal command reference

Detailed command behavior and configuration. Start with the [usage guide](../README.md).

- [Installation and upgrades](#homebrew)
- [Daemon](#daemon)
- [Workspaces](#workspaces)
- [Ports](#port-reservations)
- [Setup commands](#workspace-setup)
- [Cleanup](#automatic-cleanup)
- [Shell integration](#shell-navigation)
- [Recovery](#recovery)
- [Simulators](#simulators-macos)
- [Resource pools](#cooperative-resource-pools)
- [Agent skill](#agent-skill-outside-project-repositories)

## Homebrew

The shared [HN05 tap](https://github.com/HN05/homebrew-tap) offers two channels: tagged releases
(the default) and the current `main` branch (`--HEAD`). Both build from source.
Tap the repository once:

```sh
brew tap hn05/tap
```

Install a release:

```sh
brew install hn05/tap/shoal
```

Or track `main`:

```sh
brew install --HEAD hn05/tap/shoal
```

After installing either channel:

```sh
shoal skill install
shoal setup
```

The formula installs Worktrunk, Git, fzf, and lsof as runtime dependencies, and
uses Rust to build. Shoal manages its own per-user service through `shoal setup`.
To update releases:

```sh
brew update
brew upgrade hn05/tap/shoal
shoal daemon restart
```

To update `main`:

```sh
brew update
brew upgrade --fetch-HEAD hn05/tap/shoal
shoal daemon restart
```

The channels share one installation, daemon, and skill path. To switch, stop the
daemon with `shoal daemon stop`, run `brew uninstall hn05/tap/shoal`, then use
the desired install command above and run `shoal daemon start`. Shoal's state and
user skill links remain outside the Homebrew package and are preserved.

Installed skill links follow Homebrew's stable `opt` path automatically. Existing
copied skills need one `shoal skill install` using the Homebrew binary to migrate.
If another Shoal installation is earlier on PATH, use
`"$(brew --prefix shoal)/bin/shoal"` explicitly or adjust PATH.

Project-specific build and packaging logic lives in `scripts/install-homebrew.sh`
in this repository. The shared tap only declares source versions, dependencies,
and the invocation of that script. The selected release or main commit supplies
its own build script and skill.

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

`shoal setup` preserves the invoked executable's symlink path where possible.
A running daemon must be restarted after upgrading its binary. macOS
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
shoal add /path/to/repo --name fix-api --agent codex
shoal cd                    # Fuzzy workspace picker, even inside a workspace
shoal cd fix-login          # Enter through the shell function
shoal cd -                  # Previous directory; refuses deleted destinations
shoal exec fix-login -- cargo test
shoal claude fix-login -- --help
shoal codex                 # Current workspace or picker; defaults to cli
shoal codex cli fix-login -- --help
shoal codex app fix-login    # Open the Codex desktop app
shoal t3 fix-login           # Open in the running T3 Code desktop app
shoal inspect fix-login
shoal stop fix-login        # Stop commands; keep the worktree
shoal rm fix-login          # Delete redundant branch, otherwise choose what to keep
shoal rm fix-login --yes --keep-branch
shoal rm fix-login --yes --delete-branch
```

`add --agent codex|claude` starts the agent in the new workspace after worktree
creation and configured setup succeed. Codex uses `codex.default_mode` below. Pass a prompt or other
agent arguments after `--`, for example:

```sh
shoal add my-project --name fix-api --agent codex -- "Fix the API timeout"
```

CLI agents run in the current terminal through Shoal's tracked execution wrapper.
The command returns the agent's exit code and retains the workspace, including
when launch fails. With shell integration, your shell enters the new workspace
after the agent exits. Creation failures never launch an agent. Setup failures
must be explicitly ignored before launching. `--json` emits the workspace record
first, followed by the agent's
unmodified output.

`shoal claude` appends `--remote-control <workspace-name>`, using the resolved
workspace's name. `shoal codex cli` appends `--sandbox danger-full-access
--ask-for-approval=never`. Arguments after `--` are forwarded before these flags.
Use `shoal exec ... -- claude/codex ...` for a custom invocation.

`shoal codex` uses the default mode, initially `cli`. Set it in
`~/.config/shoal/config.toml` (or `$XDG_CONFIG_HOME/shoal/config.toml`):

```toml
[codex]
default_mode = "cli" # Or "app"
```

This setting takes effect on the next launch without restarting the daemon.
Explicit `shoal codex cli` and `shoal codex app` override it. To name a workspace,
include the mode: `shoal codex cli fix-login`. Arguments after `--` work with
either the default or an explicit mode.

App launches run
`codex app <workspace-path>` or `t3 app <workspace-path>`, with optional arguments
after `--` passed through unchanged. They add no agent flags and preserve the
launcher's output and exit code. Install the corresponding CLI on your PATH;
T3's desktop app must already be running. An omitted workspace uses the current
workspace, otherwise fzf. App handoff does not track GUI sessions or supply their
agents with Shoal execution scope or port variables. Disable automatic cleanup
when using an app whose workspace activity Shoal cannot track reliably.


Bare `shoal` opens an interactive workspace list. `shoal --help` shows help;
without a terminal, bare `shoal` also shows help. The list offers:

| Key | Action |
| --- | --- |
| Enter | Enter the selected workspace |
| Ctrl-D | Delete, with confirmation when needed |
| Ctrl-E | Run Claude/Codex CLI, open Codex/T3 apps, or run a shell command |
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
targets open an `fzf` picker; `rm`, `exec`, `claude`, `codex`, and `t3` first look for a workspace
containing the current directory. Explicit targets bypass selection. Noninteractive
calls and `--json` never prompt; management commands support JSON output, while
executed commands retain their own stdin, stdout, stderr, and exit code.

`shoal add --name` and the interactive branch prompt accept literal Git branch
names, validated by Git. The branch keeps the requested spelling, including
slashes, punctuation, and Unicode. Workspaces live at
`<state-dir>/workspaces/<name>` with a portable name derived from that input:
replace characters other than ASCII letters/digits/hyphens/underscores with `-`,
remove leading hyphens/underscores, and truncate to 64 characters. An empty result
becomes `workspace`. For example:

```sh
shoal add example-ios --name feature/123-update-profile
# Branch:    feature/123-update-profile
# Workspace: feature-123-update-profile
shoal cd feature-123-update-profile
```

Workspace names are unique across repositories. Inputs that produce an occupied
name fail without changing the existing workspace; choose a different name.
Commands and completions use the resulting workspace name (or ID).

Creation uses the repository's default branch, or `--ref <git-ref>`, on a new branch. If its name is
taken, append `-2`, `-3`, etc. Local branches, known remote branches, branch
namespaces, and retained Shoal records count as conflicts. If an ancestor blocks
the branch, suffix that component: an existing `feature` makes `feature/topic`
become `feature-2/topic`. Reserved names `HEAD` (Git), `@` (Worktrunk), and full
40/64-character hex object-ID spellings also get a suffix. Branch conflict suffixes do not change the derived workspace name or
directory. Existing branches and workspaces are not renamed.
Uncommitted source files are not copied.
Shoal reads `origin/HEAD` to find the default branch (for example, a repository may use
`develop`). Without `origin`, it uses the sole remote; multiple remotes without
`origin` are ambiguous. If the remote HEAD is not cached, creation and `pull`
query it and cache the symbolic ref. To pick up a remote's renamed default branch,
run `git remote set-head origin --auto` in the registered checkout.

Before branching, Shoal fetches the selected local branch's configured upstream
and fast-forwards it, even if the registered checkout is on another branch. An
already-ahead branch is preserved. Missing local branches or upstreams, a failed
fetch, divergence, or a dirty/managed default-branch checkout stop creation. With
no remotes, the registered checkout's current branch is the default and needs no
upstream. Detached local checkouts need an explicit `--ref`.

An explicit `--ref` selects another starting point without refreshing the default
branch. Naming the detected default branch directly or as `refs/heads/<branch>`
still refreshes it. Explicit refs use only locally known default-branch metadata;
they remain usable without contacting a remote to discover its default.

Register an existing local checkout directly; no remote is required:

```sh
shoal repo add ~/Projects/local-project --name local-project
shoal add local-project --name feature
```

The checkout stays in place. With no remote, creation uses its current committed
branch; use `--ref <branch>` to select another starting point.

The names shown by `shoal repo list` work as repository targets, including names
inferred from URLs for older clones. Explicit `--name` values take precedence.
If multiple repositories share an inferred name, use their ID, path, or source
URL, or assign a unique name with `shoal repo rename`.

URL repositories are cloned once into `~/.local/share/shoal/repositories/<name>`
and retained for reuse, separately from daemon state. The directory uses `--name`
when supplied, otherwise the URL's repository name without `.git`. If occupied,
Shoal tries `<name>-2`, `<name>-3`, etc. Files, directories, symlinks, and recorded
repository paths all reserve names. URL names are sanitized for directory use.
Set the top-level
`repositories_dir` in `~/.config/shoal/config.toml` (or
`$XDG_CONFIG_HOME/shoal/config.toml`) to choose another location:

```toml
repositories_dir = "~/Projects/shoal-repositories"
```

Choose an exact clone directory for just one repository with `--path`:

```sh
shoal repo add https://example.com/team/project.git --path ~/Projects/project
```

`--path` overrides the global directory. Relative paths resolve from your current
directory; `~/` is also supported. The destination must not exist yet. Register
an existing checkout by passing it as the source instead. Re-registering a URL
with its existing path is allowed; a different path is rejected rather than
moving or duplicating the repository.

For `repositories_dir`, use an absolute path or `~/` for your home directory,
and restart the daemon after changing it. This setting applies to new URL clones.
Existing registrations keep their recorded paths, including older clones under
`<state-dir>/repositories`; local repositories stay in place. Workspaces remain
under `<state-dir>/workspaces`.
Registration does not fetch updates automatically.
Registration reuses an existing repository when its source URL or local checkout's
`origin` identifies the same remote, including HTTPS/SSH forms and optional `.git`
suffixes. Checkouts without `origin` are identified by their canonical local path.
Custom names can be set with `repo add --name` or `repo rename`, appear in pickers,
and work as selectors (`shoal add my-project --name fix-login`). Explicit names
are unique; naming an already registered URL updates its name without duplicating it.

### Store repository config outside Git

```sh
shoal repo config my-project --file ~/project-shoal.toml
shoal repo config my-project          # Print the saved TOML
shoal repo config my-project --clear  # Return to worktree config
```

Use the same TOML format as `.shoal.toml`, including ports, resources, resource
pools, and simulator preferences. Shoal validates and copies the file into its
state database; it does not modify the repository or keep a reference to the
input file. Reimport the file to apply later edits. `--json` returns
`repository_id` and `toml` (`null` when no local config is saved).

The saved config replaces the entire worktree config for every existing and new
workspace of this repository. Omitted settings use their normal defaults; the
files are not merged. An empty file explicitly selects all defaults. Without a
saved config, Shoal reads `.shoal.toml` or `.shoal/config.toml` from each worktree
and rejects both together. Changes apply on the next resource request without a
daemon restart; existing reservations and leases remain in place. Global machine
policy and CLI overrides still apply.

Local config survives daemon restarts, repo renames, and individual workspace
removal. Successful `repo rm` deletes it with the repository registration; failed
removal retains it for retry. Scoped workspace commands cannot administer this
config.

### Delete a repository

```sh
shoal repo rm my-project --yes
```

Permanently deletes the repository checkout, all its Shoal workspaces and branches,
and their ports, simulators, and resource leases, plus the locally saved repo
config. Managed commands are stopped.
This also deletes local repositories registered in place, including uncommitted
and unpushed work. Interactive calls ask `Are you sure? [y/N]`; Enter or `n`
cancels. Pass `--yes` to skip the prompt (required for scripts and `--json`).
Names, IDs, paths, and source URLs work as selectors. `repo remove` is an alias.

Existing linked worktrees outside Shoal must be removed separately first. Stale
records for deleted directories do not block removal when Git marks them prunable.
Locked worktrees remain protected, including on disconnected disks. Shoal refuses
redirected paths and deletion that would include another registered repository or
its state directory. If cleanup fails, completed steps stay completed and remaining
records are retained. Retry the same command to finish; new workspace creation is
blocked while repository deletion is incomplete. Simulator audit history is kept.

### Pull the default branch

```sh
shoal pull                 # Current workspace (or fzf); agents resolve to their own
shoal pull fix-login
shoal --json pull          # Branch name, previous/current commits, and whether it changed
```

Fast-forwards the repository's default branch, selected as for `add`, from its
configured upstream. Requires that local branch and its tracking configuration;
the tracking remote may differ from the default remote. It does not merge or
rebase the feature branch, stash edits, or force-update history. A checked-out
default branch must be clean (including untracked files). Divergence is an error;
an already-ahead branch stays unchanged. If the default branch is checked out in another
Shoal-managed workspace, the request is refused. Otherwise Shoal updates its
checkout, or just its ref when not checked out. Git hooks and recursive submodule
updates are disabled. Scoped agents may request this only through their own
workspace; repository/service administration remains unavailable to them.

### Merge into your workspace branch

```sh
shoal merge main
shoal merge feature/api                    # Local, or discover a remote-only branch
shoal merge feature/api --remote origin    # Fetch explicitly, even if local exists
shoal merge origin/feature/api fix-login   # Qualified source and named destination
shoal --json merge feature/api
```

The destination must be the workspace's recorded branch. Scoped agents can only
target their own workspace. Local branches take precedence; otherwise Shoal
queries configured remotes and fetches the branch even if it has never been
fetched before. Multiple matches require `--remote`; an inaccessible remote
prevents automatic discovery, so select a reachable remote explicitly. Qualified
remote sources always fetch fresh data and reject deleted branches. Full
`refs/heads/<branch>` and `refs/remotes/<remote>/<branch>` names are also accepted.

Git can fast-forward or create a merge commit. Conflicts remain in your worktree
for normal `git add`/`git commit` resolution or `git merge --abort`. There is no
automatic stash, reset, push, or update of another branch. Hooks and recursive
submodule updates are disabled. JSON includes `success`, `exit_code`, commit IDs,
and Git's stdout/stderr; conflicts preserve Git's nonzero exit status. Fetch or
validation errors use the usual CLI error output.

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

Subsequent `exec`, `claude`, and `codex cli` commands receive `SHOAL_PORT_<NAME>` by
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
local default branch or its configured upstream. The comparison is between Git trees;
commit history can differ. Missing refs do not count as a match.

Otherwise, `fzf` offers these choices, with Cancel selected by default:

| Choice | Result |
| --- | --- |
| Cancel | Keep everything |
| Keep branch | Delete workspace files only |
| Delete branch | Delete workspace files and branch |

The next prompt summarizes the workspace, files, and branch action and asks
`Are you sure? [y/N]`. Both removal choices discard uncommitted/untracked files;
keeping the branch saves committed work only. `--keep-branch` or `--delete-branch`
skips the picker but still asks for confirmation. Noninteractive callers use
`--yes --keep-branch` or
`--yes --delete-branch`. `--yes` alone does not choose for differing/dirty work.

Running processes do not block manual removal. Connected Shoal commands are
stopped, along with identity-verified survivors of disconnected executions.
External or unverified processes are left alone. Automatic cleanup
still requires no active or unknown commands and no processes using the directory.
Ignored worktree files are removed; shared caches remain untouched. Git protects
branches checked out in another worktree; output reports the actual branch result.
Worktrunk hooks are disabled. Use `setup_cmd` for workspace setup (see below).

### Workspace setup

Set a top-level `setup_cmd` in `.shoal.toml`, `.shoal/config.toml`, or the local
repository config imported with `shoal repo config --file`:

```toml
setup_cmd = "scripts/setup.sh"
# Or use an absolute path on this machine:
# setup_cmd = "/opt/dev-tools/setup-project"
```

Relative paths resolve from the new worktree's root, including local config paths;
absolute paths are used as-is. The executable runs with the worktree as its working
directory, using your CLI environment and Shoal workspace scope. Make scripts
executable and include a shebang such as `#!/bin/sh`. The value is a single path;
put arguments, multiple commands, and shell expressions inside the script.

`shoal add` waits for setup before entering the worktree or launching `--agent`.
If setup fails, interactive mode asks whether to delete the new workspace and its
branch, ignore the failure and continue, or keep it for inspection (the default).
Deleting requires confirmation and preserves the registered repository and other
workspaces. Ignoring allows the requested agent to start once ownership and process
checks pass.

JSON/noninteractive mode returns nonzero and keeps the failed workspace. Setup
output goes to stderr in JSON mode; no agent starts on failure. After inspecting
partial changes, choose an explicit action:

```sh
shoal prepare fix-login                    # Retry setup, or rerun it later
shoal reconcile fix-login --repair          # Ignore failure after ownership checks
shoal rm fix-login --yes --delete-branch    # Delete only this workspace and branch
```

Setup is never inferred from package manifests. Without `setup_cmd`, creation
behaves as before. Retry scripts should tolerate partial previous runs.

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
and shared caches are retained. Active simulator leases and resource permits
prevent automatic removal. Successful removal releases port reservations and
resource permits and deletes the worktree's managed simulators.

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
root (or home if that root is gone). `shoal cd` always opens fzf, including from
inside a workspace; `shoal cd <name>` goes directly to that workspace. The picker
omits workspaces whose directories are missing. Canceling leaves you in place.

`shoal cd -` uses your shell's previous directory, including ordinary directories
outside Shoal. If that directory was deleted (for example by `shoal rm`), it
reports an error and leaves you where you are. History is local to each shell,
using `OLDPWD`, with no daemon-wide history. Scoped agents cannot navigate outside
their own worktree. Without shell integration the command prints the destination;
JSON calls return its path and never change directories. `shoal cd` without a target requires an
interactive terminal; noninteractive callers must give a name or `-`.

Reload `source <(shoal shell init)` in existing terminals after upgrading to pass
previous-directory state reliably. Shoal does not edit your shell configuration
automatically.

### Tab completion

Reload the shell integration in existing terminals to enable completion in Bash
or Zsh:

```sh
source <(shoal shell init)
```

Zsh's completion system is initialized if needed. Each Tab uses the installed
binary's current command definitions, covering subcommands, flags, fixed values
(including `codex cli`/`app`), and filesystem paths without a running daemon.
Targets and subcommands appear before flags. Zsh registration preserves this
order in fzf-tab as well; reload the shell integration to apply it.
With the daemon running, completion also suggests registered repositories and
workspaces across commands. It suggests resource pools, members, lease names,
ports, and simulator lease names for the current or explicitly selected workspace.
Live lookup respects `--state-dir`, `SHOAL_STATE_DIR`, and execution scope, times
out after 500 ms, and never starts the daemon or opens a picker.
For Bash versions that cannot source process substitutions, use
`eval "$(shoal shell init)"` instead.

To install completions separately, `shoal completions zsh` or
`shoal completions bash` prints the script. Fish, PowerShell, and Elvish are also
supported by `shoal completions <shell>`; `--json` returns a `script` field.
You can still omit targets to use Shoal's interactive fzf selection.

## Current scope

The CLI/daemon, workspaces, named TCP ports, simulator sharing, cooperative
resource pools, workspace setup, and explicit recovery are implemented. Filesystem
restrictions and prebuilt Homebrew bottles remain future work.
No Shoal filesystem sandbox is applied yet. Repository resource configuration
uses TOML; additional configuration sections will be defined as they are added.

### Recovery

```sh
shoal reconcile fix-login                  # Inspect without changing state
shoal --json reconcile --all               # Reports for every workspace
shoal reconcile fix-login --repair         # Repair verified state, retain work/resources
shoal reconcile fix-login --repair --stop  # Also stop verified surviving commands
```

Without a name, use the current workspace or `fzf`. Reports return exit code 2
while issues remain, or 0 when resolved; JSON is always an array of reports.
Reconciliation is an unscoped management operation, unavailable inside `shoal exec`.

Startup marks interrupted lifecycle operations as failed and disconnected
executions as unknown. It checks worktree ownership without deleting files or
releasing resources. Repair can restore a verified worktree to ready and clear
stopped execution records. Connected commands are left running unless `--stop`
is supplied. Moved worktrees must be restored to their recorded path; Shoal
refuses replaced worktree metadata. For a deleted directory, repair leaves the
workspace failed, then explicit `shoal rm` finishes resource cleanup and prunes
its stale Worktrunk registration while retaining the Git branch.

Executions record wrapper/child PID and start time plus a process-group ID.
Descendants inherit `SHOAL_EXECUTION_ID`; Shoal checks visible markers and live
ancestry to find detached survivors. It rechecks process identities before
signaling and does not kill unverified process-group candidates. Normal wrapper
completion cleans up its original process group; a visible detached survivor
keeps the execution unknown and prevents automatic cleanup.

Process inspection is cooperative, not complete containment. Hidden environments
(including some macOS system programs), cleared markers, and older records can
leave recovery uncertain. After independently checking that those processes have
stopped, use `--repair --acknowledge-stopped`. Known live wrappers, descendants,
unverified group candidates, and visible directory users still block clearing.
Detached children that hide their marker and outlive their original process group
may escape detection after normal command completion. Unknown records are never
silently discarded at startup. Native Linux process recovery still needs testing
on a Linux host.

### Repo port defaults

Use `.shoal.toml` or `.shoal/config.toml` (not both), or import an external file
with `shoal repo config <repository> --file <file>`:

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

Commands launched through `exec`, `claude`, and `codex cli` can inspect their own
worktree, execute there, merge any local or remote branch into their own branch
with `shoal merge`, manage resources, and pull the repository default branch with `shoal pull`.
They cannot access other worktrees, remove workspaces, perform
other repository administration, or administer the daemon. Nested
commands keep that scope. Run a cross-workspace orchestrator outside `shoal exec`.
Scope is cooperative; it does not restrict direct filesystem/Git operations.

## Simulators (macOS)

Shoal creates, boots, shares, and cleans up its own Xcode simulators. Configure
allowed profiles in `~/.config/shoal/config.toml`, then restart the daemon:

```toml
[simulators]
max_booted = 2
max_devices = 4
idle_seconds = 120
allow_any = false
default = "phone"

[simulators.profiles.phone]
device = "iPhone 17"
runtime = "iOS 26.5"
```

Use `shoal sim catalog` to find names or identifiers installed on your machine.
Repo config can set `[simulators] preferred = ["phone", "tablet"]`; explicit
flags override it. No runtimes are downloaded. With global `allow_any = true`,
other installed combinations can be requested with `--device`, `--runtime`, and
`--reason`.

```sh
shoal sim acquire                   # Current worktree, configured preference
shoal sim acquire --profile phone --name tests --wait 60
shoal sim list                      # Current worktree's devices
shoal sim list --all                 # All managed devices (human callers)
shoal sim release tests
shoal sim release                   # Release the default lease
```

Acquisition returns a ready device UDID; repeated requests for the same lease
name return the same device. Use that UDID explicitly with `simctl` or
`xcodebuild -destination 'platform=iOS Simulator,id=<UDID>'`. Leases belong to
worktrees and survive command exit and daemon restart. Scope restrictions apply.

At capacity, Shoal shuts down idle managed devices first; active leases and
personal simulators are never interrupted. External running simulators count
against the limit. Busy requests exit 2, or retry with `--wait <seconds>`.
Released instances keep apps and settings across worktrees during the reuse
window, and are deleted when the idle timer expires (checked every 15 seconds). Worktree removal deletes its devices; active leases block automatic
removal. Failed operations retain records for `sim release` or removal to retry.

The scheduler covers the default CoreSimulator device set and one Shoal daemon.
Indirect Xcode test/preview clones and cross-daemon scheduling remain future work.

### Explicit clean devices and audit history

```sh
shoal sim acquire --clean --reason "Verify first-launch permission prompts"
shoal sim history                  # Current worktree
shoal sim history --all            # Review requests across worktrees
shoal --json sim history --all --limit 50
shoal sim history --all --before 123 # Older entries
```

Normal handoff does not erase or reboot the device. A clean request returns a
fresh or fully erased device, requires a nonempty reason, and cannot erase an
active lease: release it first or request another lease name.

Shoal creates a fresh device when there is room, preserving installed apps on
existing devices. At the pool limit it minimizes the estimated number of user
apps lost, either erasing an idle compatible device or replacing an idle device
with fewer apps. Counts are captured while booted and cached on release; unknown
counts rank after known counts. Zero apps alone does not prove settings are clean.

History records workspace name/ID, execution ID (or unscoped caller), timestamps,
reason, outcome, selected device, planned resets/evictions, estimated app loss,
and whether an erase completed. Busy/failed requests are included; retries from
one `--wait` call share one entry with an attempt count. History lives in Shoal's
SQLite database, survives workspace/device deletion and daemon restarts, and is
paginated with `--limit`/`--before`. Scoped commands see only their own history.
Reasons enable review; Shoal does not automatically judge whether they justify
cleaning. A caller deliberately erasing a device outside Shoal bypasses this
cooperative audit, as it bypasses resource reservations.


## Cooperative resource pools

Global config defines resources shared across repos; repo config (`.shoal.toml`
or `.shoal/config.toml`) defines resources shared across that repo's branches.
Both accept the same format:

```toml
# Standalone mutex. Raise capacity for a counting semaphore.
[resources.signing]
capacity = 1
reason = "Signing service"

[resource_pools.devices]
capacity = 2                  # Total simultaneous permits across this pool

[resource_pools.devices.resources.alpha]
capacity = 1                  # Exclusive use of alpha

[resource_pools.devices.resources.beta]
capacity = 2                  # Up to two users of beta, within the pool limit
```

Member/standalone capacity defaults to 1. An omitted pool capacity defaults to
its members' total capacity. Every pool must contain named members. Names use
lowercase letters, digits, `_` or `-`, starting with a letter (max 64); capacities
are 1–65535. Optional `reason` works on pools and resources.

```sh
shoal resources                              # Configured capacities and own leases
shoal resource acquire devices               # Any available member
shoal resource acquire devices --resource beta --name tests --reason "Integration tests"
shoal resource acquire signing --wait 60      # Wait for the standalone resource
shoal resource list                          # Current worktree's actual leases
shoal resource release devices
shoal resource release devices --name tests
shoal resource release signing
```

Acquisition is explicit and lazy. Each semaphore lease takes one slot from both
the pool and selected member. An explicit member never silently changes to another.
Repeating the same pool/lease name (default `default`) returns the existing
permit; use distinct `--name` values for additional permits. Output always names
the chosen resource. `--json` returns the lease, or `acquired: false` with exit 2
when busy. `--wait` retries for up to the given seconds (max 3600); no fairness
or atomic multi-resource acquisition is promised.

Global names cannot be overridden by a different repo definition. Repo pools
are keyed by registered repo identity, so equal names in unrelated repos are
independent. A member's identity is its pool plus its name; the same spelling in
another pool does not automatically represent the same physical resource.
Restart the daemon for global config changes; repo config is read on request.
While leases are active, conflicting definitions block new acquisitions until
the definitions agree or the leases drain. Release still works after config edits
or removal. All sharing is within one Shoal daemon.

Shoal accounts for permits; it does not start, stop, isolate, or prevent direct
use of generic resources. Stop using a resource before releasing it. Leases
survive command exit and daemon restart, block automatic cleanup, and are freed
by successful worktree removal. Failed removal retains them. Scoped agents can
manage only their own leases; overview counts include other users without exposing
their lease records. Human callers can use `resource list --all`.


### Shared readers and exclusive writers

Set `kind = "rwlock"` on a standalone resource or a pool member:

```toml
[resources.shared-cache]
kind = "rwlock"

# The same setting works under [resource_pools.<pool>.resources.<member>].
```

```sh
shoal resource acquire shared-cache --mode read
shoal resource release shared-cache
shoal resource acquire shared-cache --mode write --wait 60
shoal resource release shared-cache
```

Any number of read leases can coexist; a write lease excludes all other readers
and writers of that resource. The lock is cooperative: a read lease is a promise
to read, not filesystem enforcement. A new rwlock lease defaults to `write`;
existing semaphore resources default to `permit`. Explicit read/write modes
only select rwlock members, and `--mode permit` only selects semaphores.

An rwlock's capacity must be 1 (the default). All readers on that member share
one pool slot; a writer uses one slot. The slot is freed after its final lease is
released. A pool may mix rwlocks and semaphores: its capacity counts occupied
rwlock members plus semaphore permits. Further readers of an occupied member
can join even when the pool has no free slots.

Use different `--name` values for simultaneous leases within the same worktree.
Repeating a lease name returns its existing mode; explicitly changing mode under
that name is rejected. Release before acquiring another mode; upgrades and
downgrades are not atomic. Writers may starve under a continuous stream of
readers: waiting is bounded polling, with no queue or writer priority.

`shoal resources` shows reader/writer counts and separate read/write availability;
its numeric `used`/`available` fields count pool/member slots, not reader limits.
Lease output always includes `mode`. Both modes survive daemon restarts, prevent
automatic worktree cleanup, and are released on successful manual removal.
Changing an active resource's kind requires draining its leases first.

## Agent skill outside project repositories

Install the skill bundled with the installed binary once at user scope. No
running daemon or source checkout is needed:

```sh
shoal skill install          # Codex and Claude Code
shoal skill install codex    # Only Codex
shoal skill install claude   # Only Claude Code
```

Homebrew installations symlink `SKILL.md` to the packaged skill through the stable
Homebrew `opt` path, so upgrades refresh it automatically. Cargo installations
copy the bundled instructions; repeat after upgrading to refresh them. Installation replaces
the existing `SKILL.md` and preserves other files in the skill directory. Codex
uses `~/.agents/skills/shoal/SKILL.md`; Claude uses
`~/.claude/skills/shoal/SKILL.md`, honoring an absolute `CLAUDE_CONFIG_DIR` override.
These are the documented user-level discovery locations for
[Codex](https://learn.chatgpt.com/docs/build-skills) and
[Claude Code](https://code.claude.com/docs/en/skills). The agent loads the skill when
relevant; it need not be launched by Shoal. No project needs a copy of the skill.
Run installation outside scoped workspace executions, since it updates user-level
configuration. `shoal --json skill install` reports the installed agents and paths.

An independently launched agent inside a managed worktree uses current-directory
resolution for Shoal commands. Its enclosing session still lacks Shoal execution
tracking and scope. In an ordinary checkout, use ordinary Git; Shoal resource
leases still require a managed workspace. Installing the skill does not register
or adopt a checkout. `shoal skill` still prints the bundled instructions;
`shoal --json skill` returns the text in a `skill` field for
integrations that manage their own instruction delivery.
