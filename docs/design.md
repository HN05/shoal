# Shoal design

Decisions and proposals; usage: [README.md](../README.md), behavior:
[docs/reference.md](reference.md), contributor rules: [AGENTS.md](../AGENTS.md).

## Purpose and boundaries

Shoal manages local Git workspaces, executions, and shared development
resources for humans and coding agents. Cleanup is part of resource ownership:
a disposable workspace must not leave substantial run-owned data behind.

Agents cooperate with Shoal's scope and allocations. Shoal does not stop a
hostile same-user process from bypassing them through Git, the filesystem, or
direct resource access; filesystem restrictions are future work.

Caller-specific integration stays outside the daemon core: Superlogical owns terminals;
Macraft owns VM/container provisioning. Shoal does not provision tools, manage
browsers, schedule agent tasks beyond the `max_agents` queue, or store conversations. Agent shortcuts
(`claude`, `codex`, `happy`, `t3`) are thin launchers around the generic execution
path; CLI launcher argument arrays are configurable defaults; session and desktop
adapters retain their tool-specific setup. Opt-in forge authentication wrappers
are user-owned executables selected per tool through repository/global config;
tracked agents receive them on a private PATH inherited by their descendants.
Wrappers own credentials and tool-specific environment changes; Shoal stores no
tokens and does not switch the user's or daemon's login. `[secrets]` names
user commands that fetch values in the CLI, for an agent's environment at launch
or, for listed tools, through private PATH launchers each time the tool runs;
the daemon and state never hold the values, hooks drop them, and tracked
worktree files cannot name them. A tool secret may require approval: the
launcher fetches it only after the daemon confirms a workspace grant bound to
its command and tools, and fails instead of waiting. Environment secrets cannot
require approval, since they are fetched before the agent can ask. A setting
that takes its value from outside the config, such as a proxy's upstream key,
shares one form: as written, `{ env }` from the agent's environment, or `{ cmd }`
run like a secret's command when the agent starts, and Shoal prints no written
value. Forge changes Shoal
makes for a workspace act as the agent too: through the `gh` wrapper, and on
Forgejo with the token the agent's fj saved under the configured `fj_home`,
because fj has no API passthrough, or else through the agent's `tea` wrapper. An agent Git profile
selected the same way reaches tracked agents as Git's environment-level config,
so the agent's commands use it in any repository while the worktree's config and
other executions keep theirs. Desktop handoffs have no authentication override
because they may reuse an existing process.
The generic repository `[env]` table is available to tracked executions and hooks:
it renders workspace identity and reserved ports into user-selected variables
without managing the integrated tool.

## Architecture

One Rust binary provides the CLI, execution wrapper, and daemon. Each state
directory has one daemon shared across repositories, with a private Unix
socket and a versioned JSON protocol; the ordinary installation is per user.
Each protocol message is one frame of at most 64 KiB. Listings that can outgrow
one are sent in as many frames as they need, then `ok`; the daemon answers any
other reply that would not fit with an error saying so.
The minimum Rust version follows current stable so the code can use its newest
features; the manifest and CI toolchain advance together, with locked dependencies.
The daemon raises its open file limit at startup. Connections reading a request
or running a request/response operation share a fixed limit; executions, waits and
streams take a separate share of the remaining descriptors, so neither can block the
other. When that share is full, a request whose client sends it again after the
daemon closes its connection, as across a restart, is closed unanswered; others are
refused. Accept failures that clear on their own,
such as running out of descriptors, are retried.
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
optional assigned issue URL, that issue's title when the linking caller supplied it, a
list of watched PR URLs, and `opened` and `related` lists of items with their kind,
URL and known title; acknowledgement-only PR cleanup records have no URL to
expose. Linking the same issue again records a newly supplied title.
Their `running` flag says whether a tracked execution is running; unverified
executions after a daemon restart do not count. Their `done` flag says whether a
completion is recorded.
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
CPU is opt-in and requires sustained aggregate busy time. One `load` setting
(`light`, `balanced`, `full`) sets the default CPU and memory thresholds, so a
user chooses how hard to load the machine without tuning each percentage;
explicit thresholds override it. Linux memory defaults rise with swap, which
makes the OOM killer a later risk. Stop connected tracked
agents through their execution wrappers, preserving work and leases: the agent
idle longest first, since an idle agent loses no turn, and a working one, newest
first, only when no idle one is left. An idle agent with processes outside its
process group counts as working, since their completion may start its next
turn, and so does one without turn reports. Once the
wrapper reports the stop, the daemon also stops the agent's identity-verified
processes outside its process group, such as background shell commands, since
automatic and manual restores both require the execution proven stopped. When
the proof fails, the waiting wrapper stays connected and restores once it holds,
instead of leaving an unknown execution. Stop and exit
notifications carry the pressure reason and recovery path, distinguishing automatic
restore from manual recovery and missing recovery records. Persist a minimal
agent recovery record with the stop reason before delivering the stop, so a lost wrapper does not lose
the handoff; failure to save warns without disabling overload protection. Agent
session recovery is automatic for built-in Codex and Claude resume commands, or
when explicitly configured for another agent, and load and disk space have
recovered, except that an agent stopped between turns has nothing to do until
an update arrives for it, so that update restores it instead, and its restore
holds no agent slot until then. Manual and automatic restores prompt the agent to continue its
unfinished assignment; configured restore commands receive that prompt through
`{prompt}`. Never replay the original task prompt. Keep waiting wrappers tracked; they reattach
like running commands, so a daemon restart or crash hands the wait to the next
daemon instead of cancelling the restore. A daemon update waits for a stop
and a restore waiting for load, but not through a wait for an update, which
may last indefinitely; that wrapper reattaches to the updated daemon. Restores wait in the agent queue,
and manual recovery persists across restarts. Saved recovery represents
unfinished work until restored or explicitly discarded, except that a done
assignment needs no agent, so a done workspace's agent is restored neither
automatically nor by `shoal resume --all`, only by resuming that workspace. A
record names the
stop's stage: the daemon saves `stopping` before delivering a stop, and the
wrapper saves `stopped` after ending the agent. At startup, and whenever an
execution becomes unknown, the daemon forgets an unknown execution with a saved
record once its wrapper has exited and no owned process survives, if its launch
identity is complete or the record reached `stopped`, since the daemon clears a
stopped child's identity only after verifying the stop. Live wrappers are
rechecked when they exit; one that reattaches with its command running discards
a `stopping` record, whose stop never arrived. Survivors and incomplete evidence
keep the execution, are rechecked periodically so it clears once they exit, and
are the only outcome that notifies; `shoal stop` names them, and
`shoal resume` still reconciles the execution it selects.
Agent metadata is transient, so restart cannot select disconnected survivors.
`shoal restart` restarts agents in place through the same wrapper path, so the
agent stays in its terminal: the daemon sends the stop protection sends, with
the reason `restarted by shoal restart` and recovery whatever the overload
policy, so wrappers of earlier releases restore it too, and resumes the agent
once its processes are proven stopped, outside the agent queue, since it keeps
its slot. The restore command resolves the agent again, so an updated agent
binary runs; the wrapper keeps its own Shoal binary. The CLI restarts the
selected agents together by default; `--sequential` spreads the load of
starting them, restarting one at a time, idle ones first, starting the next
once the daemon registers the restarted agent and at least the admission
stagger after the previous start.
Agents without a restore command and executions whose wrapper is gone are left
to `shoal stop` and `shoal resume`; a restart that cannot start the agent leaves
its record for `shoal resume`.
The daemon also samples what each workspace's processes use, attributed by
the workspace identity in their environment, so the user can see which
workspace loads the machine; protection, warnings and admission do not use it
yet. Processes that leave the workspace's environment, such as work handed
to a shared server or container runtime, count for the machine only.
Warnings come first: below each enabled protection's stop threshold, the daemon
queues an agent message for every workspace with a running agent, so agents can
reduce load before anything stops. Warnings repeat per signal and workspace only
after an interval and never stop, clean up or notify. Their thresholds are not
validated against stop thresholds, so lowering a stop threshold never invalidates
an existing configuration.
Disk protection reads available space on filesystems holding workspaces or
daemon state. Below its cleanup threshold it removes idle cleanup candidates there
without their idle delay, through the same removal path and repository settings.
When that cannot keep space above its stop threshold, it stops every agent as
overload protection does and every other tracked execution as manual stop does,
since any of them may be writing. Agents restore once space reaches the cleanup
threshold, so restores cannot refill the disk at once; commands stay explicit. A failed reading authorizes nothing on its filesystem
and blocks recovery; readings of other filesystems still protect them.
Keeping the machine awake is opt-in and machine-wide: a laptop held awake with
its lid closed drains its battery and heats up in a bag, so only its user can
choose it. While enabled and an agent's turn is working, or an idle agent
watches an open PR whose activity will start its next turn, the daemon holds the
platform's sleep inhibitor as a child process that lasts as long as a pipe only
the daemon holds, so the inhibitor cannot outlive a daemon that crashes or is
killed. A failed inhibitor notifies and waits for the work to end or the config
to reload instead of retrying in a loop.
Manual stop ends connected tracked executions through their wrappers, preserving
work and leases. Agents save session recovery for explicit resume; user commands
save their arguments, which resume reports once, as the restored agent's first
prompt or else to the user, and never reruns, because replaying an arbitrary
command is not known to be safe. Scoped callers cannot stop executions. A hangup
or terminate signal to an agent's wrapper or agent saves the agent as manual stop
does: closing a terminal, logging out and shutting down end the agent's session
without the agent choosing to quit, and the daemon may already be gone. Unlike a
manual stop, the agent then comes back on its own through the agent queue: a
request beside its record asks for the restore, which the daemon queues once the
execution has ended, so agents a reboot interrupted return without a command.
The request is its own file so that forgetting it, as a stop does, never
replaces a record a resume holds. An
agent that exits on its own before its assignment is done leaves its workspace
a last session for `shoal resume` instead of stopped work: the user may have
quit it on purpose, so it takes no agent slot and keeps nothing from cleanup.
Built-in agents resume the session ID their hooks report, since the workspace's
latest session may belong to another agent.

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
Service setup manages one per-user launchd/systemd service and refuses to change
its state directory until the existing service is stopped; foreground mode covers
environments without a service manager. The service captures the installing shell's
`PATH`.
Managed daemons record their stable executable path before startup work and restart
after a replacement, including one during startup, once daemon operations are idle
and no setup or landing runs; commands reattach.
Retain the ownership lock and listening socket across exec so queued requests reach
the replacement under the same service PID. Read-only streams reconnect after the
handoff, preserving event cursors, and item waits resume for their remaining time;
when the replacement speaks a newer protocol, they continue by executing the
installed release with that cursor or time.
An update does not wait for clients of the previous protocol. The old daemon refuses
newer requests before running them, so the installed CLI sends them again until the
replacement answers; daemon controls report the mismatch at once. Foreground daemons
keep explicit restart control.

