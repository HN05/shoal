# Shoal design

Product decisions and open work. Usage belongs in [README.md](README.md),
behavior in [docs/reference.md](docs/reference.md), contributor rules in
[AGENTS.md](AGENTS.md). Change decisions in place; never append milestone
reports, test inventories, or investigation notes.

## Purpose and boundaries

Shoal manages local Git workspaces, executions, and shared development
resources for humans and coding agents. Cleanup is part of resource ownership:
a disposable workspace must not leave substantial run-owned data behind.

Agents cooperate with Shoal's scope and allocations. Shoal does not stop a
hostile same-user process from bypassing them through Git, the filesystem, or
direct resource access; filesystem restrictions are future work.

Caller-specific integration stays outside the core. Superlogical owns its
terminal sessions and requests preparation, execution, and removal; Macraft
owns VM and container provisioning. Shoal does not provision build tools,
manage browsers, schedule agent tasks, or store conversations. Agent shortcuts
(`claude`, `codex`, `t3`) are thin launchers around the generic `exec` path;
their agent-specific flags and setup are meant to become configurable defaults
rather than deeper integration.

## Architecture

One Rust binary provides the CLI, execution wrapper, and daemon. Each state
directory has one daemon shared across repositories, with a private Unix
socket and a versioned JSON protocol; the ordinary installation is per user.
The daemon owns persistent state in SQLite and coordinates allocation,
lifecycle transitions, and recovery.

The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups, registers with the daemon, and handles stop requests.
The daemon never proxies terminals, and a lost connection is not proof that an
execution stopped or that its resources are free.

Use short transactions for atomic claims and typed lifecycle states with
lowercase persisted and wire spellings; unknown values are errors. Keep slow
external operations outside transactions while retaining ownership through
failure. Migrations preserve ownership records; setup automatically restarts an
incompatible installed daemon, verifying it stopped before replacing its service.

Worktrunk creates and removes worktrees through Shoal's adapter with isolated
configuration and hooks disabled. Invoke external tools with argument arrays.
Keep writable state outside the binary's directory. Service setup manages one
per-user launchd/systemd service; foreground mode covers environments without
a service manager. The service captures the installing shell's `PATH`.

Distribution is a source-built Homebrew formula in the shared HN05 tap with a
release channel (immutable tags, selected explicitly) and a `main` channel.
Build and packaging logic lives in Shoal's `scripts/install-homebrew.sh`; the
tap declares sources and dependencies. Releases are made by one Forgejo Actions
workflow that bumps versions, validates, creates and merges a version PR under
normal branch protection, tags the exact merged commit, publishes the release,
and then updates both taps (Forgejo-sourced, and GitHub-sourced with a push
mirror) using that tag. It never tags a later commit, downgrades, or force
merges; tap updates retry against the tap's latest main without force pushes.

## Workspaces and Git

Register local repositories in place or retain URL clones for reuse under
`~/.local/share/shoal/repositories/<name>` (global `repositories_dir` or a
one-off `--path` override; directories are reserved atomically and never
reused or moved). Registration is idempotent by normalized origin URL, then
canonical path, never fetches, and keeps a stable UUID separate from the
display name. Workspaces stay under the state directory.

A workspace branches from the repository default branch: `origin/HEAD`, the
sole remote's HEAD, or the checkout's current branch without remotes, never a
guessed `main`. The selected local default branch is fast-forwarded from its
upstream first, preserving an ahead branch and refusing divergence, dirty or
managed checkouts, and failed fetches. `--ref` starts elsewhere without
refreshing, except when it names the default branch. Creation, default-branch
refresh, setup, repository removal, and recovery share a per-repository Git gate.

Creation accepts literal Git branch names and derives a portable, globally
unique workspace name and directory from them; a normalization collision fails
without touching existing work. Branch conflicts get numeric suffixes on the
blocking component only, never changing the workspace name. Worktree Git
metadata identity is recorded so moved or replaced directories are never
adopted silently.

Repository config may name `setup_cmd`, `post_setup_cmd`, and `pre_remove_cmd`:
single executable paths resolved against the worktree, run directly without
shell parsing or PATH lookup, with the worktree as working directory. Setup
runs through the tracked wrapper with workspace scope; the daemon owns
readiness and execution records, and `add` keeps the workspace preparing until
setup exits cleanly with no survivors. Failures preserve files, branches, and
leases; interactive callers choose delete, ignore (which repairs verified state
first), or keep, while JSON callers get a nonzero exit. `prepare` reruns setup
explicitly; nothing retries automatically.

