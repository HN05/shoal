# Shoal design

Current product decisions and unresolved work. Command usage and configuration
examples belong in [README.md](README.md); contributor rules belong in
[AGENTS.md](AGENTS.md). Update decisions in place rather than appending milestone
reports, test inventories, or investigation transcripts.

## Purpose and boundaries

Shoal manages local Git workspaces, executions, and shared development resources
for humans and coding agents. Cleanup is part of resource ownership: a disposable
workspace should not leave substantial run-owned data behind.

Agents cooperate with Shoal's scope and allocations. Shoal does not prevent a
hostile same-user process from bypassing them through Git, filesystem operations,
or direct resource access. Filesystem restrictions are future work.

Keep caller-specific integration outside the core. Superlogical owns its terminal
sessions and can request workspace preparation, execution, and removal. Macraft
owns VM/container provisioning and guest lifetime. Shoal does not provision build
tools, manage browsers, schedule agent tasks, or store agent conversations.

## Architecture

One Rust binary provides the CLI, execution wrapper, and daemon. Each state
directory has one daemon, shared across repositories, with a private Unix socket
and versioned JSON protocol. The ordinary installation is per user; isolated
state directories have independent allocations. The daemon owns persistent state
in SQLite and coordinates allocation, lifecycle transitions, and recovery.

The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups. It registers with the daemon and handles stop requests.
The daemon does not proxy terminal sessions. A lost connection is not proof that
an execution stopped or that its resources are available for reassignment.

Use short transactions for atomic claims and typed lifecycle states. Keep slow
external operations outside database transactions, retaining ownership through
failure. Persisted and wire states use lowercase representations; unknown values
are errors. Migrations preserve ownership records, and protocol mismatches require
a compatible CLI and daemon.

Worktrunk creates and removes worktrees through Shoal's adapter, with isolated
configuration and hooks disabled. Invoke external tools with argument arrays.
Keep writable state outside the installed binary's directory so upgrades preserve
it. Service setup manages one per-user launchd/systemd service; foreground mode
supports environments without a service manager.

## Workspaces and Git

Register local repositories in place or retain URL clones for reuse. Registration
is idempotent by normalized origin URL, falling back to canonical local path.
Repository identity remains distinct from its optional display name. Registration
does not implicitly fetch. Workspace creation does not restore dependencies.

A workspace defaults to local `main`, fast-forwarded from its configured upstream
before branching, even when the registered checkout is on another branch. An
already-ahead main is preserved. Refresh failures (including missing upstream in
a repository with remotes, divergence, or a dirty/managed main checkout) stop
creation. Repositories with neither remotes nor a main upstream use local main.
An explicit `--ref` selects another starting point without refreshing main;
`--ref main` and `--ref refs/heads/main` still refresh it. Refresh and creation share
the repository Git gate. Registration alone does not refresh main.

A workspace starts from committed history and has a stable name and directory.
Its new branch uses that name, with numeric suffixes only on conflict. Serialize
branch selection and creation per repository; existing refs and ownership records
reserve branch names. Record the worktree's Git metadata identity so moved or
replaced directories cannot be silently adopted.

Git operations have separate meanings:

- `diff` compares against the recorded base branch's fork point, falling back to
  merge-base. A fixed-commit base stays fixed. Preserve native Git diff settings;
  advancing main alone must not appear as work done on the feature branch.
- `pull` fast-forwards repository main from its configured upstream. It preserves
  an already-ahead main and refuses divergence, dirty main checkouts, and main
  checked out in a managed workspace. It does not merge into the feature branch.
- `merge` imports any local or remote branch into the workspace's recorded branch.
  Local sources take precedence; remote-only discovery requires an unambiguous
  match, and explicit remote sources always fetch fresh data. Conflicts stay in
  the worktree for ordinary Git resolution or abort. No automatic stash, reset,
  push, or change to another workspace is implied.