Advisory repository reviews default to Codex, retaining configured agent overrides
and automatic fallback. Repository code audits run weekly read-only inspections per focus over the
entire application and open one issue per new finding. Failed or incomplete attempts
open no issues; audit results are advisory and do not gate merges.

Distribution is a Homebrew formula in the shared HN05 tap: releases install
checksummed prebuilt Linux and macOS binaries on x86_64 and arm64; `--HEAD`
builds `main` from source. `shoal update` replaces a downloaded binary with the
latest GitHub release and leaves Homebrew installations to Homebrew, so one
package manager owns each installation. A shared release-target table drives cross-builds,
archive names and Homebrew platform selectors.
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
checkout, and outside the state directory; a root inside either, or inside a
recorded repository directory, is refused.
On macOS a root Shoal creates links to `<parent>/.<name>.noindex`, which holds
its contents: Spotlight ignores `.metadata_never_index` below a volume root and
its privacy list needs root, so only a skipped name keeps build output out of
the index, and the link keeps `root_dir` the same on every system. An existing
root is used as found; only `repo migrate` changes it, by default to the link,
or back to a regular directory with `--regular-dir`.
Before registration, workspace creation or an export writes a directory, the
daemon creates a file there itself under a time limit: its file access can
differ from the terminal's, and on macOS opening a file on a volume it lacks
access to can wait for a permission prompt no one sees.
Repository roots and explicit workspace paths resolve existing symlinks and lexical
`..` before boundary checks and creation, even when trailing components are missing.
An explicit workspace path overrides the destination for one creation, without
changing the repository directory; reject existing paths and overlaps with state,
checkouts, workspaces, or other repositories' directories. Cleanup owns the worktree,
never its parent. Directories are reserved atomically and never reused; an
in-place checkout placed as `~/shoal/<name>/<x>` adopts that directory, and
`--path` clones elsewhere. A repository directory moves only whole, into a new
`root_dir`, while nothing runs or uses it: its workspaces are reserved and the
move is recorded before anything moves, then the records switch and Git's
worktree links are repaired, so a stopped daemon finishes a completed move and
rolls back an incomplete one, deleting only directories with their recorded identity. Checkouts and workspaces outside it stay. Registration is idempotent by normalized origin URL,
then canonical path, never fetches, and keeps a stable UUID separate from the
display name. Repository selectors preserve the caller's argument alongside its
canonical path. IDs and sources win before path lookup; a registered name survives
unrelated path collisions, while a name and path matching different registrations
require disambiguation.
`repo rm` binds confirmation to the resolved registration and shows
its source and checkout path. After verifying the checkout's identity and
boundaries, it deletes the checkout recursively, then removes the repository
directory only when that directory is empty.
A contribution is a URL clone registered for work in a repository the user does
not maintain. `origin` stays the repository itself, so issues, PRs and syncs use
it; when the user's login cannot push there, the user's fork, created or reused,
becomes the `fork` remote and `remote.pushDefault`, which Git, `pr open` and
cleanup all honour. It goes through `repo rm`'s path once nothing of it has been
left for the unattended grace period: no workspace, queued agent, chat, pending
issue proposal, kept branch or borrowed resource, nothing `shoal work restore` could
use, and no work in its checkout that the remotes' advertised branches and tags
lack. The final
check records the removal in the same transaction, under the gates creation
takes, and queue and chat records refuse a repository being removed. An existing
registration is never turned into one.

A new branch starts from the repository default branch: `origin/HEAD`, the sole
remote's HEAD, or the checkout's current branch without remotes, never a guessed
`main`. The selected local default branch is fast-forwarded from its upstream first,
preserving an ahead branch and refusing divergence, dirty or managed checkouts, and
failed fetches. `--base REF` starts from any locally resolvable
commit ref without refreshing, except when it names the local default branch or a
configured remote's branch, which is fetched first and must succeed.
Creation, default-branch refresh, setup, repository removal, and recovery share
a per-repository Git gate. Shoal's own Git commands skip optional locks, so
background status checks never hold a worktree's index lock. Daemon ref updates
disable hooks, Git credential prompts, and SSH askpass while preserving the user's
SSH transport configuration;
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
moves workspace branches. Retargeting goes through the same REST transport as
issue and PR actions. Adoption
takes a path; the CLI infers its repository from the owning checkout, then a unique
`origin` remote match, then a picker, with `--repo` as the override. Explicit
adoption accepts an unlocked linked worktree root on a local branch of the registered repository with no ownership
conflict, preserving dirty files and Git settings and recording it ready without
setup or hooks. It takes normal cleanup ownership. Reopening verifies its record;
adoption cannot repair a moved managed worktree, and repairs one whose metadata was
replaced only by reclaiming it at its recorded path. Never adopt a main checkout.
`work adopt --copy` records a preparing workspace before creating a fresh worktree
under the repository's Shoal directory, copies the source worktree's files while
leaving the source in place, removes checkout files the source deleted at any
depth, and adopts the copy on an available branch derived from the source branch; failures retain a failed record for normal removal.

A child workspace is split from a parent with `shoal split`, so an agent whose
work needs another PR gets another branch and worktree instead of switching
branches or creating worktrees Shoal does not own. Splitting creates a new
branch in the parent's repository, from the default branch or a `--base` that
stacks it as usual, records the parent, and links an `--issue`; it is the only
creation a scoped caller may do. Children form a tree: a scope reaches its
workspace and every workspace below it, the CLI resolves a scoped command to the
child whose directory it runs in, and agent hooks always report for the agent's
own workspace. The parent's agent receives the issue and PR activity of the
children without an agent of their own, and theirs in turn, each update naming
its workspace, through the same cursors, deliveries and acknowledgements as its
own; a child's own agent, which reports its turn state or which Shoal tracks
there, as it tracks custom and Happy agents that report none, takes over its
subtree, and so does one whose restore is queued, since it receives what
arrived once it starts. A parent stays until its children are removed, through every unattended
removal like a hold, so one `done` at the top leaves the tree until its PRs
merge; each child completes on its own. Removing a parent moves its children up
to its own parent and journals the change. `work` lists children below their
parent, and `status` names both. `split --agent` gives a child an agent of its
own through the agent queue; the daemon starts every launch in an existing
workspace itself, detached, so the agent never inherits the caller's scope, and
scoped callers may queue only for the workspaces below theirs, and cancel those
entries or move them back, but never ahead of other work. A workspace's
coordinator is the agent of its nearest ancestor that has one, or the chat that
owns the workspace or an ancestor nearer than that agent: the events about
it that notify the user, the child agent's messages, questions and completion
and its agent's exits and stops, plus its ready marks, become that agent's
messages instead, falling back to the user without a coordinator or when its
messages are full. Events a process causes in a child without an agent of its
own are the coordinator's own work and reach the user. The coordinator answers
questions asked below it or passes them up with its own question, and may
discard the stopped work below it, since only it knows whether a stopped child
agent still has work; resuming stays the user's. Approval
requests go to the coordinator too when it holds an approved grant for the same
thing; it decides them or passes them up, the user deciding at the top.
The worker skill steers agents to a subagent working in the child's directory
over `--agent`: the child then has no agent of its own, so its activity reaches
the parent, and the subagent reports to the parent directly, supervision Shoal
need not relay. Claude Code's worktree hooks make the worktree it creates for a
subagent such a child, so its own isolation needs no worktree Shoal does not own.
A subagent inherits its agent's scope, and under Codex its instructions and
directory, so a `SubagentStart` hook names the workspace it works in; without
it, a Codex subagent finishes its agent's assignment as the instructions say.

`shoal work rename` changes a ready workspace's checked-out branch and derived name
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
`work setup` reruns setup explicitly; nothing retries automatically.
Tracked setup starts by copying the repository checkout's ignored files through
Worktrunk unless `copy_ignored = false`, so local files like `.env` arrive without a
setup script and failures gate readiness; `.worktreeinclude` selects what to copy.

Hooks are deliberately untracked user processes with the workspace identity
but no scope token, because their purpose is to start or stop things that
outlive the hook (a tmux session, say) without becoming execution survivors.
Nothing outlives the workspace: removal stops every verified process that still
carries its identity, including those hooks and exported environments started.
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
HEAD, activity and file metadata; sweeps compare HEAD and activity, and walk the
tree only when the timer starts, when it expires and after removal hooks, since
change times only move forward and cannot be set. Status reports the first reason
cleanup keeps a workspace from the checks the sweep runs, so it cannot disagree
with the sweep and lsof still scans only clean worktrees. A HEAD changed by removal
hooks keeps its branch when only that branch has the new commits.
Post-remove runs after ownership is released,
from the repository checkout with its copy of the executable, retaining the old
workspace path in the environment. Its command is selected before removal.
It is best-effort: failure is a warning and notification, never a failed removal
or restored ownership. Cleanup of deleted worktrees tolerates missing ancestors
and skips hooks; post-remove events are not durably queued or replayed.

`diff` compares against the recorded base's fork point (merge-base fallback, fixed
commits stay fixed) with native Git settings, so advancing the base is never shown
as work. `conflicts` asks `git merge-tree` whether committed HEAD merges cleanly
into a target, by default the base workspace's branch, otherwise the local default
branch, which `sync` keeps current; it writes only objects. `sync` fetches the default branch's remote and fast-forwards the local default
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
A fetch of a whole remote also deletes the tracking refs of branches the remote no
longer has, since cleanup counts commits on a stale ref as pushed.

A repository's `[seed]` keeps one build of its local default branch that new
worktrees start from, instead of sharing build directories between live
workspaces: a shared directory serialises or corrupts concurrent builds, while a
copy-on-write clone costs no space until a branch rewrites a file and needs no
tool support. The daemon owns a locked, detached seed worktree in the repository
directory, builds it when the branch moves and freezes the configured outputs as
a snapshot, so a running build never reaches a workspace. Creation clones the
snapshot under the repository Git gate and restores the seed's modification times
on tracked files its commit left unchanged, since mtime-based tools otherwise treat
every freshly checked-out file as new. Platforms without clones skip the seed
rather than copy. Seeds are best effort: failures notify and never fail creation,
and builds wait under host pressure so they never add load an agent would feel.
A repository's `presets` name the build tools it uses, and each sets up what
that tool needs to stop repeating work across workspaces, so common stacks need
no script. Tools whose build outputs survive a move into another path get a
seed: Cargo, whose seed covers it better than a shared compiler cache such as
sccache, which needs another tool and gives up incremental builds, and the Node
package managers. Preset seeds run the default branch's own build code, as
`seed.cmd` would. Tools whose caches already serve every checkout get only what
makes their cache keys independent of the workspace path, or one cache where
each checkout keeps its own: Go trims paths, ccache takes the workspace as its
base directory, Zig shares its local cache, and Xcode, whose DerivedData records absolute paths so no seed survives the move,
shares one compilation cache with the workspace and DerivedData paths mapped
out of its keys. Xcode also needs one package checkout per set of resolved
pins, which only an `xcodebuild` argument can choose, so the preset puts a
wrapper first on workspace processes' PATH. These presets trade real source
paths in debug info and stack traces for the shared cache, which is why they
are opt-in. Neither shrinks a workspace's own build directory: Xcode copies
cached results into each DerivedData. That folder lies outside the worktree,
keyed by the project's path, so the preset makes removal delete it and disk
space cleanup delete what earlier removals left; Xcode rebuilds it on demand.
Package checkouts are shared by pins rather than owned by a worktree, so only
those nothing pins go, and only after a day unused, since a build that resolves
packages rewrites its pins while it runs. Xcode trims the compilation cache
itself, by default to half the space available to it.

