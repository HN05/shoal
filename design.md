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
tokens and does not switch the user's or daemon's login. An agent Git profile
selected the same way reaches tracked agents as Git's environment-level config,
so the agent's commands use it in any repository while the worktree's config and
other executions keep theirs. Desktop handoffs have no authentication override
because they may reuse an existing process.

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
Workspace JSON records expose canonical linked-item URLs under `links`, with one
optional issue URL and a list of watched PR URLs; acknowledgement-only PR cleanup
records have no URL to expose.
The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups. It retains buffered daemon controls across start, stop,
and recovery transitions so adjacent frames cannot be lost. After commands and
interactive hooks it restores the caller's foreground group and OS terminal settings. When `TERM` is nonempty and not `dumb`,
it also resets emulator input modes for the shell, writing best-effort cleanup directly
to the terminal even when output is redirected. It registers through one request carrying its execution kind,
and gives the entire command process group a shared grace period on stop requests,
even after its leader exits. The daemon prepares each kind before shared registration.
The daemon never proxies terminals, and a lost connection is not proof that an
execution stopped or that its resources are free. Command wrappers outlive the
daemon instead: on a lost connection or a shutdown's detach control they keep the
command and terminal, retry the socket with capped backoff, and reattach with the
execution ID, wrapper and child identities, process group and the execution's
scope token. The daemon verifies these against the record, restores scope and
agent metadata, and serves the connection as before; a command that exits
meanwhile reports once reattached. A refusal stops the command as manual stop
does. Reattachment is a permanent protocol method accepted at every version, and
wrappers advertise it when they register: graceful shutdown detaches those and
stops the rest as manual stop does, within the workspace stop timeout. Setup and
landing hold daemon gates, so they never reattach.

Overload protection is configured machine-wide: memory is enabled by default,
CPU is opt-in and requires sustained aggregate busy time. Stop connected tracked
agents through their execution wrappers, preserving work and leases. Stop and exit
notifications carry the pressure reason and recovery path, distinguishing automatic
restore from manual recovery and missing recovery records. Persist a minimal
agent recovery record with the stop reason before delivering the stop, so a lost wrapper does not lose
the handoff; failure to save warns without disabling overload protection. Agent
session recovery is automatic for built-in Codex and Claude resume commands, or
when explicitly configured for another agent, and load and disk space have
recovered;
never replay the original task prompt. Keep waiting wrappers tracked, serialize
restores, and preserve manual recovery across restarts. Saved recovery represents
unfinished work until restored or explicitly discarded.
Agent metadata is transient, so restart cannot select disconnected survivors.
Disk protection reads available space on filesystems holding workspaces or
daemon state. Below its cleanup threshold it removes idle cleanup candidates there
without their idle delay, through the same removal path and repository settings.
When that cannot keep space above its stop threshold, it stops every agent as
overload protection does and every other tracked execution as manual stop does,
since any of them may be writing. Agents restore once space reaches the cleanup
threshold, so restores cannot refill the disk at once; commands stay explicit. A failed reading authorizes nothing on its filesystem
and blocks recovery; readings of other filesystems still protect them.
Manual stop ends connected tracked executions through their wrappers, preserving
work and leases. Agents save session recovery for explicit resume; user commands
save their arguments, which resume reports once, as the restored agent's first
prompt or else to the user, and never reruns, because replaying an arbitrary
command is not known to be safe. Scoped callers cannot stop executions.

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
Managed daemons watch their stable executable path and restart after a replacement
once tracked executions finish and daemon operations are idle.
Retain the ownership lock and listening socket across exec so queued requests reach
the replacement under the same service PID. Read-only streams reconnect after the
handoff, preserving event cursors; foreground daemons keep explicit restart control.

Advisory repository reviews default to Codex, retaining configured agent overrides
and automatic fallback. Repository code audits run read-only inspections per focus on a weekly
rotating area and open one issue per new finding. Failed or incomplete attempts
open no issues; audit results are advisory and do not gate merges.

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
display name. Repository selectors preserve the caller's argument alongside its
canonical path. IDs and sources win before path lookup; a registered name survives
unrelated path collisions, while a name and path matching different registrations
require disambiguation.
`repo rm` binds confirmation to the resolved registration and shows
its source and checkout path; it deletes the directory only once it is empty.

