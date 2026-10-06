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

Caller-specific integration stays outside the daemon core: Superlogical owns terminals;
Macraft owns VM/container provisioning. Shoal does not provision tools, manage
browsers, schedule agent tasks, or store conversations. Agent shortcuts
(`claude`, `codex`, `happy`, `t3`) are thin launchers around the generic `exec`
path; CLI launcher argument arrays are configurable defaults; session and desktop
adapters retain their tool-specific setup. Opt-in forge authentication wrappers
are user-owned executables selected per tool through repository/global config;
tracked agents receive them on a private PATH inherited by their descendants.
Wrappers own credentials and tool-specific environment changes; Shoal stores no
tokens and does not switch the user's or daemon's login. Desktop handoffs have
no authentication override because they may reuse an existing process.

## Architecture

One Rust binary provides the CLI, execution wrapper, and daemon. Each state
directory has one daemon shared across repositories, with a private Unix
socket and a versioned JSON protocol; the ordinary installation is per user.
The minimum Rust version follows current stable so the code can use its newest
features; the manifest and CI toolchain advance together, with locked dependencies.
Resource protocol methods use domain-then-verb names matching the CLI operations.
The daemon owns SQLite state, allocation, lifecycle transitions, and recovery.
SQLite runs on one dedicated blocking thread with one reusable connection and a
128-operation queue; callers wait for queue space. Admitted operations run once
even if their caller disconnects, and shutdown drains them before releasing daemon
ownership. Connections retain foreign keys and a five-second busy timeout; a panic
or unfinished transaction discards the connection before the next operation.
Daemon errors retain typed codes and messages on the client without changing the
wire format; unfamiliar codes are preserved verbatim for version compatibility.

Internal CLI workers are built through one typed argument builder that explicitly
passes the resolved state directory and output mode.
The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups. It retains buffered daemon controls across start, stop,
and recovery transitions so adjacent frames cannot be lost. After commands and
interactive hooks it restores the caller's foreground group and OS terminal settings. When `TERM` is nonempty and not `dumb`,
it also resets emulator input modes for the shell, writing best-effort cleanup directly
to the terminal even when output is redirected. It registers through one request carrying its execution kind,
and gives the entire command process group a shared grace period on stop requests,
even after its leader exits. The daemon prepares each kind before shared registration.
The daemon never proxies terminals, and a lost connection is not proof that an
execution stopped or that its resources are free.

Overload protection is configured machine-wide: memory is enabled by default,
CPU is opt-in and requires sustained aggregate busy time. Stop connected tracked
agents through their execution wrappers, preserving work and leases. Agent
session recovery is automatic when explicitly configured and load has recovered;
never replay the original task prompt. Keep waiting wrappers tracked, serialize
restores, and preserve manual recovery across restarts. Saved recovery represents
unfinished work until restored or explicitly discarded.
Agent metadata is transient, so restart cannot select disconnected survivors.
Manual pause stops connected tracked agents through their wrappers and saves
session recovery for explicit resume, preserving work and leases. It may select
one execution; ordinary commands continue and scoped callers cannot pause agents.

Use short transactions for atomic claims. Closed Shoal enums with matching display and
wire names use one macro to share explicit spellings across conversions and reject
unknown values. Native simulator states use a
separate adapter enum that preserves unfamiliar strings and native wire spellings;
only confirmed shutdown frees running capacity. Slow operations stay outside
transactions; failures and migrations preserve ownership. `install` preserves compatible daemons and
commands, deferring service changes until restart; incompatible daemons are
verified stopped before replacing their service.

Worktrunk creates and removes worktrees through Shoal's adapter with isolated
configuration and hooks disabled. Typed adapter outcomes preserve unfamiliar strings;
only confirmed branch deletion sets the response deletion flag. Invoke external
tools with argument arrays. Captured subprocesses share optional deadlines and a
single diagnostic limit, terminate on cancellation, and drain output while sending
input. Keep writable state outside the binary's directory.
Service setup manages one per-user launchd/systemd service; foreground mode covers
environments without a service manager. The service captures the installing shell's
`PATH`.