Tool caches whose tools lock or address entries by content, such as `GOCACHE` or
sccache's directory, are the exception: `[caches.NAME]` with `scope = "shared"`
gives a repository one directory per cache, and `[env]` maps it to the tool's
variable with `{cache.NAME}`, so workspaces stop repeating downloads and
compilation. The scope is explicit because a cache shared by live workspaces is
only safe for such tools. A cache is disposable, so `cleanup caches` and disk
space cleanup empty it, but only while no tracked execution or seed build of the
repository runs; registering an execution prepares the directories in the
transaction that records it, so emptying under the store's write lock cannot
race a start.

## Scope and user interfaces

Tracked executions and processes started with exported workspace environments
inherit a daemon-validated scope token that
confines them to their own workspace and the child workspaces split from it:
status, inspect, execute, setup, and resources.
Creation other than splitting a child, removal, reconciliation, other workspaces, repository
administration, and service control need an unscoped caller. A scoped `land` is an
approval request bound to the branch and its HEAD unless `[land] requires_approval`
is off; approval lets the agent's retry land that commit through the normal checks,
so it replaces only the user typing the command. PR registration,
manual merge acknowledgement, assignment completion and its withdrawal, and syncing the
caller's own repository are own-workspace exceptions.
Effective configuration may be read for the caller's own workspace; changing it
needs an unscoped caller.
Nested executions keep scope, and the CLI refuses scoped operations to a process
whose ancestor carries scope for the same state directory, so removing the token
from a descendant's environment does not lift them. A tracked execution's wrapper
records its own process identity in the state directory before it spawns the
command, until the command exits, and the command's identity while it lives,
because macOS hides the environments of its own platform binaries, such as
shells; an exported environment is recognised only where an ancestor's
environment is readable. Unscoped callers may export a ready, verified
workspace's identity, current port variables and a fresh token through `work env`.
Exported tokens persist across restarts until explicitly revoked or the workspace
is removed; they register no execution and do not prevent cleanup.

Root help groups built-in commands by task; workspace maintenance lives under
`work`, repository administration under `repo`, forge inspection and API access under
`forge`, branch workflow runs under `forge ci`, PR checks under `pr ci`, and
shell integration under `shell`. `run` lists configured commands, runs one by
name, or executes an arbitrary command after `--`; `--workspace` selects the
workspace in either form. `done --undo` withdraws completion and `ready --undo`
withdraws review marks. Creating a question uses `questions ask`; notifications use `alerts`, with
`alerts send` to send one. Notification rows, in `alerts` and the picker, show
the time for today's and otherwise how long ago they came in whole days, weeks,
months or years, such as `1 day ago` or `2 weeks ago`.
Approval snapshots carry their stable request key through the CLI so reused
numbers cannot decide a replacement. Implicit question and approval decisions
require stdin, stdout and stderr on a terminal, so redirected details cannot
hide the decision being made.

`herdr.enabled = false` turns off every Herdr call, so a Herdr pane behaves as a
plain terminal; `new_tab` only governs opening new tabs.
The CLI owns Herdr tab handoffs after issue lookup and interactive workspace and
agent choices and labels the tab `<repo>#<number>` for an issue, otherwise with the
allocated workspace branch by default. An optional `herdr.tab_name` template
follows normal configuration precedence, substitutes repository, branch and issue
fields once, and updates the branch after allocation. The resolved template travels
with the launch plan so the worker preserves the caller's naming choice. Shoal
renders the label in its generic terminal layer; a host only applies the label it
is given, so naming rules and their daemon and forge lookups live in one place.
The plan travels in the tab's environment, naming a found issue by URL rather
than carrying its unbounded body, and preserving
state/config selection and literal arguments without hook scripts or state files.
By default, agents that start with an issue prompt or forwarded arguments stay in
the background, and tabs that wait for the user's input receive focus; an
explicit focus setting or `--focus` overrides this choice. `--focus` also starts
the agent past `max_agents`, as `--now` does, because the user waits for the
launch they focus; the setting does not, so it cannot switch the limit off. Commands say only what they
launch; one terminal layer chooses the placement and the backend that carries
it out, so focus behaves alike everywhere and another backend plugs in with
its own primitives without touching commands: Herdr focuses a tab or
opens it in the background, and outside Herdr zmx attaches the caller or starts
a detached session running the plan, which it receives in its environment as a
tab does.
The worker drops the plan before starting children; a retained tab's shell keeps
it, inert, while a non-default state directory is passed only to the worker.
Herdr tabs remain open after tracked agent exits, and the tab's shell enters the
workspace through its own exported navigation file, followed once at a prompt
after the worker writes it. Completion that keeps the workspace closes the place
Shoal opened for it, a Herdr tab or a zmx session, only after tracked executions
have resolved or the agent's turn finished, since interactive agents stay running
at their prompt, and never while stopped work waits for resume, so the place keeps
the stop's cause and `shoal resume --all` can restore the agent there; removal
closes it immediately, so completion cleanup leaves the agent its grace period.
The turn comes from the agent's hooks, and from the host only for an agent
without them, such as Herdr's `idle` or `done`; zmx reads none, so its sessions
of such agents close with their execution. The rule, the watcher and its marker
are generic: a host only closes a place and reports whether it is open and what
its agent is doing. Top-level `close_when_done` applies to every host, and
`herdr.close_when_done` overrides it for tabs, as top-level `focus` is shared.
Preparation failures and shell or untracked desktop handoffs retain them until
that lifecycle change. A watcher ends once its host reports the place missing,
whatever the daemon reports; one refused for a protocol mismatch restarts
from the installation path Shoal was started through, never a resolved versioned
binary, once another binary is installed there, so tabs opened before an upgrade
still close.

Shoal closes only the zmx sessions it created; a session the user opened stays
theirs even after an agent ran in it. Creation
sets `shoal.created=1`, which relabelling never adds. The daemon closes a
workspace's sessions when it removes the workspace, whatever `close_when_done`
says, and startup and cleanup sweeps close those whose workspace record is gone,
retrying failures without restoring ownership. Sessions from before the label
have no known origin and are left alone. Relabelling replaces obsolete ownership
before any in-place agent or chat can launch, so removal preserves a session
reused by another owner. zmx has no conditional kill, so ownership checks,
relabelling and closes hold a lock beside zmx's socket directory, shared by
every state directory using that server.

Terminal agent launches run in zmx sessions so agents outlive the terminal:
detaching or closing it leaves the agent running, and `attach` reattaches. zmx
passes output through unchanged while attached and restores the screen from its
own terminal state on attach, so terminal protocols such as images keep working;
multiplexers that re-render output through their own emulator, like tmux, were
rejected for breaking them. zmx is a required dependency. The tracked wrapper
runs inside the session's pty, so tracking, scope, stop and recovery are
unchanged and the daemon still never touches terminal I/O. A failed detached
handoff closes its session only while its process and creation time still match.
When zmx returns control to the caller, the CLI resets emulator input modes on the caller's
terminal, and an addition whose agent ran in the session leaves the caller's
shell where it was: the workspace is the session's, and detaching returns to
the shell the user left. Sessions are named
after their workspace and labelled with its ID and canonical state-directory identity,
so daemons sharing zmx can distinguish their sessions. `attach` and `status`
look sessions up through `zmx list`. A detached session that Shoal
starts labels itself once its workspace exists. Launches without a terminal, inside a zmx session, or in
a Herdr pane with the integration enabled run in place, since Herdr reads agent
state from the pane's process. zmx reports no exit status, so a session launch
exits 0; the daemon still records the agent's exit.

Demos let the user try a change where the agent's windows are out of sight,
usually over SSH. A demo is a named command the repository configures or the
agent gives, and a workspace runs several. Its wrapper is an ordinary tracked
command that records the demo with its execution, so stopping, removal and
recovery use that execution. Its verified end closes the demo session, including
a disconnected wrapper stopped by the daemon, only while the host still verifies
its launch ownership. Pending closes persist with execution deletion and retry
at startup or cleanup, even while the workspace remains. The wrapper
restarts the command, backing off after quick exits so a failing command keeps
its error on screen. The CLI places the wrapper where queued agents start,
through the terminal hosts, since a demo is something the user opens and reads,
falling back to a background process with a log. Shoal knows how a demo reaches
the user only through presets, which reserve ports or lease a simulator and say
how to open it; the repository's `open` command replaces that, so tools such
as browsers and screen sharing stay outside Shoal. Starting, reading and stopping
a demo are own-workspace actions without approval: the agent can already run
the command, and a demo only keeps it running and tells the user.