A new branch starts from the repository default branch: `origin/HEAD`, the sole
remote's HEAD, or the checkout's current branch without remotes, never a guessed
`main`. The selected local default branch is fast-forwarded from its upstream first,
preserving an ahead branch and refusing divergence, dirty or managed checkouts, and
failed fetches. `--base REF` starts from any locally resolvable
commit ref without refreshing, except when it names the local default branch or a
configured remote's branch, which is fetched first and must succeed.
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
diff base, or the opening commit if unavailable or on that same branch.
A stacked workspace records the workspace whose branch it builds on, inferred when
the creation base names another workspace's branch locally or on a remote, so
stacks span workspaces on any forge. The base is local metadata, not a link:
removing it moves its stacked workspaces down to its own base and journals the
change. When every watched PR of the base merges and covers its HEAD, the PR sweep does the same
before cleanup can remove it, retargets stacked PRs still targeting its branch,
and queues a watch update; agents rebase their own branches, since Shoal never
moves workspace branches. Retargeting is Shoal's only forge write: `gh pr edit`,
or Forgejo's API with fj's saved token for that host because fj cannot edit a
base. Adoption
takes a path; the CLI infers its repository from the owning checkout, then a unique
`origin` remote match, then a picker, with `--repo` as the override. Explicit
adoption accepts an unlocked linked worktree root on a local branch of the registered repository with no ownership
conflict, preserving dirty files and Git settings and recording it ready without
setup or hooks. It takes normal cleanup ownership. Reopening verifies its record;
adoption cannot repair a moved or replaced managed worktree. Never adopt a main checkout.
`adopt --copy` creates a fresh worktree under the repository's Shoal directory,
copies the source worktree's files while leaving the source in place, and adopts
the copy on an available branch derived from the source branch.

`shoal rename` changes a ready workspace's checked-out branch and derived name
in one durable operation; its path, identity, resources, and associations stay
with the workspace. The default branch, a branch checked out elsewhere, or a
name collision is refused. No execution other than the scoped caller may be
recorded, and interrupted intent blocks reuse until explicit repair or verified
deleted-worktree cleanup.

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
survivors. Failures preserve files, branches, and leases; process-verification
failures retain the workspace and report the command's exit status and the
blocking evidence. Other failures let interactive callers choose delete, ignore
(which repairs verified state first), or keep. JSON callers get a nonzero exit.
`setup` reruns setup explicitly; nothing retries automatically.

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
failure retains the workspace. Ownership and removal policy are rechecked after
all removal hooks, with branch deletion derived from the final inspection.
Idle cleanup requires unchanged
HEAD and activity; manual removal without a branch choice retains changed HEAD.
Post-remove runs after ownership is released,
from the repository checkout with its copy of the executable, retaining the old
workspace path in the environment. Its command is selected before removal.
It is best-effort: failure is a warning and notification, never a failed removal
or restored ownership. Cleanup of deleted worktrees tolerates missing ancestors
and skips hooks; post-remove events are not durably queued or replayed.

`diff` compares against the recorded base's fork point (merge-base fallback, fixed
commits stay fixed) with native Git settings, so advancing the base is never shown
as work. `sync` fetches the default branch's remote and fast-forwards the local default
branch under the creation refresh rules, advancing a clean registered checkout where
`git fetch` into the checked-out branch is refused; it never pushes or moves workspace branches, so updating
a workspace from it stays plain Git. Shoal has no merge or rebase command: Git
already does both in a workspace. `land`,
the local substitute for a pull request, merges the workspace branch into the default
branch, first refreshing a configured upstream while preserving an ahead branch and
refusing divergence or fetch failure. It pushes the default branch to that upstream
only on request, checking the upstream before merging and keeping the merge when the
push fails. The default branch cannot be
held by a managed workspace, and any checkout of it must be clean. Land aborts a merge
that does not apply cleanly, leaving conflicts to a Git merge of the default branch into
the workspace. Landing holds the repository Git gate for the tracked execution; the
daemon validates and refreshes, and the CLI worker merges.
After cancellation, the wrapper rolls back incomplete merges after stopping the
process group, preserving completed merges and reporting unsafe recovery failures.
Refresh fetches use private temporary refs; landing merges run in the tracked wrapper.