Hooks are deliberately untracked user processes with the workspace identity
but no scope token, because their purpose is to start or stop things that
outlive the hook (a tmux session, say) without becoming execution survivors.
The post-setup hook runs from the CLI with the terminal once the workspace is
ready and before any agent; failure keeps the ready workspace. The pre-remove
hook runs in the daemon inside the single removal path for manual, repository,
and automatic removal, after checks pass and commands stop, bounded in time;
failure retains the workspace. Both hook keys share the setup path rules.

`diff` compares against the recorded base's fork point (merge-base fallback,
fixed commits stay fixed) with native Git settings, so advancing the base is
never shown as work. `pull` fast-forwards the repository default branch only.
`merge` imports any local or remote branch into the workspace's own branch,
preferring local sources, which it first fast-forwards from their upstream
unless `--local`, they lack an upstream, or a managed workspace has them
checked out; remote discovery must be unambiguous, and conflicts are left for
ordinary Git. Fetches use private temporary refs, merges
run through the tracked wrapper, and own-branch checks are cooperative.

## Scope and user interfaces

Commands launched through Shoal inherit a daemon-validated scope token that
confines them to their own workspace: inspect, execute, merge, and resources.
`pull`, creation, removal, reconciliation, other workspaces, repository
administration, and service control need an unscoped caller. Nested executions keep their scope.

The CLI takes explicit targets and `--json` for automation, and uses
current-directory resolution and fzf interactively; noninteractive calls never
open a picker. Rust chooses navigation paths; the Bash/Zsh wrapper only changes
directory and never evaluates repository-provided code. Confirmations show the
action and ask `[y/N]`, cancel on Enter, `n`, end of input, or Ctrl-C (also
after a tracked execution in the same process), and are bypassed only by
explicit flags such as `--yes`. Removal's branch choice stays separate from
its confirmation; explicit branch flags skip the choice only.

Completion queries the installed binary per Tab and uses read-only daemon calls
with a 500 ms timeout for live targets, honoring state directory and scope and
never starting a daemon or picker. Targets sort before flags, including in
fzf-tab.

Agent shortcuts use the execution wrapper: Codex CLI gets full access without
approvals, Claude gets remote control named after the workspace and a persisted
trust entry in its config (Claude offers no flag for this; its own error text
names that entry). Codex's default mode is a global config value read at launch.
`add --issue` resolves issue numbers/URLs using the remote and existing gh/fj
login, derives a portable name, and supplies issue context to CLI agents; forge
lookup stays in the CLI with no Shoal credentials or forge configuration.
`add --agent` launches only after creation, setup, and the post-setup hook
succeed, or after an explicitly ignored setup failure, and retains the
workspace whatever the agent does. Desktop handoffs (Codex app, T3) provide no
tracking or scope; users disable automatic cleanup when that activity cannot
be tracked.

The skill is installed at user scope for Codex and Claude, independent of the
daemon and never from a scoped execution. Homebrew builds link to the packaged
skill; other builds copy it. Skill availability registers nothing.

## Resource ownership

Reservations and leases belong to a worktree, survive command exit, restarts,
and failed removal, are allocated atomically with idempotent lease names, and
are released explicitly or by successful removal. Resources are lazy, never
claimed at creation.

Global TOML is machine policy; repository TOML comes from the worktree
(`.shoal.toml` or `.shoal/config.toml`, both together is an error) or from a
local override stored in the database by repository ID, which replaces the
whole worktree config and is deleted with the registration. Config is read per
request, so changes need no restart and leave existing leases alone.
Repository config cannot expand machine policy; global pools span
repositories, repository pools span that repository's worktrees.

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
executions, run the pre-remove hook, remove owned simulators, remove the
worktree, and release leases with the record. Failures retain what is needed
to retry. Manual removal deletes a redundant branch (tree equal to the local
default or its upstream) and otherwise requires an explicit keep or delete
choice. Automatic cleanup removes only clean, fully pushed, idle worktrees with
no executions, directory users, leases, or permits, rechecked immediately
before deletion, without fetching. `repo rm` deletes the checkout and every
workspace through that path, refuses external worktrees and dangerous paths,
persists progress, and blocks new workspaces until an interrupted removal is
retried.

Reconciliation reports by default; repair restores verified worktrees and
clears executions proven stopped while preserving work and leases. Startup
audits but never deletes, kills, clears unknown executions, or releases leases.
Survivors are signaled only after verifying PID birth identity and same-user
ownership; acknowledgement cannot override visible live processes. Moved or
replaced worktrees stay unresolved until restored; a missing directory is
cleaned up only by explicit removal, retaining the branch.

## Remaining work

Implementation order: CLI/daemon, workspaces, ports, simulators, lifecycle
polish, then filesystem restrictions. Open items:

- **Agent launchers:** move agent-specific flags and setup into configurable
  defaults so `claude`, `codex`, and `t3` stop being special cases.
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