`max_agents` admission belongs to the daemon, so concurrent additions and its
dispatcher share one count. Slots derive from state the daemon already tracks:
connected agents, executions awaiting reattachment, resume records and starting
queue entries, excluding completed workspaces. Agents therefore count however
they started, and no separate counter can leak a slot. The global value limits
the machine and a repository's effective value limits that repository; a
repository without its own value inherits the global one, which already bounds
it, so normal precedence serves both. The setting defaults to `dynamic`, which
admits from measured load instead of a slot count, so the machine runs as many
agents as it can without congestion. Agents start one at a time and staggered;
an agent reads files before it edits and compiles, so a recent start reserves
the load a settled agent shows, learned from CPU use per settled agent, until
its own load is measured. The target is the warning thresholds that `load`
sets, so the user picks how hard to push the machine rather than the
mechanism's windows and intervals. An idle machine always admits one agent;
critical readings admit none. Dynamic is machine-wide, so a repository's
`dynamic` adds no limit. A positive number fixes a limit and `none` disables it
for that layer. Fixed limits keep dynamic admission's disk rule, since an agent
started on a nearly full disk is soon stopped by disk protection. Per-agent
memory profiles remain a proposal.
Automatic restores are typed queue entries ordered with launches, queued when
the stopped agent's processes are proven stopped. A restore of an agent stopped
between turns waits for an update instead, holding no slot and holding back
nothing, since that agent has nothing to do until one arrives; woken, it is
admitted like a launch once free disk space has recovered, and an update's
admission holds through later stops. Any other waiting restore keeps the
agent's slot, so fixed limits let it take that slot back rather than counting it
twice, which would deadlock a full machine of stopped agents, while dynamic
admission measures its load like a new start. Restores also need the recovery
interval's healthy readings, one per interval. An entry waiting for anything
besides a slot, such as those readings, an update or a dependency, keeps its
place without holding back work queued behind it: the stopped agent's slot still
counts and dynamic admission measures load, so the limits, not the order, keep
new starts from undoing the protection. Admission binds the recovery epoch, which stops, reloads and
readings turning unsafe advance, so any of them before the agent restarts
returns the restore to its place. The slot passes to the
agent when its restored process registers. The entry is keyed by execution, so a reattached wrapper keeps its
place and a restart cannot restore it twice; an entry without its wrapper for the
start grace period is dropped, and cancellation or failure leaves the recovery
record.
`shoal resume` queues restores instead of starting every stopped agent at once,
which overloaded the machine; `--now` starts them at once, as `add --now` does.
No wrapper waits for such a restore, so the daemon starts it through the queue
start worker as it starts a launch in an existing workspace, and admits it like
a launch: its stopped agent holds no slot while it waits, so stopped agents past
a limit cannot keep each other from restoring. Its entry names the execution
without referencing its row, which ended with the agent, and leaves the queue
once the recovery record is gone.
`shoal queue` starts, moves and cancels entries, and the daemon owns their
order, `agent_queue.seq`: a move gives the waiting entries' numbers to them in
their new order in one transaction, so it lasts across restarts, and positions
count waiting entries machine-wide, as admission does. Moving a running agent
stops it as `shoal restart` does rather than as `shoal stop` does, so its
wrapper keeps the terminal and restores it there once the queue starts it. That
restore waits only for a free slot and holds none, whatever the overload policy,
since the user rather than protection sent it back, and lets daemon updates
through, since its turn may take long.
The queue stores
the CLI's launch plan opaquely and creates nothing until a slot frees, so a
waiting entry costs no worktree and branches from the default branch current
when it starts. A
starting entry keeps its slot through setup while the launching CLI holds a
connection open, with a short grace period for Herdr handoffs and daemon
restarts, until its agent registers; setup begins once the daemon holds the slot; a dead launch cannot hold a slot, and setup
time stays unbounded. The daemon starts admitted entries through a detached CLI
worker carrying the queuing terminal's Herdr variables, since terminal launch
logic stays in the CLI: a background Herdr tab when queued from Herdr, otherwise
a detached zmx session whose shell is given the worker, because zmx gives
commands it runs itself no terminal input. That shell is zmx's bash, so the
session ends once its agent or workspace shell has run, whatever the exit
status, and only a failed start replaces it with the user's login shell.
A queued launch may depend on workspaces, queued launches and issues, including
the blockers its issue's forge records, and admission passes it over until they
are complete. Complete means done at the current HEAD, not merged: the launch
branches from the done dependency and stacks on it, so dependent agents start
before review, and the existing restack on merge carries them to the default
branch. A commit after `done` is new work and holds them again. One launch builds
on one branch, so done dependencies on separate branches wait until all but one
merge. A launch's dependency becomes its workspace once it opens one, and
removed work counts as complete, since nothing is left to wait for.

Single-workspace actions select an explicit target, otherwise the caller's scoped
workspace or the workspace containing the current directory, then an interactive
picker; a deleted current directory provides no workspace context. Explicit misses
and other refused arguments fail without fallback, before any prompt for the
omitted ones; `add` asks the daemon to check its creation request first.
Noninteractive and JSON calls without a current workspace require a target.
An action that can apply to several workspaces takes several targets, and its
picker selects several: Tab selects each entry and Ctrl-A all of them. It checks
every target before acting on any, asks one confirmation that names what each
loses, and goes on after one fails, exiting 1.
Scope remains daemon-enforced. Bare `cd` always picks; all-workspace operations retain their scope.
A `-` target selects from a per-command history of workspace IDs that the CLI keeps
in the state directory, shared across terminals so `attach -` works from a new pane;
it only saves typing a name, so it never fails a command or authorizes anything, and
scoped callers neither read nor write it.
`resume` uses the current workspace only when it has something to resume; its picker
and completion list every workspace with the agents and commands it would
restore, or that nothing would, and with nothing to resume anywhere it reports
that without a picker. Its agent picker names agents by name, stop time and
reason. Shell
navigation refreshes the Zsh line-editor prompt after changing directories,
while Bash uses its prompt command hooks.
An omitted required argument that names an existing record opens a picker on a
terminal; noninteractive and JSON calls require it.
An agent whose executables are not on PATH
is refused before any work starts and left out of pickers, the workspace menu
and completion; pickers offer “No agent” to continue without launching one. `config show` reports effective repository values with their winning layers and uses a registered checkout
before a workspace exists, then the picker. `status` combines lifecycle, fork-point changes, active work,
leases and watched PR state in one workspace view; the daemon looks PRs up on each request
and keeps a failed lookup on its PR. Human output uses `Display` for enum values and a shared
semantic palette at the CLI presentation layer; machine output and stored values
stay unstyled. The output format resolves once after parsing, from `--json` or
`--text`, then `SHOAL_OUTPUT`, then text, so results, errors and prompts agree;
internal commands name their format, and hooks run without the setting.
Agent launches set `SHOAL_OUTPUT=json` unless the launcher carries a setting,
rather than guessing from the workspace or terminal: people use workspaces
too, agents sometimes have terminals, and piped human output keeps its meaning. Progress during silent waits belongs to the CLI and shows transient elapsed-time feedback on terminal stderr,
suppressed for JSON and dumb terminals. Rust chooses paths, including
`<root_dir>/<repo>` after removal; the Bash/Zsh wrapper changes directory
without evaluating repository code. Completion leaves navigation to prompt recovery:
the shell changes directory only after cleanup removes the current worktree. If
cleanup removes the current directory or a pending navigation destination, shell
integration recovers to its nearest surviving ancestor after the command or at the
next prompt, preserving the command status.
Recovery needs no daemon and is disabled for scoped callers.
Service installation prints the shell initialization hint only while neither
startup file contains an active initialization command.
Confirmations show the action and ask `[y/N]`, cancel on Enter, `n`, EOF or
Ctrl-C (also after a tracked execution), and are bypassed only by
explicit flags such as `-y`/`--yes`. Removal's confirmation names the unsaved
work it deletes and lists uncommitted changes and untracked files with Git
status codes, reporting omitted entries when the preview reaches its size limit;
the daemon refuses unsaved work the confirmation did not cover. A failure whose fix is one
command asks interactive callers `[y/N]` whether to run it and continue;
declining, and noninteractive or JSON calls, fail naming that command. Interactive
navigation without the shell wrapper reports how to load it without changing
redirected or JSON output.

Bare `shoal` is the one place to see what needs the user and the work going
on: a plain fzf picker whose inbox view lists open questions, pending approvals,
agents that need input, failed workspaces and current ready marks, and whose
work view lists workspaces, followed by a view of recent notifications. It opens
on the inbox when that has rows, searching for the current workspace's items
when it has some; Tab reloads the next view in place, and each view binds only the keys that act on
its rows, with a key meaning the same wherever it is bound and its header listing
them by how often each is used. Acting on a row
runs the command that handles its kind. `inbox` and `work` open the same picker
on their view, and print for scripts with `--print`, filters, JSON or without a
terminal. Items that only report status stay out of the inbox, including idle
agents, which may be waiting for PR activity rather than the user.

Completion queries the installed binary per Tab and uses read-only daemon calls
with a 500 ms timeout for live targets, honoring state directory and scope and
never starting a daemon or picker. Targets sort before flags, including in fzf-tab.
Each command states what it needs of its workspace in one table; completion and
pickers leave out a workspace without what the command acts on, and describe one
refused for its state or a running execution by the reason, which pickers list
last.

Named `[commands]` are executable/argument arrays resolved per name from saved
repository config, worktree config, then global config and built-in defaults at
launch. They run through the tracked wrapper with workspace scope, terminal I/O,
and literal extra arguments. `run` lists the resolved arrays with their layers and
provides an explicit spelling for names that collide with built-ins; built-ins win
the bare shorthand. Without a current or explicit workspace, repository-only
commands pick from the workspaces whose configuration defines them. Unknown
names report a command error with suggestions for similar built-ins and never
open a picker; the definition is looked up before the arguments are parsed, so
help and argument errors belong only to defined commands. Workspace fields expand once within individual
arguments; a standalone `{args}` places the caller's literal arguments.
`{diff_base}` lazily uses `diff`'s daemon lookup. Built-in `edit` runs the
`edit` command, otherwise `$VISUAL` or `$EDITOR` with the workspace path, through the same
wrapper, so the editor gets scope and port variables and counts as activity.
Built-in `review` chooses
between the `review` command and an agent prompted to report, not change, the
work. An agent reviewing a forge item posts its findings there as one comment
unless `[review] post` or `--no-post` keeps them local; workspace reviews stay
local. A PR target, named by URL or `--pr`, resolves a same-repository PR with the user's forge login and
reviews its head branch against the PR base, reusing the owning workspace after
fast-forwarding it to the pushed head; local commits ahead stay, divergence is refused.
The daemon fast-forwards under a lifecycle reservation and the repository's Git gate,
after verifying the recorded worktree and its branch, so a switched or replaced
checkout is never moved.
An issue target asks an agent to refine the issue before implementation, in the
workspace `add --issue` would continue, opening it when missing. `repo setup`
likewise prompts an agent, in a workspace on the `shoal-setup` branch, to write the
tracked repository config, so it arrives as a reviewable commit that every clone
shares rather than as saved local config. Review tools own review storage, exports, and forge authentication, with
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
and inserted prompt text remain literal. A provider's instruction, resume and
extra directory arguments are configuration, built in for the tools Shoal ships;
an integration keyed by agent name adds only what configuration cannot express, such as hooks,
trust entries, waking an idle agent or a launch of its own (the Claude and Codex apps, Happy
sessions), so commands never name a tool. Other names add no tool-specific setup.
A `base` makes an entry a launch profile of another tool, run with the
integration its chain of bases ends at, so one tool can run with several
environments, such as a second login, without losing that setup. Its `env` and
`env_remove` apply to its launches after `[env]`; Shoal reads where the agent
keeps its config from them, so they can move that directory but not remove its
variable.
Turn retry defaults to on for Claude and its launch profiles, off for other
tools, and is overridden per tool with `retry`. Backoff is built in, and no
failure a retry can fix has a deadline: an agent waits out an outage or a weekly
quota and then carries on. A reset the agent's proxy learns of from its upstream
holds the retry until it has passed, whichever of the proxy's and the agent's
reports arrives first. Retry scheduling knows no agent: an
integration that can report why a turn failed maps the agent's error to Shoal's failure, and the
daemon persists the retry and asks the idle agent to continue through its
messages, so hook-woken and prompt-woken agents get it alike. A retry is tied to
the reporting execution and cancelled by any new turn, so it never competes with
the user or with another delivery.
The retrying proxy lives in `crates/shoal-proxy`, which knows the wire protocols
but no Shoal code, like `shoal-forge`. Each tracked agent that names one gets its
own, run by its wrapper, so a daemon restart never cuts a stream and the proxy
ends with the agent; Shoal points the agent at it through the integration, which
knows how that agent takes a base URL. All its requests share one outage budget,
so the agent's own retries never multiply the proxy's.