Advisory repository reviews default to Codex, retaining configured agent overrides
and automatic fallback. Repository code audits rotate focused read-only inspections
and maintain one recurring report issue with triage in comments. Failed or
incomplete attempts retain the last successful findings; audit results are
advisory and do not gate merges.

Distribution is a Homebrew formula in the shared HN05 tap: releases install
checksummed prebuilt Linux and macOS binaries on x86_64 and arm64; `--HEAD`
builds `main` from source.
Named configuration templates in `configs/` are embedded at build time, so
installation needs no source checkout or network access.
Shoal owns the generated formula and source-install script; Rust is required
only for `--HEAD`. One Forgejo Actions workflow releases: it
bumps versions, validates, creates and merges a version PR under normal branch
protection, tags the exact merged commit, publishes merged-PR notes excluding release
preparation since the nearest earlier published ancestor release with issue links, and from
that tag attaches cross-built binaries and recreates the release on GitHub with
change descriptions and a GitHub comparison link, omitting Forgejo PR and issue
references that Git mirrors do not carry, then updates both taps from their
respective host's published checksums. Release notes link to the runtime
dependencies in the tagged README on each host. Reruns synchronize those notes while
preserving complete asset sets. It never tags a later commit, downgrades, or force merges;
explicitly selecting the current tagged version resumes from that tag, and tap
updates retry against the tap's latest main without force pushes. Deleted release
branch labels are accepted only for matching repository and exact PR identity.

## Workspaces and Git

Every repository owns `~/shoal/<name>/` (global `root_dir`): its workspaces
default to it and a URL clone lives there as `.checkout`, a name no
workspace can take, so worktrees are grouped per repository, never nested in a
checkout, and outside the state directory; a root inside either is refused.
Repository roots and explicit workspace paths resolve existing symlinks and lexical
`..` before boundary checks and creation, even when trailing components are missing.
An explicit workspace path overrides the destination for one creation, without
changing the repository directory; reject existing paths and overlaps with state,
checkouts, workspaces, or other repositories' directories. Cleanup owns the worktree,
never its parent. Directories are reserved atomically and never reused or moved; an
in-place checkout placed as `~/shoal/<name>/<x>` adopts that directory, and
`--path` clones elsewhere. Registration is idempotent by normalized origin URL,
then canonical path, never fetches, and keeps a stable UUID separate from the
display name. `repo rm` deletes the directory only once it is empty.

A new branch starts from the repository default branch: `origin/HEAD`, the sole
remote's HEAD, or the checkout's current branch without remotes, never a guessed
`main`. The selected local default branch is fast-forwarded from its upstream first,
preserving an ahead branch and refusing divergence, dirty or managed checkouts, and
failed fetches. `--base REF` starts from any locally resolvable
commit ref without refreshing, except when it names the local default branch.
Creation, default-branch refresh, setup, repository removal, and recovery share
a per-repository Git gate. Daemon ref updates disable hooks, Git credential prompts,
and SSH askpass while preserving the user's SSH transport configuration;
interactive Git commands retain normal prompting. Ancestry and exact-ref checks
distinguish negative answers from command failures; failed checks stop branch
selection, refresh, and removal with the Git diagnostic.

