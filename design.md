# Shoal design

Decisions and proposals; usage: [README.md](README.md), behavior:
[docs/reference.md](docs/reference.md), contributor rules: [AGENTS.md](AGENTS.md).

## Purpose and boundaries

Shoal manages local Git workspaces, executions, and shared development
resources for humans and coding agents. Cleanup is part of resource ownership:
a disposable workspace must not leave substantial run-owned data behind.

Agents cooperate with Shoal's scope and allocations. Shoal does not stop a
hostile same-user process from bypassing them through Git, the filesystem, or
direct resource access; filesystem restrictions are future work.

Caller-specific integration stays outside the core: Superlogical owns terminals;
Macraft owns VM/container provisioning. Shoal does not provision tools, manage
browsers, schedule agent tasks, or store conversations. Agent shortcuts
(`claude`, `codex`, `happy`, `t3`) are thin launchers around the generic `exec`
path; CLI launcher argument arrays are configurable defaults; session and desktop
adapters retain their tool-specific setup.

## Architecture

One Rust binary provides the CLI, execution wrapper, and daemon. Each state
directory has one daemon shared across repositories, with a private Unix
socket and a versioned JSON protocol; the ordinary installation is per user.
The daemon owns SQLite state, allocation, lifecycle transitions, and recovery.

The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups, registers with the daemon, and handles stop requests.
The daemon never proxies terminals, and a lost connection is not proof that an
execution stopped or that its resources are free.

Use short transactions for atomic claims; typed lifecycle states retain lowercase
persisted/wire spellings and reject unknown values. Slow operations stay outside
transactions; failures and migrations preserve ownership. Setup preserves compatible daemons and
commands, deferring service changes until restart; incompatible daemons are
verified stopped before replacing their service.

Worktrunk creates and removes worktrees through Shoal's adapter with isolated
configuration and hooks disabled. Invoke external tools with argument arrays. Keep
writable state outside the binary's directory. Service setup manages one per-user
launchd/systemd service; foreground mode covers environments without a service
manager. The service captures the installing shell's `PATH`.

Distribution is a source-built Homebrew formula in the shared HN05 tap with a
release channel (immutable tags, selected explicitly) and a `main` channel.
Named configuration templates in `configs/` are embedded at build time, so
installation needs no source checkout or network access.
Build and packaging logic lives in Shoal's `scripts/install-homebrew.sh`; the
tap declares sources and dependencies. One Forgejo Actions workflow releases: it
bumps versions, validates, creates and merges a version PR under normal branch
protection, tags the exact merged commit, publishes merged-PR notes excluding release
preparation since the nearest earlier published ancestor release with issue links, and from
that tag updates both taps (Forgejo- and GitHub-sourced via a push mirror), attaches
cross-built Linux and macOS binaries, and recreates the release on GitHub, which
mirrors do not carry. It never tags a later commit, downgrades, or force merges;
tap updates retry against the tap's latest main without force pushes.

## Workspaces and Git

Every repository owns `~/shoal/<name>/` (global `root_dir`): its workspaces
are created inside it and a URL clone lives there as `.checkout`, a name no
workspace can take, so worktrees are grouped per repository, never nested in a
checkout, and outside the state directory; a root inside either is refused.
Directories are reserved atomically and never reused or moved; an
in-place checkout placed as `~/shoal/<name>/<x>` adopts that directory, and
`--path` clones elsewhere. Registration is idempotent by normalized origin URL,
then canonical path, never fetches, and keeps a stable UUID separate from the
display name. `repo rm` deletes the directory only once it is empty.

A new branch starts from the repository default branch: `origin/HEAD`, the sole
remote's HEAD, or the checkout's current branch without remotes, never a guessed
`main`. The selected local default branch is fast-forwarded from its upstream first,
preserving an ahead branch and refusing divergence, dirty or managed checkouts, and
failed fetches. `--ref` starts elsewhere without refreshing, except when it names
the default branch. Creation, default-branch refresh, setup, repository removal, and
recovery share a per-repository Git gate.