Agent shortcuts use the execution wrapper: by default Codex CLI gets full access without
approvals and Claude runs with its own settings, plus permission for a workspace's
agent to run the Shoal commands that finish its assignment, so completion never
depends on Claude Code's auto-mode check. A workspace's agent also gets the
repository's workspaces directory as a working directory, so it works in the
children it splits there without leaving its sandbox or permission rules; chats
split nothing and get none. Both trust the repository's checkout,
which Claude Code keys every worktree's trust by, and its shared Shoal workspaces
directory, or an explicitly placed workspace outside it, in their user config before
launch, creating the file if needed; Happy
launches and Claude and Codex app handoffs do so too. The user config is the one the agent
reads: a tracked agent's follows the workspace's `[env]`, while the app and chats
run with Shoal's environment. Agent guidance and templates become native
CLI instructions or a first-message prefix for Happy Codex; desktop handoffs carry no instructions.
Claude's and Codex's default modes are config values read at launch; `--cli` and `--app`
override them. The Claude app opens through the `claude://` link that `claude --desktop`
opens, since that command refuses a CLI signed in through an API token or gateway.
Successful repository removal drops those trust fields for the removed workspace
directory and worktrees, in Shoal's user config and in each one its workspaces'
`[env]` gave a tracked agent, while preserving other project settings. The daemon
keeps each location from when the CLI trusts a workspace there, since a renamed or
removed workspace, or a changed `[env]`, no longer resolves to it. The CLI reads
the locations once removal has succeeded, when no agent can add one, and the
daemon forgets them after cleanup.

Issue-based workspace opening requires an open issue and persists its canonical
URL before tracked setup or agent launch. Reopening first follows the issue
association, then an existing local branch with the derived name, rather than
suffixing a new branch; conflicts fail before the agent picker. Associations are idempotent and cannot
be replaced. Status and inspection expose them.
The derived name is the issue number and at most three title words, short
enough to read in full in tabs, session names and branch lists.

`add <link>` recognizes issue, PR and branch URLs and selects their registered
repository by origin identity, offering interactive registration when missing.
Issue links and numbers invoke issue opening with `default_agent` standing in
for `--agent`; numbers use `--repo`, the current checkout/workspace, then a picker.
PR links open their head against the refreshed remote base and link the PR once
the workspace is ready, as issue links associate their issue; branch links open
an origin branch. Both use existing-branch ownership checks and retain an owned
workspace's diff base. Opening fork PR branches is refused. Interactive `add`
without a branch, base or issue also offers the repository's open issues and PRs,
which open as their links do.
Interactive `add` uses an agent picker when no agent is named; non-interactive
additions launch only when an agent is named. `add --agent` launches only after
creation, setup, and the post-setup hook
succeed, or after an explicitly ignored setup failure, and retains the
workspace whatever the agent does. Desktop handoffs (Claude and Codex apps, T3) provide no
tracking or scope; external hosts can hold a workspace while their session is open.
Happy sessions are an optional integration behind a default Cargo feature and
the `happy.enabled` setting, off by default, so its encrypted client stays out of
core. They are the phone-driven flow: a console session that
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
the item's kind, canonical URL and relation so integrations can follow links without
polling workspace inspection.
Item selectors make the `pr`/`issue` word optional for numbers: a linked
item with that number decides the kind, otherwise the forge's issue read where
issues and PRs share one sequence. Where they do not, an unlinked number stays
ambiguous and needs its kind rather than a guess between two reads.
Ready workspaces with linked issues or PRs are polled every 30 seconds; the
daemon keeps a separate activity snapshot for each item and appends an
`item_activity` event for each newly observed comment, completed check,
conflict, closure, merge, reopen, or lookup failure. The first observation
establishes the baseline, and watch acknowledgements do not affect this stream.
The same poll queues each change on the workspace's watch cursor and wakes a
running watch, so an agent receives activity without watching for it. Polls
read through the forge crate's shared request layer, which remembers each
account's last response per URL in daemon memory and repeats the read as a
conditional request, so an unchanged item costs a `304 Not Modified`, which
GitHub does not count against its primary rate limit. The memory is not
persisted: a restart reads everything once in full. The same layer reads each
forge's rate-limit headers and sends nothing to a forge that limits the account
until the limit ends; polls wait for it and the user gets one notification,
because a lookup failure would ask an agent to act on what only time fixes. A
429 is such a limit, never resent within seconds as a 503 is.