New-branch creation accepts literal Git branch names and derives a portable,
globally unique workspace name and directory from them; a normalization collision
fails without
touching existing work. Branch conflicts get numeric suffixes on the blocking
component only, never changing the workspace name. Names reserved by the Worktrunk
adapter across supported Git object formats get a leaf suffix on creation and
an incompatibility error on existing-branch selection. Ownership of a worktree's
Git admin directory is proven by a marker naming the workspace, written there on
creation, adoption and verified repair: Git keeps it across moves and drops it
when the worktree is re-created, so device renumbering, restores and copies keep
ownership while replacement does not. A byte-for-byte copy of the admin directory
is therefore accepted as the same worktree. The inode and birth time (statx on
Linux independently of libc; device/inode without birth time) are still recorded
and prove only unmarked records, which gain a marker during startup audit or
explicit repair once that identity matches; an already changed device cannot be
verified from an inode alone. Records with neither proof require explicit reclaim.
Moved or replaced directories are never adopted silently.
Existing-branch selection creates a worktree without suffixing; remote heads are
discovered live and become local tracking branches. Local selection preserves
commits; remote selection fast-forwards matching tracking branches. Ready owned
workspaces reopen without setup, hooks, or refresh; other checkouts block creation.
Existing worktrees use an explicit base ref, otherwise the local default, as their
diff base, or the opening commit if unavailable or on that same branch. Explicit
adoption accepts an unlocked linked worktree root on a local branch of the registered repository with no ownership
conflict, preserving dirty files and Git settings and recording it ready without
setup or hooks. It takes normal cleanup ownership. Reopening verifies its record;
adoption cannot repair a moved or replaced managed worktree. Never adopt a main checkout.

Named Git profiles live in global config; repository or global `git_profile`
selects one for newly created worktrees, overridden by `add --git-profile`.
Apply it before setup using Git's
per-worktree config, preserving other worktrees' settings. Enable the shared
`worktreeConfig` extension only when the existing repository layout supports it;
profiles cannot change layout or extensions. Reopening keeps existing settings
and rejects an explicit profile flag.

Repository config may name setup and lifecycle hook executables:
single executable paths run directly without shell parsing or PATH lookup.
Hooks that run with a worktree resolve paths against it and use it as their
working directory. Setup runs through
the tracked wrapper with workspace scope; the daemon owns readiness and execution
records, and `add` keeps the workspace preparing until setup exits cleanly with no
survivors. Failures preserve files, branches, and leases; interactive callers choose
delete, ignore (which repairs verified state first), or keep, while JSON callers get
a nonzero exit. `setup` reruns setup explicitly; nothing retries automatically.

Hooks are deliberately untracked user processes with the workspace identity
but no scope token, because their purpose is to start or stop things that
outlive the hook (a tmux session, say) without becoming execution survivors.
They use global defaults below repository config.
The pre-setup hook runs in the daemon after ownership and execution checks,
with a time limit, while preparation excludes lifecycle and permit changes;
failure marks setup failed. It may be configured without a setup command.
The post-setup hook runs from the CLI with the terminal once the workspace is
ready and before any agent; failure keeps the ready workspace. The pre-remove
hook runs in the daemon inside the single removal path for manual, repository,
and automatic removal, after checks pass and commands stop, bounded in time;
failure retains the workspace. Post-remove runs after ownership is released,
from the repository checkout with its copy of the executable, retaining the old
workspace path in the environment. Its command is selected before removal.
It is best-effort: failure is a warning and notification, never a failed removal
or restored ownership. Missing-worktree
cleanup skips hooks; post-remove events are not durably queued or replayed.

`diff` compares against the recorded base's fork point (merge-base fallback, fixed
commits stay fixed) with native Git settings, so advancing the base is never shown
as work. `merge` imports any local or remote branch into the workspace's own branch,
preferring local sources, which it first fast-forwards from their upstream while
preserving ahead branches and refusing dirty or diverged checkouts and failed fetches,
unless `--local`, they lack an upstream, or a managed workspace has them checked out;
implicit remote discovery requires a missing local ref and must be unambiguous; failed
local lookups stop the merge, and conflicts are left for ordinary Git. `land`,
the local substitute for a pull request, merges the workspace branch into the default
branch, first refreshing a configured upstream while preserving an ahead branch and
refusing divergence or fetch failure, without pushing. The default branch cannot be
held by a managed workspace, and any checkout of it must be clean. Land aborts a merge
that does not apply cleanly, leaving conflicts to a `merge` of the default branch into
the workspace. Landing holds the repository Git gate for the tracked execution; the
daemon validates and refreshes, and the CLI worker merges.
After cancellation, the wrapper rolls back incomplete merges after stopping the
process group, preserving completed merges and reporting unsafe recovery failures.
Fetches use private temporary refs; merges use the tracked wrapper and cooperative
own-branch checks.

