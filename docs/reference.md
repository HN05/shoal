# Shoal command reference

Detailed command behavior and configuration. Start with the [usage guide](../README.md).

- [Installation and upgrades](#homebrew)
- [Daemon](#daemon)
- [Workspaces](#workspaces)
- [Setup and hooks](#workspace-setup-and-hooks)
- [Removal and cleanup](#remove-a-workspace)
- [Ports](#port-reservations)
- [Shell integration](#shell-navigation)
- [Recovery](#recovery)
- [Simulators](#simulators-macos)
- [Resource pools](#cooperative-resource-pools)
- [Agent skill](#agent-skill-outside-project-repositories)

## Homebrew

The [HN05 tap](https://github.com/HN05/homebrew-tap) offers tagged releases (the
default) and the current `main` branch (`--HEAD`). Both build from source and
install Worktrunk, Git, fzf, and lsof as runtime dependencies.

```sh
brew tap hn05/tap
brew install hn05/tap/shoal            # Or: brew install --HEAD hn05/tap/shoal
shoal skill install
shoal setup
```

Upgrade with `brew update && brew upgrade hn05/tap/shoal` (`--fetch-HEAD` for
`main`), then `shoal daemon restart`. The channels share one installation,
daemon, and skill path; to switch, run `shoal daemon stop`, uninstall, install
the other channel, and `shoal daemon start`. State and skill links live outside
the package and survive. Skill links follow Homebrew's stable `opt` path; a
skill copied by an older install needs one `shoal skill install` to migrate.
Build and packaging logic lives in `scripts/install-homebrew.sh`; the tap only
declares versions and dependencies and invokes that script.

## Daemon

```sh
cargo install --path .
shoal setup --dry-run       # Preview the OS service definition
shoal setup                 # Register and start the per-user service
shoal daemon status|stop|start|restart
```

macOS uses a launchd LaunchAgent in `~/Library/LaunchAgents` (requires a GUI
login session); Linux uses a systemd user service. Without a service manager,
run `shoal --state-dir /tmp/shoal-dev daemon run` in the foreground and target
it with the same `--state-dir` (or `SHOAL_STATE_DIR`). State defaults to
`~/.local/state/shoal`; runtime state and the Unix socket are private to the
user. Service commands target the one registered per-user service, whose state
directory must match. `daemon status` exits 1 when offline.

`shoal setup` preserves the invoked executable's symlink path and captures the
current `PATH` for the service, so install `wt`, `git`, `lsof`, and any hook
tools first. Setup restarts an incompatible installed daemon automatically;
foreground daemons must be stopped manually. macOS diagnostics go
to `daemon.log` in the state directory; Linux uses `journalctl --user -u
shoal.service`. Tests use an isolated service-manager fixture and never install
a real service; native Linux service integration is untested on a Linux host.

## Workspaces

`add --issue <number-or-url>` reads the registered repository's issue using `gh`
for github.com or `fj` for Forgejo remotes. Install the appropriate CLI and use
its existing login (`gh auth login` or `fj auth login`); no Shoal forge config or
tokens are needed. URLs must match the repository. Lookup failures create nothing.
Names default to `issue-<number>-<title-slug>`; `--name` overrides this. With
`--agent`, the issue title, URL and details supply the initial prompt (forward
agent options after `--`, not a second prompt). Codex uses CLI mode for issue
prompts even when its default is `app`. Ordinary setup, hooks and collision rules apply.

```sh
shoal repo add /path/to/repo             # Or a Git clone URL; register once
shoal repo add /path/to/repo --name my-project
shoal repo rename my-project new-name
shoal repo list
shoal add my-project --name fix-login
shoal add my-project --name fix-api --agent codex -- "Fix the API timeout"
shoal cd                                 # Fuzzy picker, even inside a workspace
shoal cd fix-login                       # Enter through the shell function
shoal cd -                               # Previous directory
shoal exec fix-login -- cargo test
shoal claude fix-login -- --help
shoal codex                              # Current workspace or picker; default mode
shoal codex cli fix-login -- --help
shoal codex app fix-login                # Codex desktop app
shoal t3 fix-login                       # Running T3 Code desktop app
shoal inspect fix-login
shoal stop fix-login                     # Stop commands; keep the worktree
shoal rm fix-login                       # Remove; choose what to keep if work differs
```

Bare `shoal` opens an fzf list of workspaces (`shoal --help`, or bare `shoal`
without a terminal, prints help). Enter enters the selection; Ctrl-D deletes,
Ctrl-E runs Claude/Codex CLI, opens Codex/T3 apps, or runs a shell command,
Ctrl-A adds, Ctrl-O inspects, Ctrl-S stops, Ctrl-F shows the diff. Each action
returns to your shell.

Omitted targets open an fzf picker; `rm`, `exec`, `claude`, `codex`, `t3`,
`diff`, `pull`, and `merge` first use the workspace containing the current
directory. `shoal add` offers repositories in most-recently-used order, then
a new-branch prompt or existing-branch picker. Noninteractive and JSON calls never prompt;
management commands support JSON output, while executed commands keep their
own stdin, stdout, stderr, and exit code.

### Agents

`add --agent codex|claude` starts the agent after worktree creation, setup, and
the post-setup hook succeed; arguments after `--` go to the agent. CLI agents
run in your terminal through the tracked execution wrapper and return the
agent's exit code; the workspace is retained even when launch fails. With shell
integration, your shell enters the new workspace after the agent exits.
`--json` emits the workspace record first, then the agent's unmodified output.

`shoal claude` appends `--remote-control <workspace-name>`. It also marks the
workspace directory as trusted in Claude Code's `~/.claude.json` (or
`$CLAUDE_CONFIG_DIR/.claude.json`) when that file exists, so Claude starts
without its workspace trust dialog. `shoal codex cli` appends `--sandbox
danger-full-access --ask-for-approval=never`. Use `shoal exec ... -- claude` or
`-- codex` for a custom invocation.

`shoal codex` without `cli`/`app` uses `codex.default_mode` from
`~/.config/shoal/config.toml` (or `$XDG_CONFIG_HOME/shoal/config.toml`),
read at launch without a daemon restart:

```toml
[codex]
default_mode = "cli" # Or "app"
```

App launches run `codex app <path>` or `t3 app <path>` with any `--` arguments
and preserve the launcher's output and exit code. They add no agent flags and
provide no execution tracking, scope token, or port variables; T3's app must
already be running. Disable automatic cleanup when using an app whose activity
Shoal cannot track.

### Branch and workspace names
`--name` creates a literal Git branch; `--branch <branch|remote/branch>` uses an
existing one (incompatible with `--name`, `--issue`, and `--ref`). The picker
queries remotes for current branches. Local branches take precedence and remain
unchanged; remote selections fetch and create tracking branches, or fast-forward
a matching local tracking branch without discarding ahead commits. Ambiguous
remotes or unrelated local branches fail. A ready Shoal workspace reopens without
setup/hooks or refresh; other checkouts block creation, including checked-out `main`.
Full `refs/heads/...` and `refs/remotes/...` selectors disambiguate names.
The workspace name replaces non-ASCII-alphanumeric/`-_` characters with `-`, drops
leading `-_`, truncates to 64 characters, and falls back to `workspace`. Globally
colliding names fail. Commands use this name or ID; directories live in the repo root.

For new branches, names taken by a local branch, known remote branch, branch
namespace, or retained Shoal record get `-2`, `-3`, etc. on the leaf; when an
ancestor blocks it, that component is suffixed (`feature` makes `feature/topic`
into `feature-2/topic`). `HEAD`, Worktrunk's `@`, and full hex object IDs are
reserved. Suffixes never change the derived workspace name.

### Base branch
New branches start from the repository's default branch: `origin/HEAD`, or the
sole remote's HEAD without `origin` (several remotes without `origin` are
ambiguous). A missing symbolic remote HEAD is discovered with `ls-remote` and
cached; update it with `git remote set-head origin --auto`. Without remotes, the
registered checkout's current branch is used; a detached checkout needs `--ref`.

Before branching, Shoal fetches that local branch's upstream and fast-forwards
it, even when the registered checkout is on another branch. A missing branch or
upstream, failed fetch, divergence, or a dirty or managed default-branch
checkout stops creation; an already-ahead branch is preserved. `--ref <git-ref>`
starts elsewhere without refreshing, except the default branch. Existing-branch
workspaces record the local default for diff, or their starting commit if unavailable.

### Repositories

Register a local checkout in place (no remote required) or a clone URL. Each
repository gets `~/shoal/<name>/`, named by `--name` or the source basename
without `.git`, suffixed `-2`, `-3` on conflict with files or recorded paths;
its workspaces are created inside it and a URL clone lives there as
`.checkout`. A local checkout already at `~/shoal/<name>/<anything>` keeps
that directory. `root_dir = "~/Projects"` (formerly `repositories_dir`) in the
global config (absolute or `~/` path outside Shoal's state directory and every
checkout; daemon restart required) changes the parent for new registrations;
existing ones keep their paths, as do workspaces created before this layout.
`repo add <url> --path <dir>` clones one repository to an exact new directory
(relative to the current directory; `~/` allowed). Re-registering a URL with
its existing path is fine; a different path is rejected.

Registration is idempotent by normalized `origin` URL (HTTPS/SSH forms and
`.git` suffixes match) or canonical local path, and never fetches. Clone URLs
lose trailing slashes so worktrees get a remote forge CLIs recognize. Names from
`--name` or `repo rename` are unique and work as selectors alongside IDs, paths,
and source URLs; inferred names work when unambiguous.

### Store repository config outside Git

```sh
shoal repo config my-project --file ~/project-shoal.toml
shoal repo config my-project          # Print the saved TOML
shoal repo config my-project --clear  # Return to worktree config
```

The file uses the `.shoal.toml` format and is validated and copied into Shoal's
database; reimport it after edits. It replaces the entire worktree config for
every workspace of that repository (no merging; an empty file selects all
defaults) and applies on the next request without a restart. Without a saved
config, Shoal reads `.shoal.toml` or `.shoal/config.toml` from each worktree
and rejects both together. The saved config survives restarts, renames, and
workspace removal; `repo rm` deletes it. `--json` returns `repository_id` and
`toml`. Scoped workspace commands cannot administer it.

### Delete a repository

```sh
shoal repo rm my-project --yes
```

Permanently deletes the checkout (including in-place local repositories with
uncommitted or unpushed work), all its workspaces and branches, their ports,
simulators, and leases, and the saved config, stopping managed commands first.
Interactive calls ask `Are you sure? [y/N]`; `--yes` skips the prompt and is
required for scripts and `--json`. `repo remove` is an alias.

Linked worktrees outside Shoal must be removed first; prunable stale records do
not block, locked worktrees do. Shoal refuses redirected paths and deletions
that would include another registered repository or its own state. If cleanup
fails, completed steps stay done, remaining records are retained, and new
workspace creation is blocked until the same command is retried.

### Pull the default branch

```sh
shoal pull                 # Current workspace (or fzf); agents resolve to their own
shoal --json pull          # Branch, previous/current commits, and whether it changed
```

Fast-forwards the repository's default branch from its configured upstream
(which may differ from the default remote). It does not touch the feature
branch. A checked-out default branch must be clean; divergence or a default
branch checked out in a managed workspace is an error. Hooks and recursive
submodule updates are disabled. Scoped agents cannot pull; `shoal merge`
refreshes its source for them.

### Merge into your workspace branch

```sh
shoal merge main                           # Any local branch: fast-forwarded from upstream first
shoal merge feature/api                    # Local, or discover a remote-only branch
shoal merge feature/api --local            # Merge the local branch as it is, no refresh
shoal merge feature/api --remote origin    # Fetch explicitly, even if local exists
shoal merge origin/feature/api fix-login   # Qualified source, named destination
```

The destination must be the workspace's recorded branch. Local branches take
precedence and are first fast-forwarded from their upstream under the `pull`
rules above (a dirty checkout or divergence is an error; `--local` skips this).
A local branch without an upstream, or checked out in a managed workspace, is
merged as it is. Otherwise Shoal queries configured remotes and fetches the branch.
Several matches or an unreachable remote require `--remote`. Qualified remote
sources and full `refs/…` names always fetch fresh data. Git fast-forwards or
creates a merge commit; conflicts stay in the worktree for `git commit` or `git
merge --abort`, and `--json` reports `success`, `exit_code`, commits, and Git
output. Nothing is stashed, reset, or pushed.

### Diff

```sh
shoal diff                 # Current workspace, otherwise fzf
```

Runs native `git diff` (your pager and external diff apply) from the fork point
of the base branch recorded at creation, using its reflog with merge-base as a
fallback, so commits added to the base later are excluded even after a rebase.
Fixed-commit bases use that commit; older workspaces without base metadata use
`main`. Committed, staged, and unstaged tracked changes appear; untracked files
follow Git. A missing or unrelated base is an error.

### Workspace setup and hooks

Repository config (`.shoal.toml`, `.shoal/config.toml`, or the imported local
config) names up to three executables:

```toml
setup_cmd = "scripts/setup.sh"        # Prepares the worktree; must exit 0
post_setup_cmd = "scripts/attach.sh"  # After the workspace is ready, e.g. open tmux
pre_remove_cmd = "scripts/detach.sh"  # Before the worktree is removed, e.g. close it
```

Each value is one path, relative to the worktree root or absolute, run with the
worktree as working directory. Give scripts a shebang and put arguments and
shell logic inside them. Setup is never inferred from package manifests.

`setup_cmd` runs through the tracked execution wrapper with workspace scope and
your CLI environment. `shoal add` waits for it before entering the worktree or
launching an agent; the workspace stays `preparing` until it exits 0 with no
surviving processes. On failure, interactive mode asks whether to delete the
new workspace and branch, ignore the failure and continue, or keep it for
inspection (default). JSON/noninteractive mode returns nonzero, keeps the
workspace, and sends setup output to stderr. Then choose explicitly:

```sh
shoal prepare fix-login                    # Rerun setup and the post-setup hook
shoal reconcile fix-login --repair          # Ignore the failure after ownership checks
shoal rm fix-login --yes --delete-branch    # Delete this workspace and branch
```

Hooks are untracked: they run as your own processes with `SHOAL_HOOK`
(`post_setup` or `pre_remove`), `SHOAL_WORKSPACE`, `SHOAL_WORKSPACE_ID`, and
`SHOAL_STATE_DIR`, without a scope token or port variables, so whatever they
leave running (a tmux server, say) is not a Shoal execution. `post_setup_cmd`
runs from the CLI with your terminal after `add` or `prepare` has a ready
workspace and before any `--agent`; a nonzero exit keeps the workspace, skips
the agent, and fails the command. `pre_remove_cmd` runs inside the daemon for
`rm`, `repo rm`, and automatic cleanup, after the removal checks pass and
managed commands stop, without a terminal and with a 60-second limit; a nonzero
exit or timeout retains the workspace with the hook's stderr as its error. It
is skipped when the worktree directory is already gone.

### Remove a workspace

Removal retains the default branch unless `--delete-branch`; other clean branches
matching the default or upstream are deleted. Otherwise fzf offers Cancel (default),
Keep branch (files only), or Delete branch, followed by a summary and `Are you
sure? [y/N]`. Both choices discard uncommitted and untracked files.
`--keep-branch` or `--delete-branch` skips the picker but not the confirmation;
scripts use `--yes` with one of them, since `--yes` alone does not choose for
dirty or differing work. Ctrl-C cancels any Shoal prompt.

Running processes do not block manual removal: connected Shoal commands and
identity-verified survivors are stopped, unrelated processes are left alone.
Ignored files are removed, shared caches are not, and Git protects branches
checked out elsewhere; output reports the actual branch result. Worktrunk hooks
are disabled; use Shoal's setup and hooks instead. Removing the workspace
containing your shell moves you to its repository root (or home).

### Automatic cleanup

Enabled by default: an idle, clean, fully pushed worktree is removed after 10
minutes through the same path as manual removal. Whether or not it is enabled,
a worktree directory deleted outside Shoal is forgotten on the next sweep, its
ports, leases and simulators released and its branch retained; a moved worktree
or one with recorded commands is left for `shoal stop`, `shoal rm` or
`shoal reconcile`. File changes (including
ignored files), HEAD changes, and Shoal commands reset the timer; running or
unknown commands, processes with a working directory in the worktree (open
shells included), dirty files, unpushed commits, simulator leases, resource
permits, and failed checks prevent it. "Pushed" means reachable from locally
known remote branches; Shoal does not fetch. Sweeps run about every 30 seconds
and timers restart with the daemon. `lsof` must be on the daemon's `PATH`.

```toml
[auto_cleanup]
enabled = false # Default: true
idle_minutes = 10
```

Restart the daemon after changing this.

### Port reservations

```sh
shoal port reserve web fix-login --reason "Frontend dev server"
shoal port reserve api --port 3001 --env API_PORT --reason "HTTP API"
shoal port list fix-login              # --all for every workspace
shoal port release web fix-login
shoal ports                            # Configured and reserved ports here
shoal exec fix-login -- sh -c 'my-server --port "$API_PORT"'
```

Named TCP reservations belong to the worktree, persist across command exits,
`stop`, and restarts, and are released by successful removal. Repeating a name
returns the same port (`--reason` may update it); changing the number or
variable requires release first. Later `exec`, `claude`, and `codex cli`
commands receive `SHOAL_PORT_<NAME>` or the `--env` variable; running
processes keep their environment, and nested executions drop the parent's port
variables. Automatic allocation uses 49152–65535, configurable with `[ports]`
`start`/`end` in the global config; `--port` may name any nonzero port. Shoal
probes IPv4/IPv6 availability and prevents duplicates within the daemon, but
reservations are cooperative and unrelated processes can still bind. UDP is
not supported.

Repository defaults, in `.shoal.toml` or the imported config:

```toml
[ports]
on_conflict = "suggest" # or "auto"

[ports.web]
port = 3000
env = "PORT"
reason = "Frontend dev server"
```

`shoal port reserve web` allocates on request; CLI flags override. A conflict
suggests a free port: fzf offers to accept it, `--json` returns `reserved:
false` with exit 2, and `--port <suggested>` or `--on-conflict auto` accepts.

### Shell navigation

Add `source <(shoal shell init)` to `.bashrc`/`.zshrc` (or `eval "$(shoal shell
init)"` for Bash without process substitution) and run it in open terminals,
including after upgrades. The function lets `add` and `cd` enter workspaces
and moves you out of a removed one. `shoal cd` always opens fzf, `shoal cd
<name>` goes directly, and `shoal cd -` returns to the shell's previous
directory (`OLDPWD`, per shell), refusing a deleted destination. Scoped agents
cannot navigate outside their worktree. Without the function, Shoal prints the
destination; `--json` returns the path and never changes directory.

### Tab completion

The same shell integration enables Bash and Zsh completion (Zsh's completion
system is initialized if needed). Each Tab asks the installed binary for
subcommands, flags, fixed values, and paths without a daemon; with a daemon
running it also suggests repositories, workspaces, pools, members, lease names,
ports, and simulator leases for the current or named workspace, honoring
`--state-dir` and execution scope with a 500 ms timeout and never starting a
daemon or picker. Targets sort before flags, also in fzf-tab. `shoal
completions <shell>` prints scripts for Bash, Zsh, Fish, PowerShell, and Elvish.

### Recovery

```sh
shoal reconcile fix-login                  # Report only
shoal --json reconcile --all
shoal reconcile fix-login --repair         # Repair verified state, retain work and leases
shoal reconcile fix-login --repair --stop  # Also stop verified surviving commands
```

Exit 2 while issues remain, 0 when resolved; JSON is an array of reports.
Reconciliation is unavailable inside scoped executions. Startup marks
interrupted lifecycle operations failed and disconnected executions unknown,
and audits worktrees without deleting files or releasing leases. Repair
restores verified worktrees to ready and clears executions proven stopped;
connected commands keep running unless `--stop`. Moved worktrees must return to
their recorded path, replaced metadata is refused, and a deleted directory is
forgotten by the next cleanup sweep or `shoal rm`, retaining the branch.

Executions record wrapper and child identities plus a process group.
Descendants inherit `SHOAL_EXECUTION_ID`, which recovery uses with live
ancestry to find survivors; identities are rechecked before signaling, and
unverified candidates are never killed. Detection is cooperative: hidden
environments, cleared markers, and old records can leave it uncertain. After
checking yourself that such processes stopped, use `--repair
--acknowledge-stopped`; visible live processes still block. Linux recovery is
untested on a Linux host.

### Scoped workspace commands

Commands launched through `exec`, `claude`, and `codex cli` carry a scope token
that confines them to their own worktree: inspect, execute, `merge`, `diff`,
and resources. They cannot `pull`, reach other worktrees, remove workspaces,
administer repositories, or control the daemon; nested commands keep the scope. Scope is cooperative and does not
restrict direct filesystem or Git operations.

## Simulators (macOS)

Shoal creates, boots, shares, and deletes its own Xcode simulators. Configure
profiles in the global config and restart the daemon:

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

`shoal sim catalog` lists installed device types and runtimes; nothing is
downloaded. Repository config may set `[simulators] preferred = ["phone"]`;
flags override it, and `allow_any = true` permits `--device`/`--runtime`
requests with a `--reason`.

```sh
shoal sim acquire                   # Current worktree, configured preference
shoal sim acquire --profile phone --name tests --wait 60
shoal sim list                      # --all for every managed device
shoal sim release tests             # Or the default lease
```

Acquisition returns a ready device UDID; use it explicitly with `simctl` or
`xcodebuild -destination 'platform=iOS Simulator,id=<UDID>'`. Repeating a lease
name returns the same device. Leases belong to worktrees, survive command exit
and restarts, block automatic removal, and are deleted with the workspace. At
capacity, idle managed devices shut down first; active leases and personal
simulators are never touched, external booted devices count toward the limit,
and busy requests exit 2 unless `--wait <seconds>`. Released devices keep apps
and settings until the idle timer deletes them (checked every 15 seconds).
Failed operations retain records for retry. Only the default CoreSimulator
device set and one daemon are covered.

### Clean devices and audit history

```sh
shoal sim acquire --clean --reason "Verify first-launch permission prompts"
shoal sim history                  # --all, --limit 50, --before <id>
```

Normal handoff preserves device state. `--clean` returns a fresh or erased
device, needs a reason, and cannot erase an active lease. Shoal prefers spare
capacity, otherwise erases or replaces the idle device with the fewest known
user apps. History records workspace, execution, reason, outcome, device,
planned resets, estimated app loss, and whether the erase completed, including
busy and failed attempts; it survives removal and restarts, and scoped commands
see only their own entries. Erasing devices outside Shoal bypasses this audit.

## Cooperative resource pools

Global config defines resources shared across repositories; repository config
defines resources shared across that repository's branches. Both use:

```toml
[resources.signing]            # Standalone mutex; raise capacity for a semaphore
capacity = 1
reason = "Signing service"

[resource_pools.devices]
capacity = 2                   # Total permits across the pool

[resource_pools.devices.resources.alpha]
capacity = 1

[resource_pools.devices.resources.beta]
capacity = 2                   # Within the pool limit
```

Capacities default to 1 (pools to their members' total); every pool needs
members. Names are lowercase letters, digits, `_`, or `-`, starting with a
letter, at most 64 characters; capacities are 1–65535.

```sh
shoal resources                              # Capacities and own leases
shoal resource acquire devices               # Any available member
shoal resource acquire devices --resource beta --name tests --reason "Integration tests"
shoal resource acquire signing --wait 60
shoal resource list                          # --all for every workspace
shoal resource release devices --name tests
```

Each lease takes one slot from the pool and the chosen member; an explicit
member never changes silently. Repeating a pool and lease name (default
`default`) returns the existing permit. `--json` returns the lease or
`acquired: false` with exit 2; `--wait` polls for up to 3600 seconds without
fairness. Global names cannot be redefined by a repository; repository pools
are keyed by repository, so equal names elsewhere are independent. Global
changes need a restart, repository config is read per request, and conflicting
definitions block new claims until they agree or leases drain. Leases survive
command exit and restarts, block automatic cleanup, and are released by
successful removal. Shoal accounts for permits only; stop using a resource
before releasing it.

### Shared readers and exclusive writers

Set `kind = "rwlock"` on a standalone resource or pool member and acquire with
`--mode read` or `--mode write` (new rwlock leases default to write; semaphores
use `permit`). Readers coexist and share one pool slot, freed by the last
release; a writer excludes everyone. Capacity must be 1. Repeating a lease
name returns its mode; changing mode requires release first, and there is no
writer priority. `shoal resources` shows reader and writer counts with
separate read/write availability.

## Agent skill outside project repositories

```sh
shoal skill install          # Codex and Claude Code
shoal skill install codex    # Or: claude
```

Installs the bundled `SKILL.md` at user scope with no daemon: Codex at
`~/.agents/skills/shoal/SKILL.md`, Claude at `~/.claude/skills/shoal/SKILL.md`
(honoring an absolute `CLAUDE_CONFIG_DIR`). Homebrew installs symlink to the
packaged skill so upgrades apply automatically; Cargo installs copy it, so
rerun after upgrading. Other files in the skill directory are preserved. Run it
outside scoped executions. `shoal skill` prints the instructions (`--json`
returns a `skill` field). An agent launched independently in a managed
worktree uses current-directory resolution but has no execution tracking or
scope; in an ordinary checkout, use ordinary Git.