Fetches for these operations use private temporary refs rather than shared
FETCH_HEAD. Merge runs through the tracked execution wrapper. Own-branch and
worktree checks are cooperative safeguards; independent Git commands can race.

## Scope and user interfaces

Commands launched through Shoal inherit a daemon-validated scope token. They may
inspect and execute in their own workspace, merge into its branch, and manage its
resources. `pull` for their repository's main is the narrow repository-management
exception. Creation, removal, reconciliation, other-workspace access, and service
administration belong to an unscoped caller. Nested executions retain scope;
changing working directory does not expand it.

The CLI supports explicit targets and JSON for automation, with current-directory
resolution and fzf for interactive selection. Noninteractive calls never open a
picker. Rust selects navigation paths; the Bash/Zsh wrapper only changes directory
and preserves status, without evaluating repository-provided shell code.

Agent shortcuts use the execution wrapper. Codex CLI launches with
`--sandbox danger-full-access --ask-for-approval=never`; Claude receives the
workspace name for remote control. Generic `exec` forwards its command unchanged.
Agent permission settings and Shoal's cooperative daemon scope are separate.

Desktop shortcuts hand an existing workspace directory to Codex or T3. A GUI
launcher returning does not mean its agent session ended. Directory handoff alone
provides neither execution tracking nor a scope token; disable automatic cleanup
when independent GUI activity cannot be tracked reliably.

The agent skill is distributed at user scope, so it works without a copy in each
project or a Shoal-launched agent. `skill` exports the bundled instructions;
`skill install` installs or refreshes them for Codex and/or Claude. Installation
is independent of the daemon and cannot run from a scoped execution. Skill
availability does not register a checkout or establish execution ownership.

## Resource ownership

All reservations and leases belong to a worktree, not to the requesting command.
They survive command exit, daemon restart, and failed removal. Allocation must be
atomic, repeated lease names idempotent, and release explicit or part of successful
workspace removal. Agents stop using a resource before releasing it.

Global TOML supplies machine policy. Repository TOML is read from the selected
worktree at `.shoal.toml` or `.shoal/config.toml`; both together are an error.
Repository preferences cannot expand machine policy. Global resource pools span
repositories within one daemon; repo pools span that repository's worktrees.
Resources are acquired lazily, not during workspace creation.

### Ports

TCP reservations are cooperative: probe availability, then record a unique port,
lease name, and environment mapping. Shoal does not retain a listening socket or
prevent unrelated processes from binding later. CLI overrides and explicit
conflict policy control allocation; a suggestion is not a reservation. Export
allocations to subsequent executions without claiming to update existing shells.

### Simulators

Simulator leases are exclusive. Use installed runtimes and mutate only devices
recorded as Shoal-owned. Persist claims before simctl mutations and retain them
after interrupted or failed operations. Active leases are never preempted;
unallocated devices may be reclaimed to satisfy capacity limits or idle expiry.
External booted devices count toward capacity but must not be mutated.

Normal handoff preserves apps, data, and settings. A fresh or erased device needs
an explicit `--clean --reason` request; release an existing lease before changing
it to clean. Minimize erased apps when choosing a device and persist the audit
record before destructive work. Failure to write the audit prevents mutation.
Audit history survives workspace removal.

Delete owned devices during workspace removal or idle expiry. If deletion fails,
retain ownership for retry; completed cleanup steps are not rolled back merely
because a later step fails. Installed runtimes and OS caches remain machine-owned.

### Generic permits

A semaphore lease consumes capacity in both its named pool and selected member.
A standalone resource is a one-member pool. An rwlock member allows unlimited
readers or one exclusive writer; all readers of that member share one pool slot,
released by its final reader. New rwlock leases default to write. Changing lease
mode or member requires release first; there is no atomic upgrade.

Check definitions and both capacity limits in the same allocation transaction.
Definition drift blocks new claims until definitions agree or existing leases
are drained; it does not revoke existing permits or prevent their release.
Bounded waiting provides no fairness, deadlock avoidance, or multi-resource
transaction guarantee. Shoal tracks permits without managing the underlying
resource's lifecycle or enforcing its use.