## Scope and user interfaces

Tracked executions and processes started with exported workspace environments
inherit a daemon-validated scope token that
confines them to their own workspace: status, inspect, execute, setup, and resources.
`land`, creation, removal, reconciliation, other workspaces, repository
administration, and service control need an unscoped caller. PR registration,
manual merge acknowledgement, assignment completion and its withdrawal, and syncing the
caller's own repository are own-workspace exceptions.
Effective configuration may be read for the caller's own workspace; changing it
needs an unscoped caller.
Nested executions keep scope, and the CLI refuses scoped operations to a process
whose ancestor carries scope for the same state directory, so removing the token
from a descendant's environment does not lift them. Unscoped callers may export a ready, verified
workspace's identity, current port variables and a fresh token through `env`.
Exported tokens persist across restarts until explicitly revoked or the workspace
is removed; they register no execution and do not prevent cleanup.

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
By default, agents that start with an issue prompt or forwarded arguments stay in
the background, and tabs that wait for the user's input receive focus; an
explicit focus setting overrides this choice.
The worker drops the plan before starting children; a retained tab's shell keeps
it, inert, while a non-default state directory is passed only to the worker.
Herdr tabs remain open after tracked agent exits. Completion closes them only
after tracked executions have resolved or Herdr reports the agent's turn finished
(`idle` or `done`), since interactive agents stay running at their prompt, and
never while stopped work waits for resume, so the tab keeps the stop's cause and
`shoal resume --all` can restore the agent in its pane; removal closes them
immediately.
Preparation failures and shell or untracked desktop handoffs retain them until
that lifecycle change. A tab watcher refused for a protocol mismatch restarts
from the installation path Shoal was started through, never a resolved versioned
binary, once another binary is installed there, so tabs opened before an upgrade
still close.

Single-workspace actions select an explicit target, otherwise the caller's scoped
workspace or the workspace containing the current directory, then an interactive
picker; a deleted current directory provides no workspace context. Explicit misses
fail without fallback; noninteractive and JSON calls
without a current workspace require a target. Scope remains daemon-enforced.
Bare `cd` always picks; all-workspace operations retain their scope.
An omitted required argument that names an existing record opens a picker on a
terminal; noninteractive and JSON calls require it.
An agent whose executables are not on PATH
is refused before any work starts and left out of pickers, the workspace menu
and completion; pickers offer “No agent” to continue without launching one. `config show` reports effective repository values with their winning layers and uses a registered checkout
before a workspace exists, then the picker. `status` combines lifecycle, fork-point changes, active work,
leases and watched PR state in one workspace view; the daemon looks PRs up on each request
and keeps a failed lookup on its PR. Human output uses `Display` for enum values and a shared
semantic palette at the CLI presentation layer; machine output and stored values
stay unstyled. Progress during silent waits belongs to the CLI and shows transient elapsed-time feedback on terminal stderr,
suppressed for JSON and dumb terminals. Rust chooses paths, including
`<root_dir>/<repo>` after removal; the Bash/Zsh wrapper changes directory
without evaluating repository code. If cleanup removes the current directory or
a pending navigation destination, shell integration recovers to its nearest surviving
ancestor after the command or at the next prompt, preserving the command status.
Recovery needs no daemon and is disabled for scoped callers.
Service installation prints the shell initialization hint only while neither
startup file contains an active initialization command.
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
the bare shorthand. Without a current or explicit workspace, repository-only
commands pick from the workspaces whose configuration defines them. Unknown
names report a command error with suggestions for similar built-ins and never
open a picker. Workspace fields expand once within individual
arguments; a standalone `{args}` places the caller's literal arguments.
`{diff_base}` lazily uses `diff`'s daemon lookup. Built-in `review` chooses
between the `review` command and an agent prompted to report, not change, the
work. An agent reviewing a forge item posts its findings there as one comment
unless `[review] post` or `--no-post` keeps them local; workspace reviews stay
local. A PR target, named by URL or `--pr`, resolves a same-repository PR with the user's forge login and
reviews its head branch against the PR base, reusing the owning workspace after
fast-forwarding it to the pushed head; local commits ahead stay, divergence is refused.
An issue target asks an agent to refine the issue before implementation, in the
workspace `add --issue` would continue, opening it when missing. Review tools own review storage, exports, and forge authentication, with
explicit feedback handoff to agents.