Creation accepts literal Git branch names and derives a portable, globally unique
workspace name and directory from them; a normalization collision fails without
touching existing work. Branch conflicts get numeric suffixes on the blocking
component only, never changing the workspace name. Worktree Git metadata identity is
recorded so moved or replaced directories are never adopted silently.
Existing-branch selection creates a worktree without suffixing; remote heads are
discovered live and become local tracking branches. Local selection preserves
commits; remote selection fast-forwards matching tracking branches. Ready owned
workspaces reopen without setup, hooks, or refresh; other checkouts block creation.
Existing worktrees use the local default as their diff base, or the opening commit
if unavailable or on that same branch. Never adopt main checkouts.

Named Git profiles live in global config; repository or global `git_profile`
selects one for newly created worktrees, overridden by `add --git-profile`.
Apply it before setup using Git's
per-worktree config, preserving other worktrees' settings. Enable the shared
`worktreeConfig` extension only when the existing repository layout supports it;
profiles cannot change layout or extensions. Reopening keeps existing settings
and rejects an explicit profile flag.

Repository config may name `setup_cmd`, `post_setup_cmd`, and `pre_remove_cmd`:
single executable paths resolved against the worktree, run directly without shell
parsing or PATH lookup, with the worktree as working directory. Setup runs through
the tracked wrapper with workspace scope; the daemon owns readiness and execution
records, and `add` keeps the workspace preparing until setup exits cleanly with no
survivors. Failures preserve files, branches, and leases; interactive callers choose
delete, ignore (which repairs verified state first), or keep, while JSON callers get
a nonzero exit. `prepare` reruns setup explicitly; nothing retries automatically.

Hooks are deliberately untracked user processes with the workspace identity
but no scope token, because their purpose is to start or stop things that
outlive the hook (a tmux session, say) without becoming execution survivors.
The post-setup hook runs from the CLI with the terminal once the workspace is
ready and before any agent; failure keeps the ready workspace. The pre-remove
hook runs in the daemon inside the single removal path for manual, repository,
and automatic removal, after checks pass and commands stop, bounded in time;
failure retains the workspace. Both hook keys share the setup path rules.

`diff` compares against the recorded base's fork point (merge-base fallback, fixed
commits stay fixed) with native Git settings, so advancing the base is never shown
as work. `pull` fast-forwards the repository default branch only; without remotes it
reports nothing to pull. `merge` imports any local or remote branch into the
workspace's own branch, preferring local sources, which it first fast-forwards from
their upstream unless `--local`, they lack an upstream, or a managed workspace has
them checked out; remote discovery must be unambiguous, and conflicts are left for
ordinary Git. `land`, the local substitute for a pull request, merges the workspace
branch into the default branch (refreshed from its upstream first) without pushing
and aborts a merge that does not apply cleanly, leaving conflicts to a `merge` of
the default branch into the workspace. Landing holds the repository Git gate for
the tracked execution; the daemon validates and refreshes, and the CLI worker merges.
After cancellation, the wrapper rolls back incomplete merges after stopping the
process group, preserving completed merges and reporting unsafe recovery failures.
Fetches use private temporary refs; merges use the tracked wrapper and cooperative
own-branch checks.

## Scope and user interfaces

Commands launched through Shoal inherit a daemon-validated scope token that
confines them to their own workspace: inspect, execute, merge, and resources.
`pull`, `land`, creation, removal, reconciliation, other workspaces, repository
administration, and service control need an unscoped caller. PR registration and
manual merge acknowledgement are own-workspace exceptions; nested executions keep scope.

The CLI takes explicit targets and `--json` for automation, and uses
current-directory resolution and fzf interactively; noninteractive calls never
open a picker. Human output uses a shared semantic palette at the CLI presentation
layer; machine output and stored values stay unstyled. Rust chooses paths,
including `<root_dir>/<repo>` after removal; the Bash/Zsh wrapper changes directory
without evaluating repository code.
Confirmations show the action and ask `[y/N]`, cancel on Enter, `n`, EOF or
Ctrl-C (also after a tracked execution), and are bypassed only by
explicit flags such as `--yes`. Removal's branch choice stays separate from
its confirmation; explicit branch flags skip the choice only.