## Removal and recovery

Manual and automatic cleanup share one removal path: establish ownership, stop
owned executions, clean up owned simulator devices, remove the worktree, and
release bookkeeping leases with the ownership record. Failures retain the records
needed to retry. Access to a shared cache never makes it disposable workspace data.

Manual removal deletes a redundant branch when the worktree is clean and its
contents match main or its upstream. Otherwise the caller explicitly chooses to
keep or delete the branch. Keeping a branch preserves committed work only;
removing a worktree discards its uncommitted files.

Automatic cleanup is enabled by default after ten idle minutes. Eligibility
requires a clean worktree whose HEAD is fully pushed according to locally known
remote refs, no live or unknown executions or processes using the directory, and
no active simulator leases or generic permits. Activity resets eligibility;
failed inspection blocks removal. Recheck immediately before deleting. No
implicit fetch or assumption that a detached GUI session ended is permitted.

Reconciliation reports by default. Explicit repair preserves work and resource
leases while restoring verified worktrees or clearing executions proven stopped.
Startup audits ownership but never deletes work, kills processes, clears unknown
executions, or releases leases. Interrupted lifecycle operations remain failed;
disconnected executions remain unknown until reconciled.

Verify native PID birth identity and same-user ownership before signaling
survivors. Process-group membership or a reused PID alone is insufficient.
Explicit acknowledgement of stopped legacy executions cannot bypass visible live
processes. Process discovery is conservative, but is not complete containment of
all detached descendants.

Refuse cleanup of moved or replaced worktrees until ownership is resolved. For a
confirmed missing directory, explicit removal cleans up its resources and stale
Worktrunk registration while retaining the Git branch. Automatic cleanup never
uses a failed or unverifiable workspace as permission to discard state.

## Remaining work

Preserve the implementation order: CLI/daemon, workspaces, ports, simulators,
lifecycle polish, then filesystem restrictions. The core resource and recovery
workflow exists; the following work remains distinct from current behavior:

- **Workspace setup:** optional repository-owned dependency restoration is a
  confirmed direction. Define command syntax, readiness, retries, and failure
  cleanup before implementation. Do not infer install commands from manifests.
- **External sessions:** define a generic attachment/hold and recovery contract
  before promising lifecycle tracking or automatic cleanup for GUI agents.
  App-specific hooks and launch adapters belong outside the core.
- **Storage policy:** define ownership and retention for run data outside the
  worktree, shared caches, logs, and retained artifacts. Numeric pruning limits
  and retained audit-history policy remain open.
- **Distribution and portability:** Homebrew installation is a confirmed future
  requirement. Preserve stable service identity across upgrades. Native Linux
  service and recovery validation remains outstanding.
- **Execution environments:** host/guest and cross-user resource coordination are
  unresolved. Independent state directories currently have independent capacity.

### Filesystem restrictions: confirmed policy, not implemented

Provide practical guardrails for cooperative agents and an explicit unrestricted
mode. Read and write access are separate. Precedence is global deny, global
allow, repository grant, then deny undeclared access. Global denies win even
inside broadly allowed directories. There is no implicit whole-home grant.

Define an inspectable baseline for the worktree, temporary storage, runtime
dependencies, shared Git metadata, and required tool state. Resolve symlinks and
distinguish writable access from cleanup ownership. Exact path syntax and launch
profiles remain open. Unsupported policies must fail clearly rather than silently
launching unrestricted or retrying a partially completed command.

Seatbelt is the intended macOS backend. Landlock is a Linux candidate, not a
selection; it must first demonstrate the required nested allow/deny semantics.
Reconsider bubblewrap if those semantics or a separate filesystem view require
it. Keep restrictions in the execution launcher, outside the resource daemon,
and validate real builds, tools, and cleanup before committing to a backend.