AI tools are providers: built-in ones plus global `[ai.<name>]` entries. A
`command` there is the tool's launcher at the global `[commands]` layer, so
repositories can still replace it, and gives it a `shoal <name>` agent shortcut
that `run` shares; names that built-in commands shadow are rejected. Pickers
and completion offer providers only, while custom `--agent` and `default_agent`
names select any named command through the same configuration layers. Agents run
as tracked agents with scope, forge wrappers, and exit notifications. Their `{prompt}` argument combines general instructions
and issue context; without it, nonempty context precedes forwarded arguments.
Plain command invocations expand `{prompt}` to an empty string. User arguments
and inserted prompt text remain literal. Claude and Codex retain
their specialized launchers; other names add no tool-specific flags or trust setup.

Agent shortcuts use the execution wrapper: by default Codex CLI gets full access without
approvals and Claude runs with its own settings. Both trust the
workspace, its repository's Shoal directory, and the registered checkout in their
user config before launch, creating the file if needed; Happy
launches and Codex app handoffs do so too. General agent templates become native
CLI instructions or a first-message prefix for Happy Codex; desktop handoffs carry no instructions.
Codex's default mode is a config value read at launch; `--cli` and `--app`
override it.

Issue-based workspace opening requires an open issue and persists its canonical
URL before tracked setup or agent launch. Reopening first follows the issue
association, then an existing local branch with the derived name, rather than
suffixing a new branch; conflicts fail before the agent picker. Associations are idempotent and cannot
be replaced. Status and inspection expose them.

`add <link>` recognizes issue, PR and branch URLs and selects their registered
repository by origin identity, offering interactive registration when missing.
Issue links and numbers invoke issue opening with `default_agent` standing in
for `--agent`; numbers use `--repo`, the current checkout/workspace, then a picker.
PR links open their head against the refreshed remote base and branch links open
an origin branch, using existing-branch ownership checks and retaining an owned
workspace's diff base. Opening fork PR branches is refused. Interactive `add`
without a branch, base or issue also offers the repository's open issues and PRs,
which open as their links do.
Interactive `add` uses an agent picker when no agent is named; non-interactive
additions launch only when an agent is named. `add --agent` launches only after
creation, setup, and the post-setup hook
succeed, or after an explicitly ignored setup failure, and retains the
workspace whatever the agent does. Desktop handoffs (Codex app, T3) provide no
tracking or scope; external hosts can hold a workspace while their session is open.
Happy sessions are the phone-driven flow: a console session that
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

Workspace lifecycle events persist in a separate SQLite journal alongside
lifecycle transitions and cleanup outcomes, survive ownership removal, and retain
the newest 1,000 records with increasing IDs. Unscoped integrations replay or
follow them without consuming notifications; expired cursors report a gap for
resync. External branch changes are observed during daemon sweeps without
changing recorded ownership. Issue and PR association changes append `linked` or
`unlinked` events in the same transaction as the association mutation, carrying
the item's kind and canonical URL so integrations can follow links without
polling workspace inspection.