## Scope and user interfaces

Commands launched through Shoal inherit a daemon-validated scope token that
confines them to their own workspace: status, inspect, execute, setup, merge, and resources.
`land`, creation, removal, reconciliation, other workspaces, repository
administration, and service control need an unscoped caller. PR registration,
manual merge acknowledgement, and assignment continuation/completion are own-workspace exceptions.
Effective configuration may be read for the caller's own workspace; changing it
needs an unscoped caller.
Nested executions keep scope.

Root help groups built-in commands by task; configured commands are discovered
through `run`.

The CLI owns Herdr tab handoffs after issue lookup and interactive workspace and
agent choices and labels the tab `<repo>#<number>` for an issue, otherwise with the
allocated workspace branch by default. An optional `herdr.tab_name` template
follows normal configuration precedence, substitutes repository, branch and issue
fields once, and updates the branch after allocation. The resolved template travels
with the launch plan so the worker preserves the caller's naming choice.
The plan travels in the tab's environment, naming a found issue by URL rather
than carrying its unbounded body, and preserving
state/config selection and literal arguments without hook scripts or state files.
By default, workspace shells receive focus and agent launches stay in the
background; an explicit focus setting overrides this choice.
The worker drops the plan before starting children; a retained tab's shell keeps
it, inert, while a non-default state directory is passed only to the worker.
Tracked agent completion closes its tab, including cleanup stops; preparation
failures and shell or untracked desktop handoffs retain it.

Single-workspace actions select an explicit target, otherwise the caller's scoped
workspace or the workspace containing the current directory, then an interactive
picker; a deleted current directory provides no workspace context. Explicit misses
fail without fallback; noninteractive and JSON calls
without a current workspace require a target. Scope remains daemon-enforced.
Bare `cd` always picks; all-workspace operations retain their scope.
`config show` never opens a picker. Agent pickers list only agents whose
executables are installed and offer “No agent” to continue without launching
one. `config show` reports effective repository values with their winning layers and uses a registered checkout
before a workspace exists. `status` combines lifecycle, fork-point changes, active work and
leases in one workspace view. Human output uses `Display` for enum values and a shared
semantic palette at the CLI presentation layer; machine output and stored values
stay unstyled. Progress during silent waits belongs to the CLI and shows transient elapsed-time feedback on terminal stderr,
suppressed for JSON and dumb terminals. Rust chooses paths, including
`<root_dir>/<repo>` after removal; the Bash/Zsh wrapper changes directory
without evaluating repository code. If cleanup removes the current directory or
a pending navigation destination, shell integration recovers to its nearest surviving
ancestor after the command or at the next prompt, preserving the command status.
Recovery needs no daemon and is disabled for scoped callers.
Confirmations show the action and ask `[y/N]`, cancel on Enter, `n`, EOF or
Ctrl-C (also after a tracked execution), and are bypassed only by
explicit flags such as `-y`/`--yes`. Removal's branch choice stays separate from
its confirmation, which lists uncommitted changes and untracked files with Git
status codes and reports omitted entries when the preview reaches its size limit;
explicit branch flags skip the choice only. A failure whose fix is one
command asks interactive callers `[y/N]` whether to run it and continue;
declining, and noninteractive or JSON calls, fail naming that command. Interactive
navigation without the shell wrapper reports how to load it without changing
redirected or JSON output.

Completion queries the installed binary per Tab and uses read-only daemon calls
with a 500 ms timeout for live targets, honoring state directory and scope and
never starting a daemon or picker. Targets sort before flags, including in fzf-tab.