Notifications stay in the terminal: the daemon records what a user would
otherwise miss (busy resources and who holds them, port conflicts, exits of
shortcut-launched agents, workspaces it removed or retained on its own, messages
a workspace's agent sends to ask for attention) and the
CLI shows them once, unless asked to keep them unread, on request or as a followed stream that also raises the
terminal's own notifications (OSC 9); `work`, workspace `status`, and `daemon status` only count them. Recording never fails the operation it describes,
repeated polled conflicts and identical messages collapse until read, and scoped
processes cannot read them. Agent messages are separate from completion: telling
the user a PR is ready neither ends the assignment nor affects cleanup. The
sending workspace may withdraw its message while it is unread; once shown, it
stays, because deleting it would not unsay it, and a correction does. `post_agent_exit_cmd` exposes tracked-agent exit notifications to user
integrations while the workspace is ready, using normal configuration precedence
and daemon hook rules with the agent name, reported code, and process-completion
status. Failure notifies without changing the exit result or marking completion;
removal uses its own hooks. Desktop or push delivery was considered and not adopted.

Messages to agents travel the other way: the user queues them for a workspace,
and the daemon holds them until an agent in that workspace reads them, then
deletes them. Unlike notifications, which a terminal notification shows as one
line, they may span lines, because instructions to an agent often need several.
Agents read messages through Shoal commands, and a pending watch
ends for a new message. Messages and item activity are pushed rather than polled by the agent,
because agents forget or misuse watches: built-in agent launches register a hook
that delivers both after each tool call and prompt, the only point where a
working agent accepts new context, except a subagent's tool call, whose context
the agent never sees, and replaces a running watch so nothing is
delivered twice. The hook never waits for cleanup or an item change, which can
hold Shoal's item records for minutes: an agent discards the output of a hook
that outlasts its timeout, so the activity waits for a later delivery. Claude Code also runs a background hook, after a turn ends normally
(`Stop`) or on an API error (`StopFailure`), that waits for the next update and
wakes the idle agent with it through `asyncRewake`. The hook
tries a failed wait again instead of exiting, because a hook that ends leaves the
idle agent with no wait until its next turn; waits outlast a daemon restart,
including the time before the next daemon listens and knows the agent's scope. Codex
has no hook that can start a turn, so its tracked wrapper types a fixed prompt
into the zmx session it runs in once Codex is idle with updates, at most once
per idle stretch, and the prompt's hook delivers them. That is the only input
Shoal writes to an agent's terminal: the daemon never does, the prompt carries
no forge text, and it is appended to anything typed but not yet sent. Shoal does not
pre-trust hooks where the agent requires review. Identical unread messages collapse.
`messages` reads new messages while `messages send` sends them. Scoped
processes may read their own workspace's messages and send messages
only to the agents of the workspaces below their own, which they coordinate.

`shoal questions` opens a question picker on a terminal. A numbered question
shows its text, workspace, recommendation, reason and numbered choices, then an
`Answer` prompt reads a choice number or written answer. Enter accepts the
recommendation shown in brackets literally, even when it is numeric; Ctrl-C or end
of input leaves the question open. Withdrawal belongs to the requester, outside the
answer prompt. The inbox uses these same
actions without changing its listing.

Questions are how an agent asks the user for a decision whose answer is text,
separate from approvals, whose answer is yes or no. A question states the
decision and the agent's recommendation, so most answers are a word or a choice
number. Only the user answers, or the agent coordinating the asking workspace
from an ancestor, and the answer travels as an agent message naming
the question, so the same hooks deliver it and wake an idle agent; an agent that
cannot continue without it waits on the question instead. The answer and its
message are recorded together, and a wait that prints the answer delivers the
message, so the agent sees it once. Re-asking an identical question returns it
until its answer is delivered, which makes an interrupted wait safe to rerun.
Delivery deletes the question: the agent's transcript keeps the exchange, and
nothing in Shoal reads it again. An open question means an agent is waiting on the
user or its coordinator, so it keeps the workspace from idle cleanup; questions belong to the
workspace and go with it. Marking the assignment done closes its open questions,
since the finished work waits on no answer and a question it made pointless would
otherwise keep the workspace until the user answers. An agent that no longer needs
an answer withdraws its question, which closes it the same way and takes back its
notification while the user has not seen it, so the user is not asked to decide
what no longer matters. Withdrawing completion does not reopen them, and a closed
question keeps its number until the workspace goes, so a late answer is refused
instead of reaching a later question.

Questions and approval requests are named by the smallest number no stored one
holds, because the user types them; their records keep a separate internal key
that is never shown. A wait follows its question's number and text, so a later
question reusing the number cannot answer it, and a keep or land request that no
longer applies stays as outdated until the workspace goes, so its number is never
reused and a decision meant for it cannot approve a newer commit. An agent may
withdraw a pending request it no longer needs, as it may a question: it stays as
withdrawn on the same terms, except a proposed issue, which would outlive the
workspace and goes as a decided proposal does.

Shoal owns forge writes for its workspaces so that every client, from an agent
in a terminal to an app built on Shoal, gets one item model, one account rule
and one policy. Issue and PR actions are typed (edit, comment, close, reopen,
merge) and planned per forge as REST requests: `gh api` and `glab api`, which
own GitHub's and GitLab's logins, or curl with the token fj saved for a Forgejo
host, since fj has no API passthrough, or the configured Bitbucket token. Gitea shares Forgejo's API; a host fj has no login for uses tea's
login through `tea api`, which owns that token, and otherwise reads anonymously.
The forge is selected from the remote's host without requiring configuration:
well-known hosts by name, and any other by asking it once through each forge's
version or metadata endpoint with the user's login for that host, since a name
cannot tell a GitHub Enterprise Server or a self-hosted GitLab from Forgejo.
Asking leaves out the tokens gh and glab read from the environment for any
host, so a host that may be another forge never receives them. The daemon records
each answer in its state and asks recorded hosts again when it starts or
reloads its config, recording a changed answer with an alert. A host it cannot reach then
keeps its answer and raises no alert, since a machine may start offline; a global `[forges."<host>"] type`
pins one instead and is never asked, and a host that answers as none fails with a message naming that
setting. Each process remembers the answers it resolved, so links of the host
parse the same way, and the CLI starts out knowing the pinned ones. A pasted
link of a host not yet resolved is classified without a request by the forge
its routes name, such as `/pull/` for GitHub
and `/-/merge_requests/` for GitLab; only an issue or repository URL, whose
form several forges share, falls back to the name: GitLab for a `gitlab` name
label or a glab login, and otherwise Forgejo. Host types describe hosts rather than
repositories, so they are global only, like `[bitbucket]`. GitLab-only concepts such as approval rules and merge trains
stay unmodeled; approvals count as approving reviews. Bitbucket Cloud has no CLI
to own a login, so a user-configured command supplies its token per lookup, as
`[secrets]` commands do, and Shoal still stores no token; agents act with the
user's Bitbucket credentials until a separate agent token is configurable. Forge differences stay in planning, such as Forgejo keeping draft
state in the title, so callers see the same item either way. Each backend
declares the capabilities it has, and a request needing one it lacks is refused
before anything is sent with one typed error naming the forge and the
capability, so a backend gains a capability without changes elsewhere;
`shoal forge capabilities` reports them so agents can check first. Actions run in the
daemon, where scope applies: they act as the workspace's agent account, and
merging stays the user's decision, so scoped callers cannot merge. Review
threads are named by any of their comments' IDs. Listing issues and PRs also
runs in the daemon, so a scoped caller's search goes out as the agent account
and anyone else's as the user's own logins; it uses each forge's own search and
reads pages only until the limit is met. `shoal forge api` passes any request under
the repository's forge API root through the daemon the same way, for reads no
command models, since fj has no API passthrough to fall back to. The daemon
cannot tell what another method changes, so a scoped caller sends only GET
unless `[api] agent_writes` accepts that its writes may merge, approve or skip
required labels; the path stays under the API root, so the login reaches only
its own forge's API. Forgejo's issue search lists PRs
without their branches, so each listed PR is read in full, and GitLab keeps
merged merge requests apart from closed ones, so listing closed ones reads all
and drops the open ones. Shoal does not sign in to a
forge's web UI, so it cannot resolve threads on Forgejo, whose API has no
endpoint for it. Resolving a thread nobody has answered needs confirmation,
because the reply tells the reviewer why. Attached files are read
by the daemon from paths the CLI checked, anywhere on disk: a body file can
already come from anywhere and scope is cooperative, while recordings kept in the
worktree would be untracked files that hold up cleanup. GitHub's REST API has no
upload endpoint, so Shoal uses the one gh itself uses, through `gh api`. Forgejo
takes a comment's files only once it is posted, so a failure there would leave it
without them: Shoal refuses what the instance's attachment settings refuse before
posting, detecting a file's media type from its leading bytes as Forgejo does, and resends the uploads and the comment's update after failures that may
pass. An upload is resent only when the forge did not get it or answered 503,
since a second copy would stay on the comment and a proxy's other 5xx may follow a
stored one; the update also after those. A comment whose files still fail stays posted, neither deleted
nor finished later, and its error says so. Each action
returns the item as the forge reports it afterwards and records an
`item_changed` event for integrations. A new issue is recorded as opened and
assigned only on request: a workspace's assigned issue is its work, while an issue
an agent opens is usually follow-up work. Browsing across repositories and review
tooling stay with clients.

`shoal pr open` pushes to the branch's push remote, then finds or opens the branch's
PR and assigns it in one daemon request; a push remote on the origin's forge host
other than `origin` is a fork the PR comes from, so a PR Shoal opens is left unlinked only when removal started
after its check, which the result reports as a warning; a PR it creates is also
recorded as opened. A new PR is ready
unless `--draft` or `[pr] draft` asks for one, because opening a ready PR is
what requests review.
An explicitly supplied description is preserved verbatim; when no description
is supplied, a linked issue is referenced with `Closes #<number>`.
Repository conventions reach agents where they open and mark items, not only in
instructions read at launch: `shoal repo labels` serves the forge's label scheme with
its descriptions, repositories may require label prefixes, which refuse a new item
before it exists, and missing labels or template headings come back as warnings
that never fail a change that already happened. Shoal never fills a template in,
because an unfilled template says less than a closing reference.
Its options are modeled rather than passed through to a CLI, so they mean the
same on every forge.

Ready-for-review marks are the agent's status signal for integrations, separate
from messages and completion: they never notify, complete, or change cleanup. A
mark covers a linked issue or PR, or the workspace when nothing is linked, and
binds to HEAD; new commits make it outdated rather than removing it, so marking
again is an explicit statement about the new revision. Marking linked items
replaces a workspace mark. Unlinking an item
withdraws its mark and removal deletes all marks. Marks and withdrawals are
journal events, and `post_ready_cmd` runs after each explicit mark under the
completion hook rules; failure notifies and keeps the marks. Marking a linked
draft PR also takes it out of draft unless `[ready] undraft = false`, since
forges and review bots often skip drafts. Withdrawing a mark leaves the PR
ready unless `--draft` or `[unready] draft` asks to return it to draft, because
an agent usually withdraws to push more work that still needs CI and review.
A failed forge change fails the command with the marks kept, so the agent can
retry it.

Agent state is the turn state a workspace's agent reports from its hooks:
`working`, `waiting` for the user mid-turn, or `idle` before its first turn
and after each. Lists name them by what the user does: `working`, `needs input`,
and `waiting`, since an idle agent waits for the user or for activity that wakes
it; a ready workspace whose agent or command reports no state is `process running`,
one where nothing runs `inactive`, a current ready mark `ready`, and a recorded
completion `finished`, which only stopped work and an agent needing input
outrank. A row's marker groups statuses by what they ask of the user, in a
shape that reads without color. Like
ready marks it is a status signal for integrations that never notifies,
completes, or affects idle cleanup; overload protection reads it only to stop
idle agents first. A workspace keeps the
latest report; a report from a tracked execution shows only while that
execution runs and ends with its record, while one from outside lasts until
replaced or removal. Changes and
endings are journal events; repeating the current state is not a change and
keeps its start time. Shoal's terminal Claude Code and Codex launches, resumed
ones included, add the state hooks to the launch option that carries their
message hooks (Happy sessions do not), so the agents' own configuration stays the
user's; an interrupted Claude Code turn has no hook and ends with Claude Code's
idle-prompt notification, while an interrupted Codex turn stays working until
its next turn ends.

A chat is a conversation with an agent about a repository, not an assignment,
so it runs in the registered checkout with the user's authority instead of in a
workspace: coordinating work is one thing a chat may do, and scoped sessions
cannot create workspaces. It refreshes the default branch first, as workspace
creation does, so the discussion starts from the remote's work; a failed
refresh only warns, so an offline or diverged checkout still gets its chat. Its
instructions describe the discussion only, from a chat template that
repositories can extend like the other prompt templates; the orchestrator skill is available when the user asks for coordination. Belonging
to no workspace, a chat is not a tracked execution: queuing, overload stops and
resume serve unattended agents, while the user attends a chat. It is still a
daemon record, so lists and integrations can follow it: the agent replaces Shoal's
process, leaving no wrapper to report its exit, so the record lasts while a
process with the registered identity lives, checked periodically and before
chats are listed. Starts and ends are journal events. A workspace belongs to the
chat that last opened it until that chat ends, and opening it outside a chat
returns it to the user, so Shoal knows which chat coordinates which workers.
Having no workspace, a chat keeps its messages in its own queue, which the same
hooks deliver; a chat that ends gives the user the notifications its unread
messages stand for, and its workers return to the user even if it is resumed.
A chat passes a question up by asking the user, who attends it. Holding no
grant, it receives no approval requests, though with the user's authority it
may decide them. A repository may have several chats, so each call starts a new
one and focuses it. A chat has a name from the same namespace as workspaces,
since both are places the user returns to by name, and its name frees when it
ends. The process the user runs chooses the name, so the tab or session that
hosts the chat carries it before the chat is recorded; the recorded chat then
labels its zmx session with its ID, so `shoal attach` finds the session by the
chat's record, not by a session name zmx may have suffixed. A chat waits for the user to speak first unless it opens on a
task: an agent that takes instructions apart from the first message gets them
so, and another gets them as its first message, which starts a turn and so asks
only for a short reply or carries the task.
A chat's launcher runs where only the checkout's path has a value, so one that
uses another workspace placeholder is refused rather than given the literal text.
A Happy chat has no terminal to focus: like a workspace Happy session it starts
detached, as Happy's daemon would start it, so the user continues it in the
Happy app, and the process that recorded the chat hands the record to the
session's process.

Issues split by whether their plan is settled: `[issue] plan_label` marks one
that needs the owner's choices and `planned_label` one whose body holds a plan
an agent implements without asking. Both are repository configuration, so Shoal
assumes no label scheme, and issues with neither stay outside the flow.
`shoal plan` settles a plan in a chat, not a workspace: planning changes no
code, needs the user's attention and their authority to rewrite the issue, and
the chat then coordinates the implementing workspace it opens. The plan goes
into the issue body because the body is all an implementing agent's prompt
carries. The planning task is built in, like an issue review's, since a
repository's planning conventions belong in its own contributor rules, and it
keeps the discussion in the conversation: questions and forge comments serve
agents blocked during implementation.

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
environment. Debug builds are development builds that share the user's home
with the installed Shoal, so they change no user-level files outside their
state directory, skills included, unless tests in a temporary home set
`SHOAL_DEV_USER_FILES=1`. A changed or removed skill stays as the user left it until an
explicit install, which restores removed skills, asks about changed ones in a
terminal like a package manager's modified configuration files, and with
`--force` replaces them. Global `[ai.<name>]` settings name skill directories, with defaults for built-in providers. Skill directories describe
the machine and cannot be set per repository; provider launchers join named
commands without tool-specific integrations. Packaged skills resolve the runtime
`SHOAL_SKILLS_DIR`, build-time directory, then an adjacent `shoal-skills` symlink
to a directory holding every bundled skill. Explicit paths must be absolute;
relative link targets resolve lexically against the link's directory. Preserve
stable installation prefixes so upgrades apply; unpackaged binaries install the
embedded skills. Installed `SKILL.md` files are regular copies, because Codex
skips linked ones; refresh replaces links earlier versions recorded. Homebrew launches the binary directly, without a shell.

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
append them to global guidance, using the bundled chat template when
no global one is configured. Empty repository values suppress only the addition.
Repository-root Markdown files and global files beside `config.toml` sit below
inline TOML values at each level; provenance names the highest configured layer
and reports the combined text.
Shoal's agent guidance is built in and precedes every worker template, so
adding instructions never freezes or replaces it. It holds only what the launch
knows (the workspace, branch, path and how activity arrives), the rules an
agent needs before it loads the `shoal-worker` skill, and a demand to load it
before any other action, since agents otherwise load it only once they judge a
Shoal command needs it. The skill owns how to work with Shoal, so the two are
never kept in step by hand; `install` adds a missing
chat template from the bundled default and never touches existing files;
rendering substitutes known fields once without evaluating their contents.
The issue prompt is built in and states only the task, pointing to `shoal view`
for the rest: a seeded issue template froze the default for whoever had a copy,
and instructions for issue work belong in the worker template. That template was
the agent template, a name that read as one per AI tool although it applies to
every workspace agent. A retired or renamed setting still loads; `config show`
warns about it in every layer, and the daemon alerts about one in the global
config when it starts or reloads its config.
Before a workspace exists, the registered checkout's file stands in for the
worktree's. Repository config is read per request, so changes need no restart
and leave existing leases alone. Repository config cannot expand machine
policy: `root_dir`, simulator limits and profiles, Git profile definitions, and global resource
definitions stay global; global pools span repositories, repository pools span
that repository's worktrees.
The repository `[env]` table layers by variable name and is rendered once when a
workspace execution or hook starts. It accepts workspace, path, repository, branch,
and reserved-port fields; values are literal strings after substitution, and
`SHOAL_*` names remain owned by Shoal.

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

Busy resource and simulator requests may queue. The daemon keeps queued lease names
in arrival order per pool, and one queue for simulators; while any remain, only the
earliest may claim new capacity, so every other request is busy even when capacity
is free. Strict order lets a request for a busy member or a writer hold back requests
that could proceed, so no request starves. Renewals and approval requests ignore the
queue; grants, approval outcomes and errors give up the place. Queued commands poll,
and a place lasts while a request is in progress or polled within 30 seconds. The
queue lives in memory, so a daemon restart loses the order; commands still polling
join again.

Approvals are the daemon-owned way a scoped caller asks the user for a yes or no;
`shoal approvals` opens a request picker on a terminal, and a numbered request
shows its details and prompts for a decision. Ctrl-A approves and Ctrl-D denies
the selected request directly from the list. Decisions bind to the displayed or
authorized record through mutation, including when a coordinating agent decides. Explicit `--approve` and `--deny` flags decide every kind: landing
a branch (see scope), resource access, and issues and PRs an agent proposed.

In a shared repository everything an agent opens is seen by other people, so
`[approvals] issue_open` and `pr_open` can make a scoped `issue open` or `pr open`
propose instead of open (`"direct"` is the default; unscoped callers always open).
A proposal is an approval request whose record carries the command's options with
their defaults resolved, the workspace, branch, commit and agent, and copies of its
attachments in the state directory until no pending proposal uses them; the command
reports it and exits 0, because the agent goes on with its work and hears the
decision as a message. Only the user decides a proposal: publishing to a forge
other people see is the review the setting asks for, so proposals are never routed
to a coordinating agent. Approval carries data: the user may replace the title and
body, and the daemon then opens the item as the agent account through the same path
`issue open` and `pr open` use, recording and linking it. Denial may carry a reason,
and an issue proposal may be denied as a duplicate of an existing issue, which gets
its text as a comment, or of another pending proposal, which posts it once opened.
A decided proposal is deleted, since the message carries the outcome. An issue
proposal belongs to the repository rather than the workspace, because agents file
follow-ups just before `done`; it opens through the repository's checkout and agent
account once the workspace is gone. A PR proposal is bound to the branch and its
commit like a land request: the push still happens, and a renamed branch or another
commit, locally or where the push went, outdates it at approval or when the agent
proposes again, and a pending one keeps the workspace
from unattended removal. Holding the push back too is #922; other agent writes,
such as comments, edits and closing, stay direct. Approving a land
request whose branch moved since it was requested is refused as stale and marks it outdated.
Agents also find problems that belong to another repository, such as a tool the
project uses, so `issue open --repo` files an issue there, on any forge, as the
agent account; every other item action stays within the workspace's repository.
People outside the project see such an issue, so `[issue] other_repositories`
governs scoped callers separately from `issue_open`: `"propose"` by default,
`"direct"`, or `"deny"`. The workspace's required labels and templates describe
its own repository and do not apply there, and the issue cannot be linked, since
an assignment's cleanup follows the workspace's repository. A proposal records
the repository it opens in.
Configured resources may require human approval for scoped acquisition. The daemon
persists a request with a caller-supplied reason and the effective allocation settings;
only an unscoped caller decides it, or the nearest coordinating agent above the
requesting child workspace that holds an approved grant for the same target and
settings, which may pass it up to the next such coordinator or the user; the
coordinators passed over are told so. The decision is sent to the requesting
workspace's agents as a message so they need not poll. Pending requests consume no capacity. Approval
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
to every lease of that kind or in the workspace. Holds use the same verbs but
keep a workspace rather than share capacity; releasing leases without a kind
preserves holds. `leases` shows leases and the usage that limits new ones;
`catalog [kind]` shows the configuration and machine inventory they draw from,
secrets without their commands; `history <kind>` shows a kind's audit trail,
starting with clean-device requests. There is no separate `sim` group.
All-workspace overviews that need independent per-workspace reads use bounded
concurrency, retain workspace order and report every failure.

## Removal and recovery

External sessions acquire caller-named workspace holds independently of assignment
completion. Holds are idempotent by name and persist across restarts. They block
automatic removal while the worktree exists; releasing the last hold restores
normal cleanup eligibility. Explicit
removal lists holders and releases holds with the workspace record; deleted-worktree
cleanup still forgets missing worktrees. Release remains available through failed or
active lifecycle operations and hooks. Holds belong to the user and to apps hosting
external sessions; scoped callers list their own workspace's holds but cannot change
them. Removal is reversible through checkpoints, so agents have no way to keep a
workspace: one that needs the user asks, and an open question, like a hold, keeps the
workspace from every unattended removal, completion and PR cleanup included. So
does a child workspace, until the child is removed, and an agent launch waiting
in the agent queue to start in the workspace.

Workspace completion uses `[done] cleanup` (default true), resolved through the
normal configuration layers, and an explicit cleanup override. Only an
explicit done signal completes an assignment; issue closure and merges do not,
and merged cleanup removes unused merged workspaces from their state instead.
Agent instructions make `done` the last step of every assignment. Completion
is persisted separately from lifecycle readiness, binds to HEAD, and notifies the
user; it does not assert that work was merged. A `done` that would change
nothing and an `undone` without a completion fail, so a misplaced signal is
reported instead of passing silently. Own-workspace withdrawal removes a
recorded completion and preserves associations and merge requirements, but never
defers cleanup: only holds, open questions, a pending PR proposal, child workspaces
and queued agent launches keep a workspace.
The daemon runs `post_done_cmd`
after persisting completion and before cleanup, using normal
config precedence and the worktree's hook identity plus the keep/cleanup choice.
It is bounded, excludes lifecycle and permit changes, and reports failure through
a notification without undoing completion or blocking cleanup. Explicit signals
rerun it; events are not queued or replayed on restart. Completion/PR serialization
is per workspace: cleanup sweeps hold a workspace's gate only while they handle it,
including its forge lookups and removal, so commands for one workspace never wait
for another's. It covers the hook, which must not call gated Shoal mutations for its
workspace. Completion without a
hook takes no resource guard, so an agent-exit hook may signal it; configured
completion hooks cannot nest inside another guarded hook. A completion without
cleanup suppresses idle and PR cleanup until another completion requests cleanup or
the user removes it; merged cleanup still applies. Completion
cleanup without a PR registration requires clean files and every commit pushed or
merged; an existing PR registration keeps its merge checks.
The daemon stops tracked executions through shared removal and rechecks files and
HEAD, including after hooks. Tracked agent exits wake the cleanup sweep after
their exit hook, without waiting for the polling interval. Changed HEAD retains
the workspace until a new completion signal; failed cleanup retains ownership
and reports why.
Issue linking and unlinking are own-workspace operations; associations are idempotent
and replacing one requires unlinking it first. Unlinking preserves recorded completion.
Issue associations suppress idle cleanup, not merged cleanup.
Item views let agents read issues and PRs without knowing the forge. The daemon
selects linked or explicit items; the CLI looks them up with the user's forge
login, because discussion can exceed a protocol frame: `gh` for GitHub, and
Forgejo's API with fj's saved token for the host, or anonymously without one,
because fj has no structured output. Views automatically include the logs of
failed CI jobs where the forge serves them, fetched by the daemon with the
workspace's agent account and returned as bounded tails with the job's URL, so
scoped callers never need forge credentials. Successful and pending checks omit
logs, and a check that links no CI job in the repository has none to read;
explicit selection limits output to one failed check.
Failed-check watch updates include its failing step when recorded by the runner; an unavailable
log is reported on that update without hiding other activity.
Agents can also rerun failed and cancel running CI jobs on a PR's current head
through the daemon, as the agent account and under the same scope, with each
action recorded as an `item_changed` event. Each forge's own job API carries them
out; Forgejo's API cannot rerun jobs and Shoal does not sign in to its web UI, so
Forgejo only cancels, and Shoal acts on no Bitbucket Pipelines job.
Agents may also start a workflow run on their own workspace's branch, on every
forge including Bitbucket. A started run is not a rerun of a PR's checks: on
Forgejo it has no head commit or event, so it cannot replace the rerun Forgejo
lacks, while elsewhere only a run on the PR's head commit reports checks there.
`forge ci` also reruns and cancels one run by its URL; `pr ci` stays the way to act
on a PR's checks. Workspace processes act only on runs of their own branch, which
the daemon reads from the run before acting, and read any run of their repository.
Viewing a run reads every page of its jobs; the daemon sends them, and listed
runs, in frame-sized pages, and each failed job's log, named by job ID, as one
request, like a PR's, so no response outgrows a protocol frame.

Manual and automatic cleanup share one path: establish ownership, stop owned
executions, saving an existing worktree's agent sessions but no commands, run
removal hooks, remove owned simulators, take a checkpoint, remove the worktree, and release leases with the record. Failures retain what is
needed to retry. Workspaces are cheap to remove because removal is reversible.
A checkpoint is a commit of HEAD plus the unsaved files Git does not ignore, built
with a temporary index and kept under `refs/shoal/checkpoints/<commit>`, with a
record in the store. A ref is named by its commit, so it belongs to whichever
records keep that commit and moving checkpoints to another workspace changes
only records; a ref no record keeps is deleted. The ref is written before its
record, so a record never names an unreachable commit, and the pair excludes
expiry, never under the repository's Git gate, which removal holds. An agent's
checkpoint is taken only once the processes it left are proven stopped, and a
restore excludes link changes like other lifecycle operations. Checkpoints are taken on request, when an agent exits or is stopped
(while the workspace is ready or stopping), before a restore to a checkpoint, and
by removal; all but requested ones extend the newest checkpoint instead when they
would keep the same branch, HEAD and files. Unsaved files beyond
`[cleanup.restore] archive_max_mb` are not kept, leaving HEAD only. A snapshot
fails when HEAD or the branch moves while it is taken, so its record's HEAD
always matches its files. Checkpoints
expire after `retention_days`, and so do those no workspace or removal record owns.
Restoring a checkpoint reserves the `restoring` lifecycle state, which excludes
other lifecycle operations and new executions, refuses while another execution is
recorded or the checkpoint's HEAD or files collide with ignored ones, keeps the current state in
a checkpoint and stops when that one keeps HEAD only, then cleans untracked files under the current ignore rules, resets
the branch and applies the snapshot's files without staging them. Scoped
callers may do this in their own workspace, since the replaced state stays a
checkpoint.
Removal's record (base, links and stopped agents' recovery records, with its
checkpoint) persists before any file is deleted, so an interrupted or failed
removal stays restorable; records of workspaces that still exist are not offered,
and a retried removal replaces the record with a new checkpoint while the earlier
one stays. When the removal's checkpoint keeps HEAD only, merged cleanup retains
the workspace. Records expire with their checkpoint. `shoal work restore`
reopens the branch through the existing-branch path, creating it at the removed
HEAD when deleted and refusing one that no longer contains it, then applies the
snapshot's files without staging them, links what the record names and saves its
agents' recovery records under the new workspace ID for `shoal resume`; once the
files are back, one transaction moves the checkpoints to that ID and deletes the
record. A restore first claims the record, keeping it and its checkpoints from
expiring for an hour while it runs. Reopening is
allocation, so scoped callers cannot.
Moving a workspace to another machine is a removal there and a restore here:
export writes the removal record and a Git bundle of its checkpoint from the
daemon, which outlives a terminal closing with the workspace and checks the
destination before removing anything. Import records both as a removal of this
repository, the ref before the records, then runs the normal restore. Moving rather than copying keeps one workspace per
branch, so two agents never push it or watch its PRs, and the source's removal
stays restorable when the transfer fails. The bundle leaves out the default
remote's default branch, which the importer fetches when it lacks the commit the
bundle builds on, since that branch only advances. Repositories match by origin.
Exporting every workspace of a repository checks each one before removing any, so
one that cannot leave refuses the set while all stay; a failure once removals
begin leaves the exported ones moved and the rest in place, each reported.
Agent conversations are the agent tools' private files, and Shoal neither
stores nor touches them. The export records the agents' sessions, with the
exporting machine's ID when it has one, beside the moved records, which older
importers read. An import on that machine, or one told to keep sessions, keeps
the session IDs, which the agents resume from any directory, dropping another
machine's terminal places and waiting wrappers, and every reopen trusts the
workspace's path for those agents, since resuming skips the launch's trust
setup. Otherwise moved recovery records drop their session, place and wrapper,
and `shoal resume` starts them through the fresh-agent launch, with its trust
setup and guidance, told the workspace moved. Shoal provides only these
operations; deciding when and where to move work
belongs to a caller outside the core.
A branch is merged when the local default branch or its remote-tracking branch
records HEAD's tree, contains HEAD, takes HEAD's merge without changing its tree
(squash merges), or has an equivalent patch for every commit since the fork point
(rebase merges); merged commits count as retained. Every removal deletes a branch
whose commits a remote-tracking branch or the default branch all have, since
deleting it loses nothing, and keeps any other: users should not have to decide
about branches. The default branch, and every branch while the default branch
cannot be resolved, is retained. Manual removal asks only before deleting
uncommitted or untracked files or commits on a detached HEAD, which no branch keeps.
Shoal remembers each branch a removal kept, at the commit removal left it on, and
the cleanup sweep deletes it once a remote or the default branch has all its
commits, without notifying anyone. At most every ten minutes it fetches the
default branch and asks every remote which branches it has; only those answers
count, never remote-tracking refs, which a narrowed fetch mapping or a deleted
remote branch can leave stale. A kept branch that moved or went away, is the
default branch or belongs to a workspace again is forgotten; one checked out, or
the start or target of a rebase or bisect elsewhere, waits. Removal records name the branches kept before Shoal remembered them.
`shoal cleanup` deletes the kept branches the user picks, several at once, after
checking each is still at the commit removal left it on and not checked out.
Automatic cleanup removes only clean, idle worktrees whose commits are all on a
remote or merged, with no executions, directory users, leases, or
permits, rechecked immediately before deletion, without fetching. Every unattended
removal except for disk space first waits `[cleanup] grace_minutes`, because
removal stops the agent the user may still want to continue: the first attempt
that passes removal's initial checks persists an end time, each sweep pass claims
it again while some rule still removes the workspace, and the pass ends periods
no attempt claimed, so they restart instead of carrying over to a later eligibility.
Idle cleanup keeps its timers in memory, so a restart starts its grace period over
too: resuming would need the pre-restart snapshot to prove nothing changed. User requests and disk pressure remove at once. Merged cleanup
(`[cleanup.merged]`, default 240 idle minutes) decides from the workspace's state
instead of its assignment: a merged branch, unsaved files only as leftovers the
removal's checkpoint keeps (the branch has commits of its own), no Git operation under
way, no open question, nothing running, no leases, permits, directory users,
stacked or child workspaces or holds, and no recorded activity, file change or per-worktree Git
change for the idle period. Idle time is persisted, so restarts do not reset it;
it decides only after fetching the default branch's remote, at most every ten
minutes and never moving local branches, so a failed fetch keeps the workspace. A link, a watch or a recorded `done` does not keep the
workspace. Idle cleanup retains saved agents and stopped commands; merged cleanup
drops queued restores and keeps the agents in removal's record for restore.
`shoal cleanup` is the one command for what Shoal left behind: the
candidates of both rules, without their idle periods and including where they are
disabled, kept branches, and shared caches. It lists them with nothing selected,
since deleting a kept branch loses its commits, and removes what the user picks;
naming a kind acts on that kind alone. It is unavailable to scoped callers. `repo rm`
deletes the checkout and every workspace through that path, refuses external
worktrees and dangerous paths, persists progress, and blocks new workspaces until
an interrupted removal is retried.
PR cleanup is separately enabled by default; disabling it pauses PR completion
without affecting links, which serve more than cleanup. Persisted watches use the
user's gh/fj login, resolve numbers against the workspace's origin into stored
URLs bound to that repository. Top-level `link` and `unlink` manage associations
and always name a relation, so no link becomes the assignment by default. `assigned`
items are the work the workspace exists for and drive completion and cleanup;
`opened` and `related` items are records for integrations without lifecycle effect,
and Shoal records `opened` itself for the issues and PRs it creates. Unlinking
selects a relation's items, one kind of them, or one item. PR links accumulate
without duplicates and require
every watched PR to merge, with HEAD present in at least one or merged into the freshly
fetched default branch, as a rebase after the merge leaves it; the existing branch
checks apply to each PR. PRs are looked up only once the assignment is done, and
confirmation proceeds to cleanup. The confirmed
HEAD persists so restart cannot query the same watch set again. Persisted manual
acknowledgement binds to exactly the recorded HEAD. Registrations distinguish watches from
acknowledgements; legacy single-watch records retain their stored and JSON shape.
Ambiguous records and conflicting actions are rejected. Own-workspace `watch`
polls linked issue and PR activity independently of cleanup, with kind filters
or an explicit item that does not change associations. It reports comments or
reviews, completed CI checks, new merge conflicts, closure, reopening or merging.
A conflict belongs to the head and base commits it was seen at, so a push or base
change that leaves the item conflicting is reported again. A forge's conflict is
reported once two polls in a row see it at the same commits, because Forgejo shows
the previous result until it rechecks and a PR it is still checking as not
mergeable. An
unfiltered watch without a linked PR runs the `conflicts` check on each poll as a
cursor item named by the branch, so work without a PR still learns of conflicts; a
linked PR leaves them to the forge, avoiding duplicate reports. Forgejo reads
come from its REST API rather than fj's output, which has no structured form,
and work anonymously for public repositories. The watch shares a
persistent cursor per workspace and item, reporting existing activity on the first
wait and changes between waits thereafter; cancelling the watch discards that cursor.
An omitted timeout keeps the watch unbounded; an explicit timeout remains a bounded
polling interval.
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
Failing or stalled cleanup shows in workspace status and notifies once per run of
failed passes.