Completion queries the installed binary per Tab and uses read-only daemon calls
with a 500 ms timeout for live targets, honoring state directory and scope and
never starting a daemon or picker. Targets sort before flags, including in fzf-tab.

Named `[commands]` are executable/argument arrays resolved per name from saved
repository config, worktree config, then global config and built-in defaults at
launch. They run through the tracked wrapper with workspace scope, terminal I/O,
and literal extra arguments. Built-in commands without configurable argument
arrays reserve their names. Repository-only commands require a current or
explicit workspace, so unknown names never open a picker. Workspace fields expand once within individual
arguments; a standalone `{args}` places the caller's literal arguments.
`{diff_base}` lazily uses `diff`'s daemon lookup. Review tools use these commands;
the tools own review storage, exports, and forge authentication, with explicit
feedback handoff to agents.

Agent shortcuts use the execution wrapper: by default Codex CLI gets full access without
approvals, Claude gets remote control named after the workspace. Both trust the
workspace in their user config before launch, creating the file if needed; Happy
launches and Codex app handoffs do so too. General agent templates become native
CLI instructions or a first-message prefix for Happy Codex; desktop handoffs carry no instructions.
Codex's default mode is a config value read at launch.
`add --issue` resolves issue numbers/URLs using the remote and existing gh/fj
login, derives a portable name, and renders issue context from a plain-text
template for CLI agents; forge
lookup stays in the CLI with no Shoal credentials or forge configuration. `issue
<url>` is the same flow from a pasted link: the URL selects the registered
repository by remote identity (never cloning), and the configured `default_agent`
stands in for `--agent`, so a paste yields a workspace with an agent working.
`add --agent` launches only after creation, setup, and the post-setup hook
succeed, or after an explicitly ignored setup failure, and retains the
workspace whatever the agent does. Desktop handoffs (Codex app, T3) provide no
tracking or scope; users disable automatic cleanup when that activity cannot
be tracked. Happy sessions are the phone-driven flow: a console session that
Happy's daemon started in a non-workspace directory creates workspaces and starts
sessions in them. Shoal launches `happy <agent>` with the daemon's own flags so
the session registers with Happy and appears in the app, detached from the
terminal but inside the tracked wrapper (a background `shoal` process holding the
execution), so it stays stoppable and visible to cleanup. Happy's Codex mode takes
no prompt argument, so Shoal delivers prompts the way the app does: it creates the
session on Happy's server with the machine's own Happy login, encrypted as
happy-cli would, attaches the CLI through Happy's reconnection variables, and posts
the first message once the session is alive, keeping a copy on disk when that
fails. Shoal reads Happy's credentials only for this and stores none. A Happy-side
pre-spawn hook asking Shoal for a workspace was considered and not adopted.

Notifications stay in the terminal: the daemon records what a user would
otherwise miss (busy resources and who holds them, port conflicts, exits of
shortcut-launched agents, workspaces it removed or retained on its own) and the
CLI shows them once, on request or as a followed stream that also raises the
terminal's own notifications (OSC 9); `list` and `daemon status` only count them. Recording never fails the operation it describes,
repeated polled conflicts collapse until read, and scoped processes cannot read
them. Desktop or push delivery was considered and not adopted.

The skill is installed at user scope for Codex and Claude, independent of the
daemon and never from a scoped execution; its availability registers nothing.

## Resource ownership

Reservations and leases belong to a worktree, survive command exit, restarts,
and failed removal, are allocated atomically with idempotent lease names, and
are released explicitly or by successful removal. Resources are lazy, never
claimed at creation.