Named `[commands]` are executable/argument arrays resolved per name from saved
repository config, worktree config, then global config and built-in defaults at
launch. They run through the tracked wrapper with workspace scope, terminal I/O,
and literal extra arguments. `run` lists the resolved arrays with their layers and
provides an explicit spelling for names that collide with built-ins; built-ins win
the bare shorthand. Repository-only commands require a current or explicit
workspace. Unknown names report a command error with suggestions for similar
built-ins and never open a picker. Workspace fields expand once within individual
arguments; a standalone `{args}` places the caller's literal arguments.
`{diff_base}` lazily uses `diff`'s daemon lookup. Built-in `review` chooses
between the `review` command and an agent prompted to report, not change, the
work. `pr review` resolves a same-repository PR with the user's forge login and
reviews its head branch against the PR base, reusing the owning workspace. Review tools own review storage, exports, and forge authentication, with
explicit feedback handoff to agents.

Custom `--agent` and `default_agent` names select named commands through the
same configuration layers. They run as tracked agents with scope, forge wrappers,
and exit notifications. Their `{prompt}` argument combines general instructions
and issue context; without it, nonempty context precedes forwarded arguments.
Plain command invocations expand `{prompt}` to an empty string. User arguments
and inserted prompt text remain literal. Built-in names retain
their specialized launchers; custom names add no tool-specific flags or trust setup.

Agent shortcuts use the execution wrapper: by default Codex CLI gets full access without
approvals and Claude runs with its own settings. Both trust the
workspace, its repository's Shoal directory, and the registered checkout in their
user config before launch, creating the file if needed; Happy
launches and Codex app handoffs do so too. General agent templates become native
CLI instructions or a first-message prefix for Happy Codex; desktop handoffs carry no instructions.
Codex's default mode is a config value read at launch; `--cli` and `--app`
override it.

Issue-based workspace opening requires an open issue and persists its canonical
URL before tracked setup or agent launch. An existing local branch with the
derived name is reopened as an existing branch rather than suffixed, and
conflicts that prevent reopening it fail before the agent picker. Associations are idempotent and cannot
be replaced. Status and inspection expose them.

`add --issue` resolves issue numbers/URLs using the remote and existing gh/fj
login, derives a portable name, and renders issue context from a plain-text
template for CLI agents; initial lookup runs in the CLI with no Shoal credentials
or forge configuration. An issue URL may select the single registered repository
by remote identity when `add` omits it; an unregistered remote offers to register
the repository URL. `issue <number-or-url>`
invokes that same path with the configured `default_agent` standing in for
`--agent`. Numbers use an explicit `--repo`, the current registered checkout or
managed workspace, or an interactive repository picker; URLs keep remote matching.
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
terminal's own notifications (OSC 9); `ls`, workspace `status`, and `daemon status` only count them. Recording never fails the operation it describes,
repeated polled conflicts collapse until read, and scoped processes cannot read
them. `post_agent_exit_cmd` exposes tracked-agent exit notifications to user
integrations while the workspace is ready, using normal configuration precedence
and daemon hook rules with the agent name, reported code, and process-completion
status. Failure notifies without changing the exit result or marking completion;
removal uses its own hooks. Desktop or push delivery was considered and not adopted.

The skill is installed at user scope, independent of the daemon and never from
a scoped execution; its availability registers nothing. Global `[ai.<name>]`
settings name skill directories for user-configured tools, with Codex and Claude
defaults. Skill directories describe the machine and cannot be set per repository;
custom launchers use named commands without tool-specific integrations.
Packaged skills resolve the runtime `SHOAL_SKILL_PATH`, build-time path, then
an adjacent `shoal-skill` symlink to an existing file. Explicit paths must be
absolute; relative link targets resolve lexically against the link's directory.
Preserve stable installation prefixes so upgrades apply; unpackaged binaries
install the embedded copy. Homebrew launches the binary directly, without a shell.

## Resource ownership

Reservations and leases belong to a worktree, survive command exit, restarts,
and failed removal, are allocated atomically with idempotent lease names, and
are released explicitly or by successful removal. Resources are lazy, never
claimed at creation.