There is no diagnostic command: each problem shows where the user looks and names
the command that resolves it. A failing command's error says what to run, `inbox`
lists failed workspaces and untracked worktrees (skipping a repository whose Git
gate is busy), and the daemon checks its PATH with the shared executable catalog,
`root_dir`, checkouts, the global config and recorded forge hosts when it starts
or reloads its config, recording an alert for each problem. Repair is a request
the commands that need it send: `work adopt` reclaims, `work setup --skip`
accepts a failed setup, and both restore verified worktrees and clear executions
proven stopped while preserving work and leases; `stop` stops survivors and asks
before forgetting an execution it cannot prove ended. Every other command reports
an unreachable or mismatched daemon itself, and `shoal cd` says when shell
integration is not loaded. Opening
persistence only migrates schema. Under the daemon lock, startup then atomically
quarantines interrupted operations in a separate transaction before ownership
auditing, serving requests, or cleanup; failure aborts startup and leaves committed
migrations available for retry. Startup audits but never deletes, kills or
releases leases, and clears an unknown execution only when stopped-agent
reconciliation proves it ended. The cleanup sweep rechecks only the workspaces the
audit failed and readies one once its ownership verifies, checking before it
reserves the lifecycle so a workspace that still fails never leaves the failed
state; automatic repair goes no further, since repair would also accept a failed
setup the user has not seen.
Survivors are signaled only after verifying PID birth identity and same-user
ownership; acknowledgement cannot override visible live processes. `shoal stop` asks for it, since an execution
without a recorded launch identity cannot be proven ended even when nothing of it is visible. Recheck
unreadable process identities before treating visibility as incomplete; an
identity that has exited or is exiting no longer blocks the ownership proof, and
an inaccessible macOS identity is treated as a different process because it
cannot be a same-user process Shoal started. One whose exec has not yet
published its environment is read again within a
bounded settle wait before it counts as unreadable. An agent's reported
exit first stops its verified processes outside its process group, so background
shell commands do not outlive it. A reported command
exit clears its execution and permits setup readiness only when marker, child and
group evidence has no survivors and environment visibility is complete; a settled
empty environment is readable evidence, except a macOS platform binary's, which
reads that way when hidden. A hidden environment takes the attribution of the
nearest ancestor whose environment tells, through any setuid program the user
started, such as a terminal's `login`, which runs as another user and is never
stopped itself; one launchd adopted after its parent exited has none and counts
as unreadable. The reporting wrapper may remain alive
awaiting acknowledgement; workspace stopping allows for child registration,
stopping and completion verification; service termination and its command
deadlines cover that drain. Managed stops signal only the daemon through its
service manager and wait for its ownership lock before removing the job, so an
already-loaded service with an older termination limit cannot cut the drain short. Recovery polls
incomplete proof within a separate bounded wait and still refuses live or
unverifiable survivors. Moved
worktrees stay unresolved until restored. Any other ownership failure is
recoverable by explicit reclaim, which a human requests after checking the
worktree by adopting its recorded path again: it re-marks a linked worktree of the recorded repository at the
recorded path on the recorded branch that no other workspace owns, then
repairs normally. It is never automatic, and scoped callers cannot request it. A deleted directory means
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