Notifications stay in the terminal: the daemon records what a user would
otherwise miss (busy resources and who holds them, port conflicts, exits of
shortcut-launched agents, workspaces it removed or retained on its own, messages
a workspace's agent sends to ask for attention) and the
CLI shows them once, on request or as a followed stream that also raises the
terminal's own notifications (OSC 9); `ls`, workspace `status`, and `daemon status` only count them. Recording never fails the operation it describes,
repeated polled conflicts and identical messages collapse until read, and scoped
processes cannot read them. Agent messages are separate from completion: telling
the user a PR is ready neither ends the assignment nor affects cleanup. `post_agent_exit_cmd` exposes tracked-agent exit notifications to user
integrations while the workspace is ready, using normal configuration precedence
and daemon hook rules with the agent name, reported code, and process-completion
status. Failure notifies without changing the exit result or marking completion;
removal uses its own hooks. Desktop or push delivery was considered and not adopted.

Ready-for-review marks are the agent's status signal for integrations, separate
from messages and completion: they never notify, complete, or change cleanup. A
mark covers a linked issue or PR, or the workspace when nothing is linked, and
binds to HEAD; new commits make it outdated rather than removing it, so marking
again is an explicit statement about the new revision. Marking linked items
replaces a workspace mark. Unlinking an item
withdraws its mark and removal deletes all marks. Marks and withdrawals are
journal events, and `post_ready_cmd` runs after each explicit mark under the
completion hook rules; failure notifies and keeps the marks. Shoal does not act
on the forge for a mark. Proposal: an opt-in that undrafts a linked PR (#454).

Skills are split by role: `shoal-worker` covers an agent's own workspace and
`shoal-orchestrator` covers unscoped coordination from a console, so neither
role loads the other's commands. Both are installed together at user scope,
for every tool whose skill directory already exists unless a tool is named,
independent of the daemon and never from a scoped execution; their availability
registers nothing. Each skill directory records what Shoal installed there;
Shoal owns a skill only while it matches that record, so it updates or removes
only those, and treats files from before the record as its own. Unscoped
commands other than the daemon refresh directories that still hold a Shoal
skill, so upgrades need no reinstall; the daemon may not share the user's tool
environment. A changed or removed skill stays as the user left it until an
explicit install, which restores removed skills and with `--force` replaces
changed ones. Global `[ai.<name>]` settings name skill directories, with defaults for built-in providers. Skill directories describe
the machine and cannot be set per repository; provider launchers join named
commands without tool-specific integrations. Packaged skills resolve the runtime
`SHOAL_SKILLS_DIR`, build-time directory, then an adjacent `shoal-skills` symlink
to a directory holding every bundled skill. Explicit paths must be absolute;
relative link targets resolve lexically against the link's directory. Preserve
stable installation prefixes so upgrades apply; unpackaged binaries install the
embedded copies. Homebrew launches the binary directly, without a shell.

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
`install` adds missing templates from the bundled repository-root defaults and
updates a regular file that still matches an earlier bundled default, since agent
guidance must reach existing installs; edited or linked files stay untouched;
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
blocks new claims but never revokes permits. `kind = "repo"` resources name a registered repository, or a remote URL matching
one, and return its checkout's path with cooperative read access; readers share one slot.
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

Leases use top-level verbs that take the kind first: `acquire <kind>`,
`release [kind [item]]` and `leases [kind]`, with `--workspace` instead of a
positional workspace. Like `unlink`, omitting the item or kind widens release
to every lease of that kind or in the workspace; listings combine configuration
or capacity with leases. Holds stay separate because they keep a workspace
rather than share capacity. The per-noun `port`, `sim` and `resource`
subcommands remain accepted but hidden; simulator machine inventory and audit
history remain under `sim`. All-workspace overviews that need independent per-workspace
reads use bounded concurrency, retain workspace order and report every failure.

## Removal and recovery

External sessions acquire caller-named workspace holds independently of assignment
completion. Holds are idempotent by name and persist across restarts. They block
automatic removal while the worktree exists, allowing automatic completion to
record done; releasing the last hold restores normal cleanup eligibility. Explicit
removal lists holders and releases holds with the workspace record; deleted-worktree
cleanup still forgets missing worktrees. Release remains available through failed or
active lifecycle operations and hooks. Scoped callers manage only their own workspace.

Workspace completion uses `[done] cleanup` (default true), resolved through the
normal configuration layers with explicit keep and cleanup overrides. Only an
explicit done signal completes an assignment unless `[done] automatic` (default
false) lets issue closure and merged PR watches record it, so a merge or closure
never removes a workspace whose agent is still working; agent instructions make
`done` the last step of every assignment. Completion
is persisted separately from lifecycle readiness, binds to HEAD, and notifies the
user; it does not assert that work was merged. Own-workspace withdrawal removes a
recorded completion and preserves associations and merge requirements, but never
defers cleanup: holds are the one mechanism that keeps a workspace.
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
HEAD, including after hooks. Tracked agent exits wake the cleanup sweep after
their exit hook, without waiting for the polling interval. Changed HEAD retains
the workspace until a new completion signal; failed cleanup retains ownership
and reports why.
Issue linking and unlinking are own-workspace operations; associations are idempotent
and replacing one requires unlinking it first. Unlinking preserves recorded completion.
Issue associations suppress idle cleanup. With automatic completion, the daemon
polls their repository-bound URLs using its existing forge login and records
completion once closure is confirmed, honoring the done default without replacing an existing completion.
Lookup failures retain the workspace; reopening the issue does not undo completion.

Manual and automatic cleanup share one path: establish ownership, stop owned
executions, run removal hooks, remove owned simulators, remove the worktree,
and release leases with the record. Failures retain what is needed to retry.
Manual removal deletes a redundant branch (tree equal to the local default or its
upstream, or merged into the default) and otherwise requires an explicit keep or
delete choice. The default branch is retained unless deletion is explicit.
Automatic cleanup removes only clean, idle worktrees whose commits are all on a
remote or the default branch, with no executions, directory users, leases, or
permits, rechecked immediately before deletion, without fetching. `shoal cleanup`
removes those candidates on request without their idle delay, including where idle
cleanup is disabled; it is unavailable to scoped callers. `repo rm`
deletes the checkout and every workspace through that path, refuses external
worktrees and dangerous paths, persists progress, and blocks new workspaces until
an interrupted removal is retried.
PR cleanup is separately enabled by default: persisted watches use the
user's gh/fj login, resolve numbers against the workspace's origin into stored
URLs bound to that repository. Top-level `link` and `unlink` manage associations;
unlinking can select one item, one kind or the entire set. PR links accumulate
without duplicates and require
every watched PR to merge, with HEAD present in at least one; the existing branch
checks apply to each PR. Once confirmed, an explicit completion proceeds to cleanup;
automatic completion records it through `done`, honoring its configured default or
a prior explicit choice. The confirmed
HEAD persists so restart cannot complete the same watch set again. Persisted manual
acknowledgement binds to exactly the recorded HEAD. Registrations distinguish watches from
acknowledgements; legacy single-watch records retain their stored and JSON shape.
Ambiguous records and conflicting actions are rejected. Own-workspace `watch`
polls linked issue and PR activity independently of cleanup, with kind filters
or an explicit item that does not change associations. It reports comments or
reviews, completed CI checks, new merge conflicts, closure, reopening or merging. Forgejo CI
and mergeability fall back to the anonymous API, which reads the PR head without a
login, when `fj pr status` fails. The watch shares a
persistent cursor per workspace and item, reporting existing activity on the first
wait and changes between waits thereafter; cancelling the watch discards that cursor.
Activity lookups run outside completion serialization; persisting results
rechecks the watch set so cancelled watches cannot recreate their cursors.
Unacknowledged deliveries persist with the cursor so timeout or disconnection
cannot discard unseen updates; the CLI acknowledges after successful output.
Waits share the cursor, so a newer wait in a workspace supersedes the running one
before it takes updates, and an acknowledgement arriving while a newer wait runs
leaves the updates pending for it; competing waits would consume updates the agent
never reads.
Unavailable activity sources are explicit failures, never successful checks or
proof of mergeability; a persisting failure is reported again on an interval so it
cannot silence a wait while hiding a new conflict. Waiting does not resume stopped
agents or grant merge permission; agent instructions direct agents to handle
updates and wait again.
When completion requests cleanup, confirmed merges stop tracked agents through shared removal,
rechecking clean files and HEAD after stopping. Failures retain work and leases;
registered workspaces are excluded from idle cleanup until cleared or removed.
An invalid record retains its workspace without blocking cleanup of others.
A failed sweep step does not skip the others.
Background loops restart after a panic, and a failed daemon log write never stops them.
Failing or stalled cleanup shows in `doctor` and workspace status and notifies once
per run of failed passes.

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
identity that has exited or is exiting no longer blocks the ownership proof, and
an inaccessible macOS identity is treated as a different process because it
cannot be a same-user process Shoal started. One whose exec has not yet
published its environment is read again within a
bounded settle wait before it counts as unreadable. A reported command
exit clears its execution and permits setup readiness only when marker, child and
group evidence has no survivors and environment visibility is complete; a settled
empty environment is readable evidence. The reporting wrapper may remain alive
awaiting acknowledgement. Recovery polls
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