Global TOML is machine policy, seeded by `install` with the stated defaults when
absent; explicit key edits preserve unrelated settings and comments and validate
once per command before saving, so dependent keys change together, while reset or named template installation replaces the file.
Global writes keep the previous file as a backup; the daemon reads it at startup
and on reload, which global CLI writes request of a running daemon, replacing its
snapshot only with a valid file so agents and leases are untouched; an operation
that combines global definitions with resolved settings reads both from one snapshot. The CLI reads agent settings per command. Repository TOML comes from the
worktree (`.shoal.toml` or `.shoal/config.toml`, both together is an error) with
a local override stored in the database by repository ID layered over it per
option, a named table replacing the one below it whole, and deleted with the
registration. Inline repository key edits use the same validation as imports and
serialize read/modify/write in the daemon; unset restores lower-layer values. Every option
that does not describe the machine may be set at either level and resolves per
option: saved config, worktree file, global config, then the built-in default.
One option table defines both the merge and its provenance, so a reported layer
is the one whose value is in use; the daemon resolves a workspace's settings
once per request and every use site reads them rather than layering on its own.
Prompt templates select repository additions by saved/worktree precedence and
append them to global guidance, using the bundled issue template when no global
issue template is configured. Empty repository values suppress only the addition.
Repository-root Markdown files and global files beside `config.toml` sit below
inline TOML values at each level; provenance names the highest configured layer
and reports the combined text.
`install` adds missing templates from the bundled repository-root defaults;
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
removal or idle expiry. Workspace lookups index the JSON record's current owner,
falling back to its last owner only when unclaimed; SQLite maintains the index
on every write and rejects malformed ownership fields. Allocation planning is pure
over recorded devices and inventory; its executor revalidates ownership and live capacity under the simulator
gate before ordered audit, claim and simctl operations. An omitted runtime selects
the latest installed, available iOS version compatible with the requested device;
explicit runtimes stay pinned and existing leases keep their runtime. Generic permits consume
pool and member capacity in one transaction; rwlock members allow unlimited readers sharing one slot or one
writer, default to write, and require release to change mode. Definition drift
blocks new claims but never revokes permits. `kind = "repo"` resources return a
registered checkout's path with cooperative read access; readers share one slot.
The lease binds its repository ID and path, including for approval, survives
restart, and prevents repository deletion while borrowed by another repository's
workspace. Release and workspace cleanup only drop the lease; they never modify
the borrowed checkout. The view is live, with no automatic fetch or pinned ref.
Optional daemon hooks run after a
permit is persisted and before it is released; failure retains ownership.
Repeated acquisition reruns its hook against the same lease, so scripts must be
idempotent. Hooks receive lease JSON, use normal config precedence and executable
path rules, and cannot overlap permit or lifecycle transitions in their workspace.
Release hooks also run in the shared removal path while the worktree exists;
all permits remain owned until removal succeeds. User scripts own integrations
with the underlying resources.

Configured resources may require human approval for scoped acquisition. The daemon
persists a request with a caller-supplied reason and the effective allocation settings;
only an unscoped caller decides it. Pending requests consume no capacity. Approval
lasts until release by default, or for the workspace when configured, limited to the
same resource and access settings. Changed settings require a new request; approval
never overrides capacity, scope, or simulator cleanup rules. Requests and grants
survive restart and failed removal, and disappear with successful removal.
Approval lookups validate the selected ID or active workspace/target/name record.
Grant reuse validates all records for the workspace and target, including released
grants, before matching typed settings. Malformed history outside those selections
does not block access; listings validate every selected record.

Port, simulator, and generic resource commands share the same shape: the bare
noun (or `list`) combines relevant configuration or capacity with leases;
`acquire` and `release` change ownership. Simulator machine inventory remains
under `sim catalog`. All-workspace overviews that need independent per-workspace
reads use bounded concurrency, retain workspace order and report every failure.

## Removal and recovery