Global TOML is machine policy, seeded by `setup` with the stated defaults when
absent and replaced only by an explicit reset or named template installation,
with the previous file kept as a backup; the daemon reads it at startup,
while the CLI reads agent settings per command. Repository TOML comes from the
worktree (`.shoal.toml` or `.shoal/config.toml`, both together is an error) with
a local override stored in the database by repository ID layered over it per
option, a named table replacing the one below it whole, and deleted with the
registration. Every option
that does not describe the machine may be set at either level and resolves per
option: saved config, worktree file, global config, then the built-in default.
Prompt templates follow the same precedence, with repository-root Markdown files
and global files beside `config.toml` below inline TOML values at each level.
Setup installs missing templates from the bundled repository-root defaults;
rendering substitutes known fields once without evaluating their contents.
Before a workspace exists, the registered checkout's file stands in for the
worktree's. Repository config is read per request, so changes need no restart
and leave existing leases alone. Repository config cannot expand machine
policy: `root_dir`, simulator limits and profiles, Git profile definitions, and global resource
definitions stay global; global pools span repositories, repository pools span
that repository's worktrees.

Ports are cooperative TCP reservations: probe, record, export to later
executions, never hold a socket. Simulator leases are exclusive over
Shoal-created devices only: persist claims before mutating, keep them after
failure, never preempt active leases, count external devices toward capacity,
preserve device state on handoff, require `--clean --reason` for erasure with
an audit record written before the destructive step, and delete devices on
removal or idle expiry. Generic permits consume pool and member capacity in
one transaction; rwlock members allow unlimited readers sharing one slot or one
writer, default to write, and require release to change mode. Definition drift
blocks new claims but never revokes permits. Shoal does not manage the
underlying resources.

## Removal and recovery

Manual and automatic cleanup share one path: establish ownership, stop owned
executions, run the pre-remove hook, remove owned simulators, remove the worktree,
and release leases with the record. Failures retain what is needed to retry.
Manual removal deletes a redundant branch (tree equal to the local default or its
upstream, or merged into the default) and otherwise requires an explicit keep or
delete choice. The default branch is retained unless deletion is explicit.
Automatic cleanup removes only clean, idle worktrees whose commits are all on a
remote or the default branch, with no executions, directory users, leases, or
permits, rechecked immediately before deletion, without fetching. `repo rm`
deletes the checkout and every workspace through that path, refuses external
worktrees and dangerous paths, persists progress, and blocks new workspaces until
an interrupted removal is retried.
PR cleanup is separately enabled by default: persisted watches use the
user's gh/fj login and require a merged PR containing HEAD; manual acknowledgement
binds to HEAD. Both stop tracked agents immediately through shared removal,
rechecking clean files and HEAD after stopping. Failures retain work and leases;
registered workspaces are excluded from idle cleanup until cleared or removed.

Reconciliation reports by default; repair restores verified worktrees and
clears executions proven stopped while preserving work and leases. Startup
audits but never deletes, kills, clears unknown executions, or releases leases.
Survivors are signaled only after verifying PID birth identity and same-user
ownership; acknowledgement cannot override visible live processes. Moved or
replaced worktrees stay unresolved until restored. A deleted directory means
the user removed the worktree: the cleanup sweep forgets it through the shared
removal path, releasing its leases and retaining its branch, unless it has
commands Shoal cannot verify stopped.

## Remaining work

Implementation order: CLI/daemon, workspaces, ports, simulators, lifecycle
polish, then filesystem restrictions. Open items:

- **External sessions:** a generic attach/hold contract before promising
  tracking or cleanup for GUI agents.
- **Storage policy:** ownership and retention for run data outside the
  worktree, caches, logs, and audit history.
- **Distribution:** Homebrew bottles; stable service identity across
  upgrades; native Linux service and recovery validation.
- **Execution environments:** host/guest and cross-user coordination;
  independent state directories currently have independent capacity.

### Filesystem restrictions: decided policy, not implemented

Provide guardrails for cooperative agents plus an explicit unrestricted mode.
Read and write access are separate. Precedence is global deny, global allow,
repository grant, then deny; global denies win inside allowed directories and
there is no implicit whole-home grant. Define an inspectable baseline for the
worktree, temporary storage, runtime dependencies, shared Git metadata, and
tool state; resolve symlinks and separate writable access from cleanup
ownership. Unsupported policies fail clearly rather than launching
unrestricted. Seatbelt is the intended macOS backend; Landlock is a Linux
candidate pending nested allow/deny semantics, with bubblewrap as fallback.
Restrictions live in the launcher, not the daemon.