Workspace completion uses `[done] cleanup` (default true), resolved through the
normal configuration layers with explicit keep and cleanup overrides. Completion
is persisted separately from lifecycle readiness, binds to HEAD, and notifies the
user; it does not assert that work was merged. Own-workspace continuation cancels
pending completion and defers issue, PR and idle cleanup until explicit completion,
persisting across restarts while preserving associations and merge requirements.
The daemon runs `post_done_cmd`
after persisting explicit or automatic completion and before cleanup, using normal
config precedence and the worktree's hook identity plus the keep/cleanup choice.
It is bounded, excludes lifecycle and permit changes, and reports failure through
a notification without undoing completion or blocking cleanup. Explicit signals
rerun it; events are not queued or replayed on restart. Completion/PR serialization
covers the hook, which must not call gated Shoal mutations. Completion without a
hook takes no resource guard, so an agent-exit hook may signal it; configured
completion hooks cannot nest inside another guarded hook. Keeping a completed workspace
suppresses idle and PR cleanup until another completion requests cleanup or the
user removes it. Completion
cleanup without a PR registration requires clean files and every commit pushed or
on the local default branch; an existing PR registration keeps its merge checks.
The daemon stops tracked executions through shared removal and rechecks files and
HEAD, including after hooks. Changed HEAD retains the workspace until a new
completion signal; failed cleanup retains ownership and reports why.
Issue associations suppress idle cleanup. The daemon polls their repository-bound
URLs using its existing forge login and records completion once closure is
confirmed, honoring the done default without replacing an existing completion.
Lookup failures retain the workspace; reopening the issue does not undo completion.

Manual and automatic cleanup share one path: establish ownership, stop owned
executions, run removal hooks, remove owned simulators, remove the worktree,
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
user's gh/fj login, resolve numbers against the workspace's origin into stored
URLs bound to that repository. Watch registration and cancellation are explicit
actions; cancellation can select one PR or the entire set. Watches accumulate
without duplicates and require
every watched PR to merge, with HEAD present in at least one; the existing branch
checks apply to each PR. Once confirmed, the daemon records completion through
`done`, honoring its configured default or a prior explicit choice. The confirmed
HEAD persists so restart cannot complete the same watch set again. Persisted manual
acknowledgement binds to exactly the recorded HEAD. Registrations distinguish watches from
acknowledgements; legacy single-watch records retain their stored and JSON shape.
Ambiguous records and conflicting actions are rejected.
When completion requests cleanup, confirmed merges stop tracked agents through shared removal,
rechecking clean files and HEAD after stopping. Failures retain work and leases;
registered workspaces are excluded from idle cleanup until cleared or removed.
An invalid record retains its workspace without blocking cleanup of others.

`doctor` reports current issues by default, falling back to the recorded
failure and repair guidance; repair restores verified worktrees and clears
executions proven stopped while preserving work and leases. Added
environment and untracked-worktree checks are diagnosis only. The daemon
checks its own PATH using the shared executable catalog and Git worktree
registrations under owned repository roots;
the CLI diagnoses daemon health before requesting workspace checks and checks
shell integration locally, including when the daemon is unavailable. Opening
persistence only migrates schema. Under the daemon lock, startup then atomically
quarantines interrupted operations in a separate transaction before ownership
auditing, serving requests, or cleanup; failure aborts startup and leaves committed
migrations available for retry. Startup audits but never deletes, kills, clears
unknown executions, or releases leases.
Survivors are signaled only after verifying PID birth identity and same-user
ownership; acknowledgement cannot override visible live processes. Recheck
unreadable process identities before treating visibility as incomplete; an
identity that has exited no longer blocks the ownership proof. Recovery polls
incomplete proof within the workspace stop budget and still refuses live or
unverifiable survivors. Moved
worktrees stay unresolved until restored. Any other ownership failure is
recoverable by explicit reclaim, which a human requests after checking the
worktree: it re-marks a linked worktree of the recorded repository at the
recorded path on the recorded branch that no other workspace owns, then
repairs normally. It is never automatic, and scoped callers cannot run doctor. A deleted directory means
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
- **Distribution:** stable service identity across
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
