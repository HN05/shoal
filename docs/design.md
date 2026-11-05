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
browsers, schedule work beyond the agent queue, which also admits registered
repositories' CI jobs, or store conversations. Agent shortcuts
(`claude`, `codex`, `happy`, `t3`) are thin launchers around the generic execution
path; CLI launcher argument arrays are configurable defaults; session and desktop
adapters retain their tool-specific setup. Shoal does not switch the user's or
daemon's login. `[secrets]` names
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
makes for a workspace act as the agent too, with the host's configured
`agent_token`. An agent Git profile
selected the same way reaches tracked agents as Git's environment-level config,
so the agent's commands use it in any repository while the worktree's config and
other executions keep theirs; the push rewrites of `[permissions] push` travel
the same way. Desktop handoffs have no authentication override
because they may reuse an existing process.
The generic repository `[env]` table is available to tracked executions and hooks:
it renders workspace identity and reserved ports into user-selected variables
without managing the integrated tool.

## Architecture

One Rust binary provides the CLI, execution wrapper, and daemon. Each state
directory has one daemon shared across repositories, with a private Unix
socket and a versioned JSON protocol; the ordinary installation is per user.
Each protocol message is one frame of at most 64 KiB. A list too large for one
frame is sent in pages, each frame but the last marked as continued; streamed
listings send their frames, then `ok`; the daemon answers any other reply that
would not fit with an error saying so.
The protocol is the one protocol for the CLI and for apps. Apps use it through
`shoal::client`, the library's one public module, which holds the requests and
responses with the types they carry, the connection the CLI makes, and the
client-side logic the CLI shares with apps; the CLI and the daemon stay private
to the crate, behind the default `cli` feature. Commands print only the
library's types with `--json`, which a development build checks read back, so
a program that runs `shoal --json` parses the output with them. The protocol
keeps no
compatibility promise: its version and exact-match rule refuse a client of
another release with `protocol_mismatch` naming both versions, and an app
updates with Shoal. Without a scope token an app is the user; with one, it is
that workspace's agent, under the scope checks and approvals of its `shoal`
commands. Creating or splitting a workspace, opening a PR and merging start
operations, below, whose ID a client follows for typed progress, then a result
or a typed failure. A subscription sends a snapshot of the records of its
topics, then each record that changed or was removed, comparing again after
each stored change, until its scope token is revoked; a daemon restart closes
it and `shoal::client` subscribes again and reports the new snapshot as a
resync.
The minimum Rust version follows current stable so the code can use its newest
features; the manifest and CI toolchain advance together, with locked dependencies.
The daemon raises its open file limit at startup and accepts every connection,
whatever its requests hold. Request/response operations share a fixed limit;
executions, waits and streams take a separate share of the remaining descriptors,
so neither can block the other, and daemon administration takes neither. A request past the limit is refused at once with
the requests that hold it, and a request that runs past a minute is logged, again
when it ends. A request that only reads state is cancelled when its client hangs
up; one that changes state finishes, so a hangup never leaves it half done. When the long-lived share is full, a request whose client sends it
again after the daemon closes its connection, as across a restart, is closed
unanswered; others are refused. Accept failures that clear on their own,
such as running out of descriptors, are retried; any other stops the daemon with
a log line.
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
Workspace setup with the post-setup hook, `pr open`'s push and `merge` are
operations: the daemon starts their work in a worker process of its own,
detached as it starts queued agents, so the work goes on when the CLI or app that
asked for it goes. The worker acts with the caller's scope and, from the CLI, its
environment without the secrets an agent launch exported, and runs the same tracked executions and hooks the CLI ran, under
the same checks; what it and its processes write becomes the operation's output,
and it reports typed steps and its end. The CLI starts an operation and follows
it through `shoal::client`, as an app does, showing its output where it showed
it before, so the CLI and apps cannot drift. `shoal add`, `split` and apps open a
workspace and start its preparation through one request, `StartWorkspace` or
`StartSplit`: it refuses an agent that cannot run or an issue that is not open
before creating anything, creates the workspace or reopens the one on the
branch, PR head or record it names, assigns the issue, removing a new workspace
it cannot assign, and starts the operation, which sets it up, links its PR, runs
a new workspace's post-setup hook and queues the detached agent the request
names, with its model, arguments, fork, issue and caller's place, as
`split --agent` and a scoped `add --agent` start theirs. The CLI follows it
and keeps its prompt for a failed setup and an agent it launches in the terminal. An interrupt or termination of the CLI cancels the operation, which
interrupts the worker as it interrupted the CLI; a CLI that ends otherwise leaves
it running. The daemon keeps an operation in memory until ten minutes after it
ends, so a client follows it again by its ID from any event; a restart forgets
it, and setup and merges fail with their daemon as before.

The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups. It retains buffered daemon controls across start, stop,
and recovery transitions so adjacent frames cannot be lost. After commands it
restores the caller's foreground group and OS terminal settings. When `TERM` is nonempty and not `dumb`,
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
wrappers advertise it when they register: a restart or an update detaches those and
stops the rest as manual stop does, within the workspace stop timeout. Setup and
merges hold daemon gates, so they never reattach.
Only a planned exit, a restart or an update, keeps work running. The daemon
cannot tell a service command's SIGTERM from a shutdown's, a logout's or a
`kill`, so a signal stops
all work with the save-first shutdown stop below, and the service commands
instead ask the daemon over its socket how to exit: `shoal daemon restart` and
an install that replaces an incompatible daemon ask it to detach its wrappers,
and `shoal daemon stop` to stop work as `shoal stop --all` does, so agents stay
down until `shoal resume`, except that queued launches in existing workspaces
stay for the next daemon to start. The request is permanent like reattachment and
answered before the protocol check, since restarting is how an upgrade reaches
a daemon of another release; a daemon that does not answer gets SIGTERM.

Overload protection is configured machine-wide: memory is enabled by default,
CPU is opt-in and requires sustained aggregate busy time. One `load` setting
(`light`, `balanced`, `full`) sets the default CPU and memory thresholds, so a
user chooses how hard to load the machine without tuning each percentage;
explicit thresholds override it. Linux memory defaults rise with swap, which
makes the OOM killer a later risk. Stop connected tracked
agents through their execution wrappers, preserving work and leases: agents no
terminal shows first, since the user is likely working with a shown one, then
the agent idle longest, since an idle agent loses no turn, and a working one,
newest first, only when no idle one is left. Shown agents still stop once no
other is left, since a host such as Herdr may count every agent as shown. An idle agent with processes outside its
process group counts as working, since their completion may start its next
turn, and so does one without turn reports. Its hook waiting for the next
update runs there too, marked so that it does not count. Once the
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
unfinished assignment, naming when and why it stopped, which every stop that
saves an agent records and a last session keeps, and that its subagents and
background commands ended; configured restore commands receive that prompt through
`{prompt}`. Never replay the original task prompt. Keep waiting wrappers tracked; they reattach
like running commands, so a daemon restart or crash hands the wait to the next
daemon instead of cancelling the restore. A waiting wrapper's session shows
only the stop, so `attach` and its picker leave it out, under every place
recorded for it. A daemon update waits for a stop
and a restore waiting for load, but not through a wait for an update, which
may last indefinitely; that wrapper reattaches to the updated daemon. Restores wait in the agent queue,
and manual recovery persists across restarts. Saved recovery represents
unfinished work until restored or explicitly discarded, except that a done
assignment needs no agent, so a done workspace's agent is restored neither
automatically nor by `shoal resume --all`, only by resuming that workspace.
An idle agent of a done workspace whose cleanup is held could run until the
workspace goes, so the cleanup sweep stops it as protection stops an agent
between turns, and an update restores it although the assignment is done,
since Shoal rather than the agent ended it. Load plays no part, so the recovery
policy does not apply. Stopping any idle agent works the same way and is
opt-in, since timers the agent keeps itself end with it, which only a done
assignment leaves nothing to do. Either stops an agent once its prompt cache
has expired and not before, since a resumed session's first request writes
most of the cache again. A
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
`shoal resume` still reconciles the execution it selects. An unknown execution
whose wrapper, and child when recorded, ran in an earlier boot needs no
evidence: restarting the machine ended its processes, while its recorded process
group may name another's, so the daemon forgets it, keeping its record, or
else the session its agent reported as a last session. The daemon records each
execution's agent name for that.
Agent metadata is transient, so restart cannot select disconnected survivors.
`shoal restart` starts each agent in a new wrapper: the daemon sends the stop
protection sends, with the reason `restarted by shoal restart` and recovery
whatever the overload policy, and once the agent's processes are proven
stopped, cancels the wrapper's restore, which every release finishes on with
the agent's record kept. The record becomes no last session, and its end no
exit notification; the daemon then records the record's restore as admitted
and starts it through the queue start worker, as `shoal resume --now` does,
in the terminal host of the agent's recorded place. The new wrapper runs in a
new process tree, so a restart recovers an agent whose wrapper's tree broke,
such as one that lost access to an external volume. The record keeps the
agent's slot until the admitted restore takes it over, so the restart neither
queues behind launches nor loses the slot to them; a start that fails or
leaves the queue before the agent registers fails the restart, leaving the
record for `shoal resume`. The restart, with the runs that asked for it,
follows the agent to the execution the new wrapper registers, which answers
it once the agent starts. `--in-place` restarts through the same wrapper,
which keeps the agent in its terminal: the daemon resumes the agent once its
processes are proven stopped, outside the agent queue. A restart outlives the requests that asked for it; a request for an
agent already restarting joins that restart and is answered as the first one
is. Each `shoal restart` run names itself in its requests, and the daemon
lists each pending restart with the run that asked for it and each agent with
the run that last restarted it, so a run that finds restarts in progress
continues under their run's name and leaves out what that run restarted. An
agent between its wrappers is listed as the execution it ran as, and one a new
wrapper runs with the executions it ran as, which requests may still name. The
restore command resolves the agent again, so an updated agent
binary runs. A wrapper whose binary an upgrade replaced execs the installed one
when the daemon resumes its agent in place, keeping its process, terminal and daemon
connection: the daemon sends nothing until the restored child starts, so no
control is lost. The resume names the binary the daemon runs, and the wrapper
continues only when the installed file is that binary, whatever path either
started from, since the daemon holds back its own update until the agent
starts; the installed release bounds the commands it runs to prepare the agent. The installed release registers the execution again on that
connection, which records its place and returns a new plan, then restores the
agent, so new wrapper code reaches running sessions without stopping them.
The CLI restarts the selected agents together by default; `--sequential` spreads the load of
starting them, restarting one at a time, idle ones first, starting the next
once the daemon registers the restarted agent and at least the admission
stagger after the previous start.
Agents without a restore command and executions whose wrapper is gone are left
to `shoal stop` and `shoal resume`; a restart that cannot start the agent leaves
its record for `shoal resume`.
`shoal stop --interrupt` redirects an agent without stopping it: the agent's
own interrupt key cancels its turn and keeps its process, background shells
and subagents, which a stop and resume would end and start again. The CLI types
the key through the terminal host of the place the daemon recorded for the
agent, so a client driving Shoal need not know each agent's key or each host,
and the daemon records the turn's end, which no hook reports. The daemon keeps
the interruption with the agent's transient metadata until a turn starts or
ends again; `shoal resume` continues the interrupted turn, as it continues
stopped work, so a client pairs one command with each.
The daemon also samples what each workspace's processes use, attributed by
the workspace identity in their environment, so the user can see which
workspace loads the machine; protection, warnings and admission do not use it
yet. Processes that leave the workspace's environment, such as work handed
to a shared server or container runtime, count for the machine only.
Warnings come first: below each enabled protection's stop threshold, the daemon
queues an agent message for every workspace with a running agent, so agents can
reduce load before anything stops. Warnings repeat per signal and workspace only
after an interval and never stop, clean up or notify. A memory or CPU warning
wakes an idle agent only while something besides the agents runs in its
workspace, checked when the agent would be woken rather than when the warning
is queued, since a turn that has just ended leaves nothing to stop and each
woken turn costs tokens; otherwise it waits for the agent's next update or turn.
Checks made at the same time share one process scan that starts after each asks;
operations that stop or remove processes verify ownership with their own.
A disk warning still wakes it, since deleting files frees space.
Their thresholds are not
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
work and leases, and means the user wants its agents down until they resume
them: each stopped agent of the workspace whose execution ended, including one
a signal or protection stopped before, becomes a last session, as an agent that
exits on its own leaves one. The wrapper saves its recovery record as for any
stop and the daemon converts it once the stop finishes, so wrappers of earlier
releases stop the same way. Taking agents down briefly is `shoal restart`.
A launch waiting in the agent queue to start in the workspace has no session
to save, so a manual stop cancels it before stopping anything, under the queue
gate so admission cannot start it after, and reports it; one already starting
belongs to its agent. `shoal stop` of a waiting launch of a new workspace
cancels it as `shoal rm` does, and `--all` also selects the workspaces such a
launch waits to start in. `shoal move root`, which starts the stopped work
again after the move, keeps them. User commands
save their arguments, which resume reports once, as the restored agent's first
prompt or else to the user, and never reruns, because replaying an arbitrary
command is not known to be safe. Scoped callers stop only the executions of the
workspaces below theirs. A hangup
or terminate signal to an agent's wrapper or agent saves the agent as manual stop
does: closing a terminal, logging out and shutting down end the agent's session
without the agent choosing to quit, and the daemon may already be gone. The
wrapper saves it before stopping the agent, as the daemon does before a
protection stop, since shutting down may kill the wrapper first. Unlike a
manual stop, the agent then comes back on its own through the agent queue: a
request beside its record asks for the restore, which the daemon queues once the
execution has ended, so agents a reboot interrupted return without a command.
The signals alone depend on each wrapper receiving one and saving before the
session ends, so on Linux the daemon holds logind's shutdown delay lock and,
once a shutdown starts, stops running work itself with the same save-first stop
while it and the wrappers still run, then releases the lock. A delay lock, not
a blocking one: a shutdown never waits on Shoal beyond logind's limit, and the
records saved first survive a stop that outlasts it. Elsewhere the signal that
ends the daemon starts the same stop, which the service definition's stop
timeout bounds; after a logind stop the signal leaves work alone.
As after a protection stop, an agent stopped between turns returns only for an
update, counting processes still running outside its process group as a turn
in flight, and its restore keeps its workspace from no cleanup meanwhile; `shoal resume` asks for the agent itself, so its restore waits for
nothing but admission.
The request is its own file so that forgetting it, as a stop does, never
replaces a record a resume holds. An
agent that exits on its own before its assignment is done leaves its workspace
a last session for `shoal resume` instead of stopped work: the user may have
quit it on purpose, so it takes no agent slot, keeps nothing from cleanup and
comes back only when `shoal resume` names it or `--all` queues it. A workspace
keeps one per agent, so resuming several stopped together restores each; a new
agent replaces them, since the user moved on, while a restored one consumes
only its own, which the wrapper says when it registers.
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
configuration and hooks disabled, except that plain Git, with hooks disabled too,
creates a workspace at a commit's worktree, since Worktrunk only makes worktrees on
a branch; Worktrunk removes it like any other. Typed adapter outcomes preserve unfamiliar strings;
only confirmed branch deletion sets the response deletion flag. Invoke external
tools with argument arrays. Captured subprocesses share optional deadlines, whose
error names the command line and directory, and a
single diagnostic limit, terminate on cancellation, and drain output while sending
input. Keep writable state outside the binary's directory.
Service setup manages one per-user launchd/systemd service and refuses to change
its state directory until the existing service is stopped; foreground mode covers
environments without a service manager. The service captures the installing shell's
`PATH`.
Managed daemons record their stable executable path before startup work and restart
after a replacement, including one during startup, once daemon operations are idle
and no setup or merge runs; commands reattach.
Retain the ownership lock and listening socket across exec so queued requests reach
the replacement under the same service PID. The lock file names the owning
process by PID and birth, so a client refused by the socket tells a live daemon
that does not answer from none running and points to restarting it. Read-only streams reconnect after the
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
checksummed prebuilt Linux and macOS binaries on x86_64 and arm64. The formula
has no head channel, so every Homebrew installation runs a CI-signed release.
`shoal update` replaces a downloaded binary or app bundle with the latest GitHub release and
leaves Homebrew installations to Homebrew, so one package manager owns each
installation. A shared release-target table drives cross-builds,
archive names and Homebrew platform selectors.
Named configuration templates in `configs/` are embedded at build time, so
installation needs no source checkout or network access.
Shoal owns the generated formula. One Forgejo Actions workflow releases: it
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
A release runs the test suite once: it validates the version commit before opening
the PR, again only when the merge changed that tree, and CI runs no checks on the
release PR or on its version commit on main.

Release signing: the release job signs each macOS binary with `rcodesign` as
`com.shoal.cli` using a project-owned self-signed certificate, packages a copy
as `Shoal.app` and signs the bundle too, and fails unless both signatures have
the expected designated requirement. macOS keeps privacy grants such as Removable Volumes
for an app bundle by its identifier and that requirement, wherever it is
installed. For a bare executable it keeps them by path, and Homebrew installs
each version at its own path, so each upgrade would ask again; an ad-hoc
signature's requirement is its hash, so each release would need a new grant.
The formula links `bin/shoal` to the bundle's executable, so the daemon, the
execution wrappers and every command share the bundle's grant; the archive
keeps the bare binary for downloaded installations, which stay at one path. Packaged files
such as the skills link sit beside the bundle, since a file added inside it
breaks its signature. Only a stable
requirement is needed, not Gatekeeper trust, so there is no Developer ID or
notarization. The certificate came from `rcodesign
generate-self-signed-certificate --person-name "Shoal" --validity-days 7300`;
its unified PEM is the `MACOS_SIGNING_PEM` Actions secret, and the maintainer
keeps a copy outside Forgejo. A new certificate changes the requirement, so
every user grants access again.

Privacy grants of agents: macOS asks the process responsible for an access,
which a process inherits from whatever started it, for grants such as removable
volume access. A zmx session's processes inherit one that has exited, so macOS
judges each new program by itself and a restarted agent asks for its own grant
(Claude Code's). Execution wrappers started in a place a host or the daemon
opened (`internal add`, `session`, `detached` and `demo`) therefore replace
themselves at startup with their own binary, started with responsibility
disclaimed: the agent and everything it starts use Shoal's grant while the
wrapper runs, whatever started it. Only certificate-signed builds claim, since
an ad-hoc build's grant lasts one build and test builds would prompt. A process
that replaces itself with another program, such as `internal launch` or
`chat`, never claims: that program would take the responsibility over.

## Workspaces and Git

Every repository owns `~/shoal/<name>/` (`machine.root_dir`): its workspaces
default to it and a URL clone lives there as `.checkout`, a name no
workspace can take, so worktrees are grouped per repository, never nested in a
checkout, and outside the state directory; a root inside either, or inside a
recorded repository directory, is refused.
On macOS a root Shoal creates links to `<parent>/.<name>.noindex`, which holds
its contents: Spotlight ignores `.metadata_never_index` below a volume root and
its privacy list needs root, so only a skipped name keeps build output out of
the index, and the link keeps `root_dir` the same on every system. An existing
root is used as found; only `move root` changes it, by default to the link,
or back to a regular directory with `--regular-dir`.
Before registration, workspace creation or an export writes a directory, the
daemon creates a file there itself under a time limit: its file access can
differ from the terminal's, and on macOS opening a file on a volume it lacks
access to can wait for a permission prompt no one sees.
File access in a worktree can wait without end: while macOS waits for an
answer to a privacy prompt, whichever process it is for, every new process that
opens a file in the protected location waits until the prompt is answered or the
kernel interrupts it (`Interrupted system call`), so each Git the daemon starts
does. Shoal cannot end the wait, only keep it from holding up unrelated work,
and does not depend on where the worktree is or which permission it waits on.
The daemon's own reads in a worktree, such as its ownership marker or whether
it exists, can wait too, and no time limit interrupts a read waiting in the
kernel, so requests and sweeps read worktree files on the blocking pool, where
a read holds no runtime thread and a check's time limit ends the wait for it.
The startup audit, its recheck, the sweep's branch observation and cleanup
checks, the Git reads of `shoal status`, and the reads of a checkout's or
worktree's config file, which every command that resolves its settings, such
as `shoal attach`, waits on, check a worktree within 30 seconds, a status
reporting what it could not read; one that runs out or is interrupted
records the directory holding the worktree as stalled, which the daemon reports
once as an alert and in `daemon status` until a check there finishes. The
directory comes from the path alone, since reading the filesystem could stall
too, and groups a repository's workspaces, which a prompt for their location
holds together.
Those checks skip a stalled directory until 20 seconds after its last check, and
while another check rechecks it, so each sweep rechecks it once; the sweep's
cleanup phases skip a workspace or checkout in a stalled directory, reading its
HEAD themselves when a recheck is due, since none may be left there that branch
observation can read; an unattended removal, deleted-worktree cleanup included,
is refused while a workspace below it is in a stalled directory, and PR cleanup
skips a base while one stacked on it is. Listings read
HEAD for review marks within three seconds in all and skip stalled directories,
leaving staleness unknown, so `shoal attach` and `shoal work` answer from the
daemon's state. Repository pickers start Git only to tell apart repositories
that share a name, reading their origins at once within three seconds, and
label one whose origin they cannot read by its recorded source.
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
rolls back an incomplete one, deleting only directories with their recorded identity. Checkouts and workspaces outside it stay.
`move root` stops the commands and agents running there as `shoal stop`
does and queues the agents' restores after the move, trusting the new paths
for them: a stop keeps sessions, which resume by ID from any directory. Registration is idempotent by normalized origin URL,
then canonical path, never fetches, and keeps a stable UUID separate from the
display name. Repository selectors preserve the caller's argument alongside its
canonical path. IDs and sources win before path lookup; a registered name survives
unrelated path collisions, while a name and path matching different registrations
require disambiguation.
`repo rm` binds confirmation to the resolved registration and shows
its source and checkout path. After verifying the checkout's identity and
boundaries, it deletes the checkout recursively, then removes the repository
directory only when that directory is empty.
Each workspace and chat gets its own temporary directory as `TMPDIR`, in the
repository directory's `.tmp` under its ID, which never changes or comes back
like a name. Outside the worktree it neither dirties it nor enters checkpoints,
and its path stays short enough for the Unix sockets tools create in it on
macOS. It is created only once its record exists and deleted with it, by
the removal of its space; the cleanup sweep
deletes any without a record, since nothing there is worth keeping. The
post-remove hook runs after it is gone and keeps the inherited `TMPDIR`.
A contribution is a URL clone registered for work in a repository the user does
not maintain. `origin` stays the repository itself, so issues, PRs and syncs use
it; when the user's login cannot push there, the user's fork, created or reused,
becomes the `fork` remote and `remote.pushDefault`, which Git, `pr open` and
cleanup all honour. It goes through `repo rm`'s path once nothing of it has been
left for the unattended grace period: no workspace, queued agent, chat, pending
issue proposal, kept branch or borrowed resource, nothing `shoal work restore` could
use, no files a removed chat keeps, and no work in its checkout that the remotes' advertised branches and tags
lack. The final
check records the removal in the same transaction, under the gates creation
takes, and queue and chat records refuse a repository being removed. An existing
registration is never turned into one.

A new branch starts from the repository default branch: `origin/HEAD`, the sole
remote's HEAD, or the checkout's current branch without remotes, never a guessed
`main`. It starts from the local default branch as the background sync left it,
so creating a workspace waits on the remote only to discover a remote HEAD not
yet cached; the sync fast-forwards it from
its upstream, preserving an ahead branch and leaving it on divergence, dirty or
managed checkouts, and failed fetches. `--base REF` starts from any locally
resolvable commit ref, except a configured remote's branch, which is fetched first
and must succeed.
Creation, default-branch sync, setup, repository removal, and recovery share
a per-repository Git gate. Shoal's own Git commands skip optional locks, so
background status checks never hold a worktree's index lock, and except clones
are stopped after ten minutes, so a hung volume fails the request that waits on it. Daemon ref updates
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

A workspace may sit at a commit instead of on a branch: a worktree at a detached
HEAD with no branch of its own, created from a commit, tag, branch or PR head, a
fork's included, so comparing with another commit, researching older code or
reviewing a fork's PR never disturbs another worktree. It holds no agent of its
own, so no session, ready marks, completion or the hooks that follow them: whoever
created it owns it and runs things in it, a CI job, a reviewer, the parent's agent
or the user's shell, `shoal run` and `shoal demo`, and an agent that should work at
a commit gets a branch workspace from it instead. Everything else goes through the
workspace paths: the seed clone, executions, leases, scope tokens, children, holds,
checkpoints and removal. `split --no-branch` creates one, below the caller's
space or as the user's top-level one, and a fork PR's head opens in one, placed
the same way, that is reused per PR and moved to its new head while it has nothing of its own. Its
starting commit counts as kept on removal, since a fork's head or a local commit is
on no remote, while commits made on top count as unsaved work; restore reopens it
without a branch, and branch-only commands refuse it. `--skip-setup` and
`--disposable` are general creation options of `add` and `split`, not special cases
for a runner: one skips what gates readiness, with the removal hooks that tear it
down, and the other makes every removal delete the workspace without inspecting it,
asking or keeping a checkpoint.

A child workspace is split from a parent with `shoal split`, so an agent whose
work needs another PR gets another branch and worktree instead of switching
branches or creating worktrees Shoal does not own. Splitting creates a new
branch in the parent's repository, from the default branch or a `--base` that
stacks it as usual, records the parent, and links an `--issue`. Any space opens
a child of either kind the same way: `shoal add` and `shoal chat` run inside a
space put the new workspace or chat below it, in its repository, and a scoped
caller creates nothing else, while the user's terminal creates top-level spaces.
A chat an agent starts opens in a background place, since the agent cannot
hand over its terminal, and keeps the agent's scope until it records the chat
below the agent's space. The scope token goes through a private directory the
place's command names, never the place's environment, which a shell left there
would keep. The agent waits for that handoff, since its scope may
end with it, and may not run the chat in its own terminal, where the chat's
processes would be its own; places drop the marks and scope token of the space
that opened them, so what runs there is the new chat's. Spaces form one tree through one parent link,
`space_parents`, which records a workspace split from another and one a chat
opened alike: every space has at most one parent, a ready workspace or a chat
of its own repository, never one below it, and a child in another repository
is refused, since spaces across repositories need a space without one. Every
reader takes one walk up or one walk down that link, with no branch on kind,
and asks each space one question, whether it has an agent: a workspace's
agent, a chat's running agent, or a queued restore of either. A scope reaches
its workspace and every space below it, the CLI resolves a scoped command to the
child whose directory it runs in, and agent hooks always report for the agent's
own workspace. The parent's agent, a workspace's or a chat's, receives the
issue and PR activity of the spaces below it without an agent of their own,
and theirs in turn, each update naming its space, through the same cursors,
deliveries and acknowledgements as its own; a child's own agent, which reports its turn state or which Shoal tracks
there, as it tracks custom and Happy agents that report none, takes over its
subtree, and so does one whose restore is queued, since it receives what
arrived once it starts. A child never keeps its parent and never outlives it:
the parent is cleaned up by its own rules, and every removal of it, manual or
unattended, first removes the spaces below it, deepest first, through the
shared removal path, with a checkpoint and kept branch for each workspace but a
disposable one, whatever their work, holds or questions; one that cannot go
retains the parent, and the journal and
the removal's output name those taken. While a parent's completion with cleanup
stands at its HEAD, its children of either kind go too after the grace period,
whatever their own work, unless one of their own keep-reasons, or a space still
below them, keeps them; a child's own `done` follows the usual rules. The parent owns its
children, so its processes may remove and hold them, except that the user's
holds anywhere below refuse the removal. `work` lists children below their
parent, and `status` names the parent and children of either kind. `split --agent` gives a child an agent of its
own through the agent queue, and `split --fork` gives it the parent's agent
from a fork of its session, the one the caller's agent or else the parent's only
agent reported last, so the parent need not restate what it learned; the daemon starts every launch in an existing
workspace itself, detached, so the agent never inherits the caller's scope, and
scoped callers may queue only for the workspaces below theirs, and cancel those
entries or move them back, but never ahead of other work. A workspace's
coordinator is the agent of the nearest space above it that has one, whatever
its kind, which one operation queues a message for: the events about
it that notify the user, the child agent's messages, questions and completion
and its agent's exits and stops, plus its ready marks, become that agent's
messages instead, falling back to the user without a coordinator or when its
messages are full. Events a process causes in a child without an agent of its
own are the coordinator's own work and reach the user. The coordinator answers
questions asked below it or passes them up with its own question, and may
stop the agents below it and discard their stopped work, since only it knows
whether a child agent still has work. It resumes only what it or a workspace
below it stopped, which the stopped session records, through the queue, so the
daemon starts them detached; resuming what the user or Shoal stopped, and
confirming that an execution Shoal cannot prove ended did, stay the user's.
Stopping a workspace stops the workspaces below it unless `--skip-children`
says otherwise, since their agents work for it. Approval
requests go up the same tree: to the nearest space above with an agent, of
either kind, since each knows best what its children need. A parent space,
workspace or chat, approves only what it holds an approved grant for, or a
hold on its child, and otherwise denies or passes the request up; the user
decides at the top, and a chat whose agent stops or a
deciding space that is removed hands the user what it had not decided, since
neither can decide any more.
The worker skill steers agents to a subagent working in the child's directory
over `--agent`: the child then has no agent of its own, so its activity reaches
the parent, and the subagent reports to the parent directly, supervision Shoal
need not relay. Claude Code's worktree hooks make the worktree it creates for a
subagent such a child, so its own isolation needs no worktree Shoal does not own.
A subagent inherits its agent's scope, and under Codex its instructions and
directory, so a `SubagentStart` hook names the workspace it works in; without
it, a Codex subagent finishes its agent's assignment as the instructions say.

`shoal branch rename` changes a ready workspace's checked-out branch and derived name
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
survivors; manual removal also takes a preparing workspace with no execution, whose
setup an interrupted command never started. Failures preserve files, branches, and leases; process-verification
failures retain the workspace and report the command's exit status and the
blocking evidence. Other failures let interactive callers choose delete, ignore
(which repairs verified state first), or keep. JSON callers get a nonzero exit.
`work setup` reruns setup explicitly; nothing retries automatically.
Tracked setup starts by copying the repository checkout's ignored files through
Worktrunk unless `setup.copy_ignored = false`, so local files like `.env` arrive without a
setup script and failures gate readiness; `.worktreeinclude` selects what to copy.

Hooks are deliberately untracked user processes with the workspace identity
but no scope token, because their purpose is to start or stop things that
outlive the hook (a tmux session, say) without becoming execution survivors.
Nothing outlives the workspace: removal stops every verified process that still
carries its identity, including those hooks and exported environments started,
and its leftovers. A leftover is a detached process, one whose parent exited and
left it to launchd, init or the subreaper that takes the daemon's orphans (such
as `systemd --user`), or a descendant of one, that the space's worktree,
temporary directory, program or identity ties to it, with everything it started: no live process outside the
space keeps it, and the ties cover platform binaries whose hidden environment
cannot show the identity and programs built in the worktree that run elsewhere.
Exclusion follows live ancestry, not environment variables: the processes of
tracked executions and chat agents, those that started them, and terminal
session servers are never untracked. `shoal stop` takes the same processes
through the same operation after the executions, and `--processes` and
`--leftovers` take them alone, so an agent stops what its own commands left
behind without stopping itself. Automatic cleanup counts a leftover working
in the worktree as use instead of stopping it, but stops a program built there
and left running elsewhere.
Leftovers are reported, not stopped on sight: the cleanup sweep finds every
space's from one process listing and keeps them in memory, so no tool call
waits for a scan, and the agent's hook names each new one once, since the agent
knows whether its own test servers should keep running. They create no
notification; `shoal status` lists them for the user.
They use global defaults below repository config.
The pre-setup hook runs in the daemon after ownership and execution checks,
with a time limit, while preparation excludes lifecycle and permit changes;
failure marks setup failed. It may be configured without a setup command.
The post-setup hook runs in setup's operation, without a terminal, once the
workspace is ready, after its PR is linked and before any agent; failure keeps
the ready workspace. The pre-remove
hook runs in the daemon inside the single removal path for manual, repository,
and automatic removal, after checks pass and commands stop, bounded in time;
failure retains the workspace. Ownership and removal policy are rechecked after
all removal hooks, with branch deletion derived from the final inspection.
Idle cleanup requires unchanged
HEAD, activity and file metadata; while the timer runs, sweeps compare HEAD and
activity and check recorded state and directory users, and inspect Git state and
walk the tree only when the timer starts, when it expires and after removal hooks,
since change times only move forward and cannot be set. For the same reason,
merged cleanup walks a worktree again only once the newest change its last walk
saw is older than the idle period. A sweep lists directory
users with one lsof scan shared by every workspace it checks; removal scans again.
Status reports the first reason cleanup keeps a workspace from the checks the sweep
runs, so it cannot disagree with the sweep. A HEAD changed by removal
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
branch, which the daemon keeps current; it writes only objects. Git merges two
branches that pick the same migration or protocol version cleanly, since their
edits agree, so a repository lists the files that hold such numbers as
`[conflicts] sensitive` paths and globs, without value patterns, and `conflicts`
also lists each one that HEAD and the target both changed since their merge base
as changed on both sides, left out like conflicts once the target has HEAD's
changes. Ordinary changes rarely break the default branch this way, so Shoal has
no merge queue. Every minute, after a
PR of the repository merges, and on `sync`, the daemon fetches the default branch's remotes under a fetch gate of its own,
which moving or removing the repository also takes, and only then fast-forwards
the local default branch to what it fetched under the repository's Git gate, so
creation never waits on a sync's fetch, advancing a clean registered checkout where
`git fetch` into the checked-out branch is refused; it never pushes or moves workspace branches, so updating
a workspace from it stays plain Git. Commands that only read the default branch
never sync, so none waits on a fetch or handles a failed sync; workspace
status shows a failing sync, and one failing for 30 minutes of awake time, which
a brief outage or a wake before the network does not reach, needs the user and
joins the inbox with one alert. Git already merges and rebases a workspace onto the
default branch, so Shoal has no command for that direction. `merge`, the guarded
local substitute for a pull request, merges the workspace branch into the default
branch, first refreshing a configured upstream while preserving an ahead branch and
refusing divergence or fetch failure. It pushes the default branch to that upstream
only on request, checking the upstream before merging and keeping the merge when the
push fails. The default branch cannot be
held by a managed workspace, and any checkout of it must be clean. It aborts a merge
that does not apply cleanly, leaving conflicts to a Git merge of the default branch into
the workspace. Merging holds the repository Git gate for the tracked execution; the
daemon validates, resolves the push target and refreshes, and the CLI worker merges.
Pushing publishes the merge where a local reset cannot reach it, so a merge
that pushes asks the user first, after the daemon's checks and before it
changes anything; it names the direction, since `git merge main` in a workspace
merges the other way, and any open PR `shoal pr merge` would merge instead. A
local merge asks only when the last check found sensitive files changed on both
sides, which the same question names. An agent's approval request carries the
same details and is its confirmation; the agent still answers the question about
sensitive files with `--yes`, so the approver decides on them.
A repository that merges only through PRs turns it off with `[merge] enabled =
false`, which refuses agents' requests to merge too.
Because a local merge can be undone, `unmerge` is a command of its own rather than
a flag that flips `merge`. The daemon records each merge when its execution ends,
from what the default branch holds rather than what the worker reports, and drops
records the default branch no longer holds or its upstream has. `unmerge` undoes
the branch's latest recorded merge under the same gate, checks and tracked
execution: it moves the default branch back when nothing came after the merge, and
otherwise replays the later history onto the commit before it with
`--rebase-merges` in a worktree of its own, then moves the default branch to the
result in one step, rewriting the records of the merges it replays. It refuses a
pushed merge, since only a revert undoes that, and a replay that would bring the
merged commits back through a later merge, drop a later merge commit's own
changes, or does not apply cleanly, and an undo that would overwrite ignored files
in the default checkout; Shoal makes
no revert commits. It recreates a branch cleanup deleted, so the commits stay on one.
Since it runs in the workspace's tracked execution, a workspace cleanup removed is
restored with `shoal work restore` first; records follow the branch, not the
workspace's ID, so the restored workspace's merge stays undoable.
After cancellation, the wrapper rolls back incomplete merges after stopping the
process group, preserving completed merges and reporting unsafe recovery failures;
an interrupted replay's worktree is removed, and the default branch has not moved.
Refresh fetches use private temporary refs; merges run in the tracked wrapper.
A fetch of a whole remote also deletes the tracking refs of branches the remote no
longer has, since cleanup counts commits on a stale ref as pushed.

A repository's `[setup.seed]` keeps one build of its local default branch that new
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
`setup.seed.command` would. Tools whose caches already serve every checkout get only what
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

Build acceleration and the CI runner belong in core: Shoal runs the CI jobs of
the repositories registered with it, so a job starts from the seed and rebuilds
only what its commit changed, and the machine's agents and jobs share one
admission instead of competing for memory. The forge's own runner runs the
steps on the host, `forgejo-runner one-job --handle` or a GitHub just-in-time
`actions/runner`, which keeps the workflow semantics, actions and log
streaming, while Shoal decides which job runs when: it lists the jobs waiting
for its labels as agent queue entries, admits them with agents, and claims a
job only once admitted, so until then any other runner may take it. GitHub
has no handle and no label that keeps a runner to one job, so its runner may
take another job its labels match; a job-started hook holds a job of another
run until its workspace is at that run's commit, so a check never reports on
another commit. Polling stands in for webhooks, which a local daemon cannot
receive; it runs only while a job could start, and conditional reads keep
unchanged answers off the rate limit. Each job runs as a tracked execution in
a disposable workspace at its commit, which goes once the forge's runner
exits. A job ends only on the forge's word, so stopping it cancels its run
there and protection leaves it running. The workspace a run's PR or branch links lists the job, but
neither owns it, lends it a build nor reaches it with its scope. The runner is
registered per repository with the user's own login, never for a user,
organisation or instance, or on GitHub one runner per job, and what the forge
does its own way is a `Backend` operation, so another forge adds an
implementation, not daemon code. Container jobs and forges other than Forgejo
and GitHub remain proposals.

## Scope and user interfaces

Tracked executions and processes started with exported workspace environments
inherit a daemon-validated scope token that
confines them to their own workspace and the spaces below it:
status, inspect, execute, setup, and resources.
Creation other than of a child of the caller's space, removal other than of a descendant the user holds nothing below,
reconciliation, other workspaces, repository administration, and service control need an
unscoped caller. A scoped `merge` is an
approval request bound to the branch and its HEAD unless `[permissions] merge`
and `push` are `"direct"`, and a scoped `unmerge` one bound to the merge it undoes;
approval lets the agent's retry merge that commit, or undo that merge, through the normal checks,
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
`work`, repository administration under `repo`, moving workspaces to another
machine and repositories to another root under `move`, forge inspection and API access under
`forge`, CI runs and the checks on a PR's head under `ci`, and
shell integration under `shell`. `run` lists configured commands, runs one by
name, or executes an arbitrary command after `--`; `--workspace` selects the
workspace in either form. `done --undo` withdraws completion and `ready --undo`
withdraws review marks. Creating a question uses `questions ask`; notifications use `alerts`, with
`alerts send` to send one for a space, which the notification names. Notification rows, in `alerts` and the picker, show
the time for today's and otherwise how long ago they came in whole days, weeks,
months or years, such as `1 day ago` or `2 weeks ago`.
Approval snapshots carry their stable request key through the CLI so reused
numbers cannot decide a replacement. Implicit question and approval decisions
require stdin, stdout and stderr on a terminal, so redirected details cannot
hide the decision being made.

`terminal.herdr.enabled = false` turns off every Herdr call, so a Herdr pane behaves as a
plain terminal; `new_tab` only governs opening new tabs.
The CLI owns Herdr tab handoffs after issue lookup and interactive workspace and
agent choices and labels the tab `<repo>#<number>` for an issue, otherwise with the
allocated workspace branch by default. An issue number or branch says little about
the work, so its agent names it with a workspace title, which replaces that
default and fills a template's `{title}`. The user may set the title or ask the
agent to choose one, a fixed agent message refused without a running agent,
which would leave it unread until removal. Setting one relabels the places Shoal
created for the workspace that the caller's host server reaches, while other
places take it when they next open. An optional `terminal.herdr.tab_name` template
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
explicit focus setting or `--focus` overrides this choice. A launch the user
waits for starts past `max_agents`, as `--now` does: one `--focus` focuses, or
an interactive call's agent waiting for input that the setting does not send to
the background, unless `--after` makes it wait for work; the setting alone does not, so it
cannot switch the limit off. Commands say only what they
launch; one terminal layer chooses the placement and the backend that carries
it out, so focus behaves alike everywhere and another backend plugs in with
its own primitives without touching commands: Herdr focuses a tab or
opens it in the background, and outside Herdr zmx attaches the caller or starts
a detached session running the plan, which it receives in its environment as a
tab does. A resume takes no new input, so it restores agents where they stopped
or in the background; only `--focus` restores one in the caller's terminal.
The worker drops the plan before starting children; a retained tab's shell keeps
it, inert, while a non-default state directory is passed only to the worker.
Herdr tabs remain open after tracked agent exits, and the tab's shell enters the
workspace through its own exported navigation file, followed once at a prompt
after the worker writes it. Completion that keeps the workspace closes a place
Shoal opened for it, a Herdr tab or a zmx session for an addition, an attached
agent or a background resume, only after tracked executions
have resolved or the agent's turn finished, since interactive agents stay running
at their prompt, and never while stopped work waits for resume, so the place keeps
the stop's cause and `shoal resume --all` can restore the agent there; removal
closes it immediately, so completion cleanup leaves the agent its grace period.
The turn comes from the agent's hooks, and from the host only for an agent
without them, such as Herdr's `idle` or `done`; zmx reads none, so its sessions
of such agents close with their execution. The rule, the watcher and its marker
are generic: a host only closes a place and reports whether it is open, what
its agent is doing and whether closing it ends another place, as closing a Herdr
pane's tab ends its other panes, where a batch removal or a streaming export
then refuses to start; generic code tells the Shoal it starts in a new place
whether to watch it, while a resume in the pane its agent stopped in leaves that
tab's watcher as it was. Top-level `terminal.close_when_done` applies to every host, and
`terminal.herdr.close_when_done` overrides it for tabs, as `terminal.focus` is shared.
Preparation failures and shell or untracked desktop handoffs retain them until
that lifecycle change. A watcher ends once its host reports the place missing,
whatever the daemon reports, and leaves the place open once no daemon has owned
its state directory for two minutes, longer than a restart takes. It asks the
daemon whose its place is until the daemon answers; a place the daemon records, for its workspace or another
owner, or whose host labels it, is the daemon's, and the watcher closes it at
removal only when the daemon knows nothing of it, such as a tab opened before
Shoal recorded its places, never before an answer, which could close what
removal already did. Only such a watcher, whose host closes more than the
place, keeps the marker. One refused for a protocol mismatch restarts from the installation path Shoal was started through, never a
resolved versioned binary, once another binary is installed there, so tabs
opened before an upgrade still close.

Shoal closes only the zmx sessions it created; a session the user opened stays
theirs even after an agent ran in it. The daemon records the sessions Shoal
creates as it does other hosts' places, so `attach` still finds one whose launch
failed and left its shell, but labels carry ownership: zmx reuses session names,
so a recorded name does not prove a later session under it Shoal's, while a
label goes with the session, and a close from the record checks it. A record
names its session's process and creation time, so `attach` passes over a later
session under the name, whose own record replaces it. Creation
sets `shoal.created=1`, which relabelling never adds. The daemon closes a
workspace's sessions when it removes the workspace, whatever `terminal.close_when_done`
says, and startup and cleanup sweeps close those whose workspace record is gone,
retrying failures without restoring ownership. Sessions from before the label
have no known origin and are left alone. Relabelling replaces obsolete ownership
before any in-place agent or chat can launch, so removal preserves a session
reused by another owner. zmx has no conditional kill, so ownership checks,
relabelling and closes hold a lock beside zmx's socket directory, shared by
every state directory using that server. Finding sessions needs neither: the
labels and the lock exist only for cleanup.

Rex is a terminal host as Herdr is: inside a Rex block, Shoal opens a window in
the caller's session, or with `terminal.rex.placement = "split"` splits the caller's
block, and runs agents in place in their block, since Rex keeps sessions
running without a connected client. Generic code decides a split: only a start
from the caller's own place passes it to the host, so queued starts and starts
without one open windows. A block beside others carries its own label, and
Shoal renames a window only while the block it opened is alone there, so a
split never renames the user's window. A terminal no host
serves goes to `terminal.default_host`, zmx unless set to another host, whose places
then join the group the host's config names, such as `terminal.herdr.workspace_name`;
generic code reads the setting and renders the name, and a
host only finds or creates a group by label. A host opens a place for a
start and shows a place; generic code decides the rest: whether a start stays
in this terminal, whether an agent runs in place, which host takes a queued
start, and that a focused place opened from a terminal the host does not
serve is shown there, as `shoal attach` and opening a demo show one. Showing is
the host's own: Herdr attaches the terminal to the session holding the tab,
Rex, without a terminal client, selects the window in its app, and zmx
attaches the session. Shoal remembers
every place as one host-neutral origin record: the host, its group and place,
host-defined data, and whether Shoal created the place. The place is the one ID
a host names it by, for its watcher as well: a Herdr place is the pane, whose
tab Herdr finds to close, rename or focus. Generic code passes the
record to the host that reads it and never interprets the data, which for Rex
is how to reach the server and its CLI. Queued entries carry the queuing
terminal's origin, recovery records the stopped agent's, demos their launch's,
and the daemon the place each execution and chat runs in, which `attach`, `status`
and attach completion ask the host about for every host, together with the
places Shoal created for each workspace, whose records outlive their sessions;
the host never calls the daemon. A host
declares the variables naming its places, and a zmx session drops the
other hosts', so Shoal inside it never takes the caller's place for its own. A queued start
falls back from its origin's group to its host's configured group on the same
server, then to `terminal.default_host`; with that host turned off, a terminal no host
serves runs work in place, a queued start without another fallback fails, and
a demo runs as a background process. Rex cannot set a new terminal's
environment, so a new block runs a launch step that loads the worker and its
variables from a private file and deletes it, keeping them off screen and out
of shell history, then the user's login shell, which stays as in a Herdr tab.
Herdr cannot start a tab with a command, and a Rex demo needs its block before
its command, so Shoal types those into the new shell. Typed before the prompt,
a line shows twice, echoed by the terminal and drawn again by the line editor,
so Shoal waits until the terminal no longer takes input a line at a time, or
long enough for a shell without a line editor. Rex blocks and Herdr panes carry
no ownership marks, so the daemon's records of the places Shoal created are
the proof of ownership, closed through the same removal hook, orphan sweep
and pending-close retries as zmx sessions; neither host reuses IDs on one
server, Herdr not even across its restarts, so a recorded place that
still exists is Shoal's, and each state directory knows only its own. Servers
number places alike, so a record names its place by host, the server the host
reads from its data, and ID. An agent of
another workspace that starts in a recorded place takes it over and a chat
releases it from its workspace, as relabelling does for a zmx session, while a
nested command takes nothing; place watchers close recorded places
through the daemon, which reads the owner and closes under one lock, or forgets
the record and leaves the close to the watcher for a host it cannot close
places of. A close ends only that block or pane, leaving the user's other
blocks in its window and panes in its tab; Herdr closes a tab with its last
pane. Rex's own
`own_block` arbitrates terminal size between clients and is not ownership.

tmux is an opt-in host for users who already work in it, so agents run in their
tmux panes without a zmx session nested inside one. tmux draws panes through its
own emulator, so images and notifications reach the outer terminal only through
its passthrough, off by default, and an attach redraws tmux's screen; Shoal
documents these limits rather than working around them. `terminal.tmux.enabled`
lets tmux serve its panes and, unset, follows whether `terminal.default_host`
names tmux, so choosing the default host opts in everywhere and tmux users who
set neither keep zmx. Inherited variables cannot tell which of nested hosts is
innermost, so hosts claim a terminal in a fixed order, tmux last: a zmx session,
Herdr pane or Rex block inside tmux keeps its own behavior. A tmux session is a
group and a pane a place, as a Rex session and block are: Shoal opens a window
in the session the caller's pane started in, or splits that pane, runs agents in
place, and outside tmux opens windows in a configured session of the default
server and attaches the terminal to show one. tmux starts a pane with a
command, so a new window runs the launch step, which reads the command and its
variables from a private file and drops other hosts' place variables the
server's environment may carry, then the user's login shell; nothing is typed
on screen. A demo, whose worker needs its pane, replaces the window's shell
with it. Only a resume in an existing pane types the launch step at its prompt.
A server Shoal starts gets none of Shoal's own variables. A queued start carries
the caller's `TMUX` and socket directory, empty when unset, so a daemon started
inside tmux never sends it to the daemon's own server, and a start queued from a
pane reaches its origin's server through the socket its origin records.
tmux numbers panes afresh when its server restarts, so a place records the
server's socket and instance, its process and start time, beside the pane ID,
and a recorded pane is Shoal's only on that instance. Panes carry no ownership
marks, so the daemon's records prove ownership as for Rex blocks, and a close
kills only that pane, leaving the user's other panes, windows and sessions.

Zellij is an opt-in host on the same terms as tmux, enabled the same way and
claiming a terminal after it: a Zellij session is a group and a pane a place, a
new place opens as a tab in the caller's session or splits the caller's pane,
and agents run in place. Zellij also redraws panes through its own emulator,
but handles images, notifications and clipboard writes itself, as its
documentation describes, so Shoal documents its limits as for tmux and keeps zmx
the default. Zellij 0.45 drops tabs opened in a session no client shows, which
a background start would otherwise lose, so a place for a session without a
client, or for a terminal outside Zellij, opens as the first pane of a session
of its own. Zellij keeps no configured group: generic code then hands the
host no origin, and the host decides between the origin's session and a new
one, since only it can tell whether a client shows a session. Under load
Zellij can list none of the clients that show a session, so the host asks a
few times, briefly, before it takes a session as shown by none. Zellij runs a
server per session and numbers panes per server, so a place's ID names its
session and pane, it records the socket directory, and its instance is the
server's process and birth; Zellij takes no condition with a command, so the
host checks the instance just before each command on a recorded pane. Zellij
counts a session only when its server answers a probe in time, so it can
report a busy session as missing, and a running session can list no pane
before it lays out its first tab or while it is busy: the host asks again while
the session's server process still runs, the recorded instance for a recorded
pane, and takes a session as ended only once it does not. Under load Zellij
can also let a command return before its answer and hand that answer to the
next command, so the host takes a tab or pane a command printed only when the
session listed no such one before, and asks a pane or client listing or
screen dump again when it holds another command's answer or none.
Zellij closes a tab or session with its last pane only while a client shows the
session, so a close ends either along with the last pane there, as an attached
client would see it, leaving no empty tab or session. A session's last pane
closes by ending the session, never its tab, since a `zellij action` whose
server ends before answering never returns; the pane records nothing
of how its session began, since the place records itself from its own
environment.

Terminal agent launches run in zmx sessions so agents outlive the terminal:
detaching or closing it leaves the agent running, and `attach` reattaches. zmx
passes output through unchanged while attached and restores the screen from its
own terminal state on attach, so terminal protocols such as images keep working,
which multiplexers that re-render output through their own emulator, like tmux,
break or support only in part; zmx therefore stays the default host. The daemon requires whichever host
`terminal.default_host` names, so a user who sets another one need not install
zmx. The tracked wrapper
runs inside the session's pty, so tracking, scope, stop and recovery are
unchanged and the daemon still never touches terminal I/O. A failed detached
handoff closes its session only while its process and creation time still match.
When zmx returns control to the caller, the CLI resets emulator input modes on the caller's
terminal, and an addition whose agent ran in the session leaves the caller's
shell where it was: the workspace is the session's, and detaching returns to
the shell the user left. Sessions are named
after their workspace and labelled with its ID and canonical state-directory identity,
so daemons sharing zmx can distinguish their sessions. A detached session that Shoal
starts labels itself once its workspace exists. A start queued from a zmx
session joins no group, so it goes to `terminal.default_host`. Launches without a terminal, inside a zmx session, or in
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
The journal records a demo's start at its command's first run, when a start
reports it, and its stop when its record goes with the execution, so clients
showing demos follow them without polling; restarts between runs are not
changes. The listing carries a web demo's URL, which the daemon derives as
opening does, so clients do not rebuild it.

`max_agents` admission belongs to the daemon, so concurrent additions and its
dispatcher share one count. Slots derive from what the daemon tracks: connected
agents, executions awaiting reattachment, resume records, starting queue entries
and admitted one-shot runs such as reviews, excluding completed workspaces.
Agents therefore count however they started. A one-shot run's slot is its
in-memory request, which ends with the run, or with the daemon for a run whose
processes could not be proven stopped, so no slot outlives a restart. `machine.max_agents`
limits the machine; the repository option `agent.max_running` limits each repository,
with a global default like any other option, so one name never means both caps.
The machine setting defaults to `dynamic`, which
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
memory profiles remain a proposal. CI jobs go through the same admission and
order: `machine.max_jobs` limits them apart from agents, and under `dynamic` a
job counts at the CPU and memory its recent runs reached, which a job repeats
far more closely than an agent, and takes the one staggered start an agent
would.
Automatic restores are typed queue entries ordered with launches, queued when
the stopped agent's processes are proven stopped. A restore of an agent stopped
between turns waits for an update instead, holding no slot and holding back
nothing, since that agent has nothing to do until one arrives; until then it
also keeps its workspace from no cleanup, whose removal cancels it and keeps
the agent stopped for `shoal work restore`. Woken, it is
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
once the recovery record or last session is gone. A last session's admitted
restore counts as a launch, having no slot to take over.
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
worker carrying the queuing terminal's origin, since terminal launch logic
stays in the CLI: a background Herdr tab when queued from Herdr, otherwise
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
A `-` target selects from a per-command history of space IDs that the CLI keeps
in the state directory, shared across terminals so `attach -` works from a new pane;
it only saves typing a name, so it never fails a command or authorizes anything, and
scoped callers neither read nor write it. `attach` and every launch that shows a
workspace's or a chat's agent session here, such as `add`, `claude` or `chat`, record
the space in `attach`'s history once the session is shown and still runs: when the terminal
detaches, or right away for a host that shows it without attaching the terminal. A
focused `add` that opens a place records the workspace once the place hosts it, and a
focused `chat` that opens one records the chat once it registers there, since the host
may show the place before the workspace or chat exists. So `attach -` returns to the
session last detached from, not one another terminal still shows or one that ended;
it passes over removed workspaces and chats that no longer run, while other
commands' `-` selects only workspaces.
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
before a workspace exists, then the picker; `--repo` names a repository's checkout directly. `status` shows any space,
a workspace or a chat, running or stopped, through one path: links, watched PR state,
completion, sessions and what awaits the user for both, and for a workspace also
lifecycle, fork-point changes, active work and leases; the daemon looks PRs up on each
request and keeps a failed lookup on its PR. Human output uses `Display` for enum values and a shared
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
`review run` chooses between the `review` command and a local review, which
follows the review bot. The daemon runs the reviewer once, without a session,
read-only in a detached checkout of the committed HEAD, so the review judges what
the agent committed and never sees or changes its uncommitted work; Worktrunk only
creates worktrees on branches, so the checkout is a plain Git worktree. The
reviewer returns findings in a fixed schema, which the daemon stores, so the agent
answers each with a typed outcome carrying its reason, issue or original finding,
and the next review checks every `fixed` one instead of rereading free text. A finding
resolved `false-positive` outlives its workspace as a claim of its repository, which
every later review receives, so a wrong claim the reviewer would raise in every change
touching the same code is answered once. It is kept in a file beside the
repository's local config rather than a tracked one: the outcome records it, with no
command or commit of its own, reopening the finding withdraws it, and the user edits
the file to drop or add a claim. The
history a review receives is context, not instructions, and the verdict is shown
but decides nothing. Failures are typed reasons classified like the bot's, so
backend output never reaches a message. The 30-minute limit is fixed: a review that
long has stopped making progress. The daemon starts the reviewer as a one-shot
agent run, which any feature can use: an AI tool's command without a session or
terminal, in its own process group, killed on timeout, cancel or a daemon stop and
found again by its marker after a crash. A one-shot run takes an agent slot like
any agent and waits in the agent queue ahead of launches, since it reviews work
already done that an agent waits on. Protection stops a one-shot run before
any agent, since it has no session to lose; it asks for a slot again and waits
for healthy readings, as a stopped agent's restore does. A chat has no worktree or branch, so it
reviews no changes of its own: it names the workspace it reviews, or reviews
its one linked PR as a PR target, and its reviews are those of its linked PRs. A PR target, named by URL or `--pr`, resolves a same-repository PR through the daemon's
forge reads, as the agent account for a scoped caller and the user's logins otherwise, and
reviews its head branch against the PR base, reusing the owning workspace after
fast-forwarding it to the pushed head; local commits ahead stay, divergence is refused.
The daemon fast-forwards under a lifecycle reservation and the repository's Git gate,
after verifying the recorded worktree and its branch, so a switched or replaced
checkout is never moved.
Refining an issue before implementation is `shoal plan`'s, which settles it with
the user in a chat. `repo setup`
likewise prompts an agent, in a workspace on the `shoal-setup` branch, to write the
tracked repository config, so it arrives as a reviewable commit that every clone
shares. For a repository that won't take that file, or settings for one user,
`--local` or the interactive “Save locally” choice has the agent write the local
config and its scripts in the repository directory's `.shoal/` instead. Nothing
then needs a branch, so the agent runs as a chat in the registered checkout,
given that directory as another working directory;
the result applies as soon as it is saved, and the agent tries the setup script
in a temporary workspace it removes again. Explicit arguments are checked
before the interactive choice. A `review` command such as tuicr owns its own
storage, exports and forge authentication, with explicit feedback handoff to
agents.

AI tools are providers: built-in ones plus `[agents.<name>]` entries, one table
per tool holding its launcher, resume command, launch mode and argument
templates, merged option by option across layers so a repository can replace
`command` and keep the global `resume`. A `command` or `base` gives a tool a
`shoal <name>` agent shortcut that `run` shares; names that built-in commands
shadow are rejected, and `[commands]` holds only shortcuts that are not agents.
What a tool needs of the machine, its skill directory and proxy, sits under
`[machine.agents.<name>]`. Pickers and completion offer providers only. Agents run
as tracked agents with scope, an agent Git profile, and exit notifications. Their `{prompt}` argument combines general instructions
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
An agent's model and reasoning effort are `model` and `effort` options of its
tool, which a setting that chooses an agent, such as `[agent] default`, may set
beside it for that use, and `--model` and `--effort` override for one launch.
Values pass through unchanged, since each tool names its models and effort
levels differently; the tool receives them through `model_args` and
`effort_args` templates, built in for Claude and Codex, and a launch with a value
its tool has no template for is refused rather than run without it. Claude's
effort variable overrides its flag, so a launch with an effort sets both. The
execution and its recovery records keep the values, so restores, `shoal resume`
and reports use what the agent started with rather than the configuration at
that time; a restore that cannot pass them waits, as a launch is refused. Desktop apps take neither and say so.
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
the user or with another delivery. A turn Shoal does not retry, with `retry` off or a
failure no retry fixes, notifies once per execution until the agent ends a turn
normally, so an idle agent whose turns fail is never silent and a run of failures
is one notification.
The retrying proxy lives in `crates/shoal-proxy`, which knows the wire protocols
but no Shoal code, like `shoal-forge`. Each tracked agent that names one gets its
own, run by its wrapper, so a daemon restart never cuts a stream and the proxy
ends with the agent; Shoal points the agent at it through the integration, which
knows how that agent takes a base URL. All its requests share one outage budget,
so the agent's own retries never multiply the proxy's. Claude Code keeps its prompt cache for 5 minutes outside
a claude.ai login within its usage limits, while an agent waiting for CI or a
review idles longer, so a proxied Claude agent gets the 1-hour cache unless its
environment or settings choose a lifetime.

Agent shortcuts use the execution wrapper: by default Codex CLI and Claude run with their
own sandbox and approval settings. Every Codex launch, chats included, turns on its
workspace-write sandbox's network access, which reaching the daemon's socket needs since
Codex allows a single socket only along with the network, whatever the configured
command. A workspace's Claude agent gets permission to run the Shoal commands that
finish its assignment, so completion never
depends on Claude Code's auto-mode check. Its settings also tell auto mode, in
an `autoMode.environment` entry after Claude Code's own, that the workspace's
branch, commits and PR are the agent's own work, since the classifier reads
settings but not the agent's instructions; it then allows the
`--force-with-lease` push of the rebased branch that the worker skill
prescribes. The classifier still judges each command, rather than an allow rule
passing every push. Every agent, chats included, gets the
configured `agent.dirs`, `/tmp` by default, as working directories, so it reads what the
tools it runs leave there without a permission prompt. A workspace's agent also
gets the repository's workspaces directory as one, so it works in the children it
splits there without leaving its sandbox or permission rules; chats split nothing
and get only their temporary directory, plus the local config directory when a
chat sets up local config. Both trust the repository's checkout,
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
Issue links and numbers invoke issue opening with `agent.default` standing in
for `--agent`; numbers use `--repo`, the current checkout/workspace, then a picker.
PR links open their head against the refreshed remote base and link the PR once
the workspace is ready, as issue links associate their issue; branch links open
an origin branch. Both use existing-branch ownership checks and retain an owned
workspace's diff base. A fork PR has no origin branch, so its head opens in the
PR's workspace at a commit instead, with the PR's base as its diff base, and
`--agent`, `--path`, `--git-profile` and `--base` are refused. Interactive `add`
without a branch, base or issue also offers the repository's open issues and PRs,
which open as their links do.
Interactive `add` uses an agent picker when no agent is named; non-interactive
additions launch only when an agent is named. `add --agent` launches only after
creation, setup, and the post-setup hook
succeed, or after an explicitly ignored setup failure, and retains the
workspace whatever the agent does. Desktop handoffs (Claude and Codex apps, T3) provide no
tracking or scope. A host runs Shoal's tracked agents, chats and demos and shows
them, as zmx, Herdr, Rex, tmux and Zellij do; an external app, such as Quay, is a UI that is
not a host and reads and acts on Shoal's state through `shoal::client`. It holds
a workspace while it shows it and queues agents, which the daemon starts
detached in `terminal.default_host`, which never names an external app.
Happy sessions are an optional integration behind a default Cargo feature and
the `terminal.happy.enabled` setting, off by default, so its encrypted client stays out of
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
`item_activity` event for each newly observed comment, edit, completed check,
conflict, closure, merge, reopen, or lookup failure. The first observation
establishes the baseline, and watch acknowledgements do not affect this stream.
The same poll queues each change on the workspace's watch cursor, holding
passing checks as the watch below describes, and wakes a running watch, so an agent receives activity without watching for it. A
poll reads spaces, their items, an item's independent parts and the agent queue's issue
dependencies with bounded concurrency, so one slow forge does not hold back the rest. Polls
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
stays, because deleting it would not unsay it, and a correction does. `hooks.post_agent_exit` exposes tracked-agent exit notifications to user
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
delivered twice. A workspace's messages reach its agent once they settle: a run
queued in quick succession, such as the user deciding several approvals, would
otherwise cost a wake or a hook output each, so deliveries hold unread messages
until none has arrived for a configured window, bounded so a steady stream still
arrives; reading them with `messages` does not wait. The hook never waits for cleanup or an item change, which can
hold Shoal's item records for minutes: an agent discards the output of a hook
that outlasts its timeout, so the activity waits for a later delivery. Claude Code also runs a background hook, after a turn ends normally
(`Stop`) or on an API error (`StopFailure`), that waits for the next update and
wakes the idle agent with it through `asyncRewake`. The hook
tries a failed wait again instead of exiting, because a hook that ends leaves the
idle agent with no wait until its next turn; waits outlast a daemon restart,
including the time before the next daemon listens and knows the agent's scope. Codex
has no hook that can start a turn, so its tracked wrapper types a fixed prompt
into the zmx session, Rex block or Herdr pane it runs in once Codex is idle with updates, at most once
per idle stretch, and the prompt's hook delivers them. Claude Code's wake hook
does not run after an interrupted turn, so its wrapper prompts it the same way
while it idles after one. Besides that prompt, Shoal writes to an agent's
terminal only the interrupt key `shoal stop --interrupt` sends and the fixed
prompt with which `shoal resume` continues the interrupted turn: the daemon
never does, the prompts carry no forge text, and they are appended to anything
typed but not yet sent. Shoal does not
pre-trust hooks where the agent requires review. Identical unread messages collapse.
A message may attach files, which the daemon copies from paths the CLI checked
into the repository directory beside its workspaces: agents get that directory
as a working directory, so a sandboxed agent reads them there without a
prompt, and a copy in the worktree would be an untracked file that holds up
cleanup. A copy outlives its message, whose path the agent's transcript keeps,
until the workspace is removed, and is named by the message and the files'
contents, so an identical message with the same files still collapses.
A workspace being removed takes no message, since removal decides from the
messages it has and deletes them with the workspace.
`messages` reads new messages while `messages send` sends them, to one space,
a workspace or running chat, or, with `--all`, to every running agent of a
repository, in its workspaces and chats, such as to have them all rebase onto a
fix: a workspace without one would hold the message until removal. A message
to a chat goes the way the user's own messages to it go, with its files kept
among the chat's. Scoped
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
space, a workspace or chat, whose agent asked them and go with it, while a stopped chat
keeps them. Marking the assignment done closes its open questions,
since the finished work waits on no answer and a question it made pointless would
otherwise keep the workspace until the user answers. An agent that no longer needs
an answer withdraws its question, which closes it the same way and takes back its
notification while the user has not seen it, so the user is not asked to decide
what no longer matters. Withdrawing completion does not reopen them, and a closed
question keeps its number until its space goes, so a late answer is refused
instead of reaching a later question.

An agent asks in its chat with `questions ask --in-chat`, and the question
reaches the user's questions too. A user talking with the agent reads the question
as the agent wrote it, while a `questions ask` command line in the chat is hard to
read; a user who is away gets it where they look. Shoal cannot tell from a host
whether the user is there: zmx counts its own short-lived probes as clients, and
an attach from a connection left open counts while the user is away, so routing
questions by attachment left them unanswered for hours. Agents are therefore not
told about attachment, which only orders overload stops. The question is the
turn's final message, which the `Stop` hook passes, so the agent writes it once.
A prompt the user submits in the chat closes it, since the user answered there,
while one Shoal submits does not.

Questions and approval requests are named `S.N`: the number of the space that
asked, then the smallest number no question or request of that space holds.
Both kinds share the space's sequence, so a number names one item and says
which space it belongs to, and counting per space keeps the numbers short
because the user types them. A bare number or one of the other kind is refused
with the form or command to use rather than guessed. Their records keep a
separate internal key that is never shown. A wait follows its question's space,
number and text, so a later question reusing the number cannot answer it, and a
keep or merge request that no longer applies stays as outdated until the
workspace goes, so its number is never reused and a decision meant for it cannot
approve a newer commit. A proposed issue outlives its space, so its space's
number stays reserved until the last such proposal is decided, keeping the
number the user was told. An agent may
withdraw a pending request it no longer needs, as it may a question: it stays as
withdrawn on the same terms, except a proposed issue, which would outlive the
workspace and goes as a decided proposal does.

Shoal owns forge writes for its workspaces so that every client, from an agent
in a terminal to an app built on Shoal, gets one item model, one account rule
and one policy. Issue and PR actions are typed (edit, comment, close, reopen,
merge) and planned per forge as REST requests, sent with curl and the token
`[machine.forges."<host>"]` gives the host; without one only reads go out,
anonymously. GitHub's GraphQL requests go the same way. Shoal needs only a token per host, so it runs
no forge CLI and reads none of their login files, and a forge works without its
CLI installed. A token
is given in the shared value form, so it may come from a command such as a
password manager, and is fetched at most once per operation and never stored;
an `{ env }` token reads the environment of the process sending the request,
which for the daemon is the service's. The agent account has its own
`agent_token` per host; without one it uses the user's token, as the agent
itself would. A change to a host with no token for the account is refused
before it is sent, naming the setting and the reload the daemon needs, since
a forge answers an unsigned change only with a bare 401.
The forge is selected from the remote's host without requiring configuration:
well-known hosts by name, and any other by asking it once through each forge's
version or metadata endpoint with the user's token for that host, since a name
cannot tell a GitHub Enterprise Server or a self-hosted GitLab from Forgejo;
the token goes only to the host it is set for. The daemon records
each answer in its state and asks recorded hosts again when it starts or
reloads its config, recording a changed answer with an alert. A host it cannot reach then
keeps its answer and raises no alert, since a machine may start offline; a global `[machine.forges."<host>"] type`
pins one instead and is never asked, and a host that answers as none fails with a message naming that
setting. Each process remembers the answers it resolved, so links of the host
parse the same way, and the CLI starts out knowing the pinned ones. A pasted
link of a host not yet resolved is classified without a request by the forge
its routes name, such as `/pull/` for GitHub
and `/-/merge_requests/` for GitLab; only an issue or repository URL, whose
form several forges share, falls back to the name: GitLab for a `gitlab` name
label, and otherwise Forgejo. Host types describe hosts rather than
repositories, so they are global only, like the tokens in the same table. GitLab-only concepts such as approval rules and merge trains
stay unmodeled; approvals count as approving reviews. Forge differences stay in planning, such as Forgejo keeping draft
state in the title, so callers see the same item either way. Milestones are
named by title and looked up as the ID each forge takes, and a new item gets its
assignees and milestone from the edit that finishes it, so one path sets them.
An edit fails when the item the forge then reports lacks its labels, assignees
or milestone, because forges drop what they cannot apply without an error. Each backend
declares the capabilities it has, and a request needing one it lacks is refused
before anything is sent with one typed error naming the forge and the
capability, so a backend gains a capability without changes elsewhere;
`shoal forge capabilities` reports them so agents can check first. Where a
forge's releases differ in a capability, the backend asks the server for its
version before the request, so an older server refuses as clearly and the
report matches the server. A revert uses
the forge's own where it has one, and elsewhere reverts the merge with Git in a
new workspace that opens the PR and goes once it merges, so backing out a merge
needs no agent; a conflicting revert does, so it is abandoned rather than left
half done. Which commits the merge added is established from their trees and
patches, never assumed, since reverting a guess can undo someone else's work. Actions run in the
daemon, where scope applies: they act as the workspace's agent account, while
merging, deleting a PR's branch and a review's verdict stay the user's
decisions, so scoped callers can comment in a review but not merge, delete a
branch, approve or request changes, and those act as the user's own login.
Deleting the branch of a merged or closed PR refuses a branch that no longer
points to the PR's head, since its other commits are in no PR. A forge that refuses a verdict, such as on its author's
own PR, says so itself rather than Shoal guessing first. A review's line
comments are checked against the PR's diff before anything is sent, so a
review is refused whole rather than posted in part. Review
threads are named by any of their comments' IDs. Reactions use GitHub's names
on every forge, so a client offers one set, which `shoal forge reactions` narrows
to what an instance allows, and are read for a view with its
comments, which costs a request per comment on forges that list them per comment;
reacting reads first and changes only a reaction not already as asked, since
forges differ in whether adding one twice fails. A comment edit changes only
a comment the acting login wrote, checked against the item's discussion before
anything is sent, since an account with admin rights could otherwise rewrite
someone else's words. Every forge read also runs in the daemon: listing issues
and PRs, items' views, the kind of a bare number, an issue's details and
blockers, a PR's head, possible reviewers and review threads, and a
repository's labels, reactions and capabilities. Apps read them without
choosing a login or a host's forge themselves, and the CLI resolves no forge
host of its own: a scoped caller reads as the agent account and anyone else as
the user's own logins, in another repository too, since checking for a
duplicate before filing there is only a read. A search uses each forge's own search and
reads pages only until the limit is met. `shoal forge api` passes any request under
the repository's forge API root through the daemon the same way, for reads no
command models. The daemon
cannot tell what another method changes, so a scoped caller sends only GET
unless `[permissions] api_writes` accepts that its writes may merge, approve or skip
required labels, and only GET in another repository, which that setting does
not speak for; the path stays under the API root, so the login reaches only
the API of the forge its repository is on. `shoal forge download` reads a file
such as an image in a description the same way, so a client without forge
logins shows a private repository's images; it takes only URLs on the
repository's forge host, and the daemon sends the bytes back in frames, since
the caller, not the daemon, chooses where they go. GitLab's web routes for
uploads take no access token, so its backend maps an upload URL to the API
route for the same file and reads it with the host's token, like every other
GitLab request; curl sends the token only to that host, not to object storage
a redirect leads to. Bitbucket's files need a browser session, so it
has no downloads. Forgejo's issue search lists PRs
without their branches, so each listed PR is read in full, and GitLab keeps
merged merge requests apart from closed ones, so listing closed ones reads all
and drops the open ones. Shoal does not sign in to a
forge's web UI, so it cannot resolve threads on Forgejo, whose API has no
endpoint for it. Resolving a thread nobody has answered needs confirmation,
because the reply tells the reviewer why. Attached files are read
by the daemon from paths the CLI checked, anywhere on disk: a body file can
already come from anywhere and scope is cooperative, while recordings kept in the
worktree would be untracked files that hold up cleanup. GitHub's REST API has no
upload endpoint, so Shoal uses the one gh itself uses on GitHub's upload host. Forgejo
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
same on every forge. Label and reviewer choices are listed under the names
those options take, a team as `owner/team`, so a client can offer them without
forge API calls of its own.

Ready-for-review marks are the agent's status signal for integrations, separate
from messages and completion: they never notify, complete, or change cleanup. A
mark covers a linked issue or PR, or the workspace when nothing is linked, and
binds to HEAD; new commits make it outdated rather than removing it, so marking
again is an explicit statement about the new revision. Only an open PR whose
head on the forge is HEAD takes a mark, since a branch that opens several PRs in
turn leaves merged ones linked, and a PR cannot be reviewed at a commit it lacks;
marking skips the others with their reasons rather than failing the rest. Marking linked items
replaces a workspace mark. Unlinking an item
withdraws its mark and removal deletes all marks. Marks and withdrawals are
journal events, and `hooks.post_ready` runs after each explicit mark under the
completion hook rules; failure notifies and keeps the marks. Marking a linked
draft PR also takes it out of draft unless `[pr] undraft_on_ready = false`, since
forges and review bots often skip drafts. Withdrawing a mark leaves the PR
ready unless `--draft` or `[pr] draft_on_unready` asks to return it to draft, because
an agent usually withdraws to push more work that still needs CI and review.
A failed forge change fails the command with the marks kept, so the agent can
retry it.
A chat marks the same way, except that its checkout is not on a PR's branch:
it marks each linked open PR at the head the forge reports, and its issue or itself
at its checkout's HEAD. Its marks are not journal events, since the journal
records workspaces, and they show it `ready` in lists and the inbox.

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
completes, or affects idle cleanup, though a failed report from a workspace
process alerts the user with the error, which the hooks discard; overload protection reads it only to stop
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
An idle report that ends a turn also starts the agent's prompt cache lifetime,
which each request renews, so Shoal knows when an idle agent's next turn would
write its whole conversation to the cache again. The AI tool's configured
lifetime wins, then the one the agent's own settings give, which only its hook
reads from the environment the agent runs with, then 30 minutes. Every agent
gets a lifetime, since idle stop takes an unknown one for an expired cache and
would stop the agent as soon as its turn ends; 30 minutes is the least OpenAI
keeps a GPT-5.6 or later cache, and a guess for other tools.

A chat is a conversation with an agent about a repository, not an assignment,
so it runs in the registered checkout instead of in a workspace: coordinating
work is one thing a chat may do. Every chat is scoped, as a workspace's
processes are: its agent carries a scope token, kept with the chat across
stops and restores, that covers the chat and the spaces below it, so only the
user's own terminal acts unscoped. The agent follows the workspace-agent rules
without what needs a worktree: it opens workspaces and chats only below its
chat and in its repository, never takes over one outside it, and renames only
its own chat. It proposes issues where `[permissions]` says so, as a workspace's agent
does, recording no branch or commit, since it has none; approval opens the
issue from the chat's checkout and records it as the chat's. It starts at once from the default branch the
background sync keeps current, so an offline or diverged checkout still gets its
chat. Its
instructions describe the discussion only, from a chat template that
repositories can extend like the other prompt templates; the orchestrator skill is available when the user asks for coordination. Belonging
to no workspace, a chat is not a tracked execution: queuing and overload stops
serve unattended agents, while the user attends a chat. It is still a
daemon record, so lists and integrations can follow it: the agent replaces Shoal's
process, leaving no wrapper to report its exit, so the chat runs while a
process with the registered identity lives, checked periodically and before
chats are listed, and is stopped after, as a workspace stays when its agent
exits. With no wrapper to save it, the daemon saves a chat itself
where it saves workspaces' agents, marking its record before stopping its
processes so the end of the process does not end the chat. A stop records each
process before signalling it, so a later stop and the check before a restore or
discard follow the lineage of what it signalled after an interrupted stop
forgot them. A stop that fails leaves it running only while its agent does: a
stop before a shutdown or a signal's saves it to come back on its own, and `shoal daemon stop`
for `shoal resume`, where an agent that exits leaves it too. A stopped chat keeps its record, and with it its name, links, files,
messages, questions, workspaces and temporary directory, until a restore runs it again in
a new process. Only removal ends a chat: `shoal rm`, `shoal resume --discard`
for one with no workspace below it, or space cleanup, which treats a chat as a
workspace whose worktree is always clean, pushed and merged, its running agent
as use, a restore waiting to run as a queued agent and a session Shoal saved as
saved stopped work, and takes the idle time
of merged cleanup from when its agent stopped. `shoal resume` resolves each target to a
stopped chat or a workspace in one lookup over the shared namespace, so its
picker, `--all` and a list of names act on both. Restores and removals wait for a save
under way and refuse while a process the chat owns still runs, since two agents
would write one session and a removal would delete files in use, and a restore
whose agent does not start leaves the chat stopped and tells the user, who may
not see the place it opened. Starts, renames, stops, restores and removals
are journal events. A workspace a chat opens is its child in the one tree of
spaces, unless it was split from a workspace, below which it stays; opening it
outside a chat returns it to the user, so Shoal knows which chat coordinates
which workers. Removing the chat first removes the spaces below it through the
removal path of any space's children, and a stopped chat passes their events
on up, to the user at the top, unless it waits for its automatic restore. A
chat opens workspaces only in its own repository.
A chat may run in a ready workspace's worktree instead of the checkout, to
debug, audit or review that workspace's live work, committed and uncommitted,
with the commands that read a branch, rather than reading another worktree by
path. A workspace has one editing agent, its own: a chat in it reads and does
not edit, which its instructions say and nothing enforces until modes decide
what an agent may do. It stays a chat, with its own identity, scope,
temporary directory and messages, and is not the workspace's child, since a
child's questions and approvals go to its parent's agent while the user attends
a chat; so Shoal commands it runs never act as the workspace's agent. Its scope
reads the workspace as well as the chat and the spaces below it, and refuses
any request that changes the workspace. Its
record names the workspace, so a restore runs it there again, commands that
take the workspace from the directory act on it, and those that act on chats
keep acting on the chat. A running chat in a workspace is use, so no unattended
removal takes the workspace from under it, and one Shoal saved is the
workspace's saved stopped work, which keeps it from idle cleanup but not merged
cleanup. Removing the workspace removes its chats first through the chat
removal path, so a workspace one of them opened that cannot go keeps the chat,
which keeps the workspace. Nothing moves a session between directories: a chat
about another workspace is a new chat.
Having no workspace, a chat keeps its messages in its own queue, which the same
hooks deliver, including the user's request to rename it; a removed chat gives the user the notifications its unread
messages stand for.
A chat passes a question up by asking the user, who attends it. The approval
requests of the spaces below it come to it when it is the nearest space above
them with an agent, and it approves only what it holds a grant for, as a
parent workspace does, and otherwise denies or passes them up; those it
has not decided go to the user when its agent stops or it is removed, as with
any deciding space. A repository may have several chats, so each call starts a new
one and focuses it. A chat has a name from the same namespace as workspaces,
since both are places the user returns to by name, and its name frees when it
is removed or renamed. Renaming changes the record and the files kept under the
name, not the zmx session, which zmx names only at creation. A tab or block
that hosts the chat takes the new name only while it still shows the old one,
since another label is the user's. The process the user runs chooses the name, so the tab or session that
hosts the chat carries it before the chat is recorded; the chat's record names
the session it runs in, so `shoal attach` finds it there, not by a session name
zmx may have suffixed, and the chat labels the session with its ID so no
workspace's end closes it. A chat waits for the user to speak first unless it opens on a
task: an agent that takes instructions apart from the first message gets them
so, and another gets them as its first message, which starts a turn and so asks
only for a short reply or carries the task.
A chat's launcher runs where only the checkout's path has a value, so one that
uses another workspace placeholder is refused rather than given the literal text.
A Happy chat has no terminal to focus: like a workspace Happy session it starts
detached, as Happy's daemon would start it, so the user continues it in the
Happy app, and the process that recorded the chat hands the record to the
session's process.
A chat's agent reports its session through the session-start hook and the
`agent-state` and `turn-failed` commands a workspace's agent uses, which act on
the space they run in, and the record keeps the session so the chat can move; it
records no turn state.

Issues split by whether their plan is settled: `[issue] plan_label` marks one
that needs the owner's choices and `planned_label` one whose body holds a plan
an agent implements without asking. Both are repository configuration, so Shoal
assumes no label scheme, and issues with neither stay outside the flow.
`shoal plan` settles a plan in a chat, not a workspace: planning changes no
code, needs the user's attention and their authority to rewrite the issue, and
the chat then coordinates the implementing workspace it opens. The plan goes
into the issue body because the body is all an implementing agent's prompt
carries. The planning task is built in, since a
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
with the installed Shoal, so unless tests in a temporary home set
`SHOAL_DEV_USER_FILES=1`, they change no user-level files outside their state
directory, skills included, and refuse the user's own state directory, whose
daemon serves every running agent, however it is selected; the password
database names the user's home, so a temporary `HOME` does not. A changed or removed skill stays as the user left it until an
explicit install, which restores removed skills, asks about changed ones in a
terminal like a package manager's modified configuration files, and with
`--force` replaces them. `[machine.agents.<name>]` settings name skill directories, with defaults for built-in providers. Skill directories describe
the machine and cannot be set per repository. Packaged skills resolve the runtime
`SHOAL_SKILLS_DIR`, then an adjacent `shoal-skills` symlink
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

Global TOML is `[machine]`, the settings that describe this machine, plus the
repository schema for this machine's defaults, so nothing is declared twice and
a setting copies between the files. Each part is deserialized strictly on its own
from one parse: `serde(flatten)` would accept unknown keys and lose error
positions. It is seeded by `install` with the stated defaults when
absent; explicit key edits preserve unrelated settings and comments and validate
once per command before saving, so dependent keys change together, while reset or named template installation replaces the file.
Global writes keep the previous file as a backup; the daemon reads it at startup
and on reload, which global CLI writes request of a running daemon, replacing its
snapshot only with a valid file so agents and leases are untouched; an operation
that combines global definitions with resolved settings reads both from one snapshot. The CLI reads agent settings per command. Repository TOML comes from the
worktree (`.shoal.toml` or `.shoal/config.toml`, both together is an error) with
a local config layered over it per option, a named table replacing the one below
it whole. The local config is a file, `.shoal/config.toml` in the repository's
own directory beside its workspaces and `.checkout`: outside every Git tree, so
nothing has to ignore it, and read in place on every request, so an edit reaches
every workspace without copying or a restart. As a file, the user can edit it
with any tool, back it up and keep its scripts beside it: its relative hook and
setup paths name scripts there, made absolute when it loads. Key edits run in the daemon, serialized with moves
and removal, hold the file's lock, keep a backup and save only a valid result,
as global edits do; unset restores lower-layer values.
It moves with the repository's directory and is deleted with the registration.
Every option that does not describe the machine may be set at either level and
resolves per option: local config, worktree file, global config, then the
built-in default.
One declaration of each option, the field of the repository schema with its
kind, generates its built-in default, merge, resolved form and provenance, so a
reported layer is the one whose value is in use; the daemon resolves a workspace's settings
once per request and every use site reads them rather than layering on its own.
Prompt templates select repository additions by local/worktree precedence and
append them to global guidance, using the bundled chat template when
no global one is configured. Empty repository values suppress only the addition.
Repository Markdown files, at the root or in `.shoal/` like the repository
config and refused in both, and global files beside `config.toml` sit below
inline TOML values at each level; provenance names the highest configured layer
and the file it read, and reports the combined text.
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
every workspace agent. Former option names fail as unknown keys rather than
loading with a warning: the format broke once, so no setting keeps two names.
Before a workspace exists, the registered checkout's file stands in for the
worktree's. Repository config is read per request, so changes need no restart
and leave existing leases alone. Repository config cannot expand machine
policy: the `[machine]` table holds `root_dir`, simulator limits and profiles, Git profile definitions, and global resource
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
on every write and rejects malformed ownership fields, and journals a lease when
its record enters or leaves the leased state, or goes while leased. Allocation planning is pure
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
the highlighted request inside the list, which fzf updates in place rather than
closing and reopening. Decisions bind to the displayed or
authorized record through mutation, including when a coordinating agent decides. Explicit `--approve` and `--deny` flags decide every kind: merging
a branch (see scope), resource access, and issues and PRs an agent proposed.

In a shared repository everything an agent opens is seen by other people, so
`[permissions] issue_open` and `pr_open` can make a scoped `issue open` or `pr open`
propose instead of open (`"direct"` is the default; unscoped callers always open).
A proposal is an approval request whose record carries the command's options with
their defaults resolved, the workspace, branch, commit and agent, and copies of its
attachments in the state directory until no pending proposal uses them; the command
reports it and exits 0, because the agent goes on with its work and hears the
decision as a message. A proposal is routed like any request, to the nearest
space above with an agent and up to the user, but publishing to a forge other people see is the review
the setting asks for, so only the user approves one. A parent space may deny it
with a reason or pass it up; marking it a duplicate comments on the forge too, so that is not a workspace's
either. Approval carries data: the decider may replace the title and body, and the daemon then opens the item as the agent account through the same path
`issue open` and `pr open` use, recording and linking it. Denial may carry a reason,
and an issue proposal may be denied as a duplicate of an existing issue, which gets
its text as a comment, or of another pending proposal, which posts it once opened.
A decided proposal is deleted, since the message carries the outcome. An issue
proposal belongs to the repository rather than the space that made it, because
agents file follow-ups just before `done`; it opens through the repository's
checkout and agent account once the space is gone. A PR proposal is bound to the branch and its
commit like a merge request: the push still happens unless `[permissions] push`
holds it back, and a renamed branch or another
commit, locally or where the push went, outdates it at approval or when the agent
proposes again, and a pending one keeps the workspace
from unattended removal. Other agent writes, such as comments, edits and closing,
stay direct.

A proposed PR still publishes its branch, and agents push with plain Git too, so
`[permissions] push = "propose"` holds a scoped caller's pushes to the workspace
repository's remotes for the user (`"direct"` is the default; unscoped callers
always push); `"deny"` refuses them outright for repositories that should get
no agent push, through the same helper and `pr open` check, recording nothing.
A push request is a proposal, decided only by the user, and binds
exactly what the push changes: the remote and, where it has several, the push
URL, the ref, the object, and for a forced push or a deletion what it replaces,
so approval cannot push something the user did not see. A newer push to the
same ref outdates it, as does a moved branch at approval. A new PR whose push is
held becomes one PR proposal carrying the push, whatever `pr_open` says, since
it cannot open first, so one approval pushes and opens. Plain `git push` is
covered by rewriting the agent's push URLs, through its Git environment, to a
Shoal remote helper that asks the daemon about each ref. The rewrite is chosen
over a pre-push hook because `--no-verify` skips hooks and a Shoal hook would
replace the repository's own; like scope, it stops accidents, not a process
that clears its Git settings. A push URL that a fetch URL starts with cannot be
rewritten without breaking fetches, so the agent launch refuses it rather than
leave those pushes unheld. Approval runs the push without the agent, in an
operation whose end decides the request, with the agent's Git profile as
`pr open`'s push has. Nothing decides, withdraws or replaces a request while its
push runs, since the push lands regardless. Pushes Shoal authorized itself run
without the rewrites. Unless `push` is `"direct"`, a scoped `merge` needs approval
whatever `[permissions] merge` says, since it may push the default branch, and a
forge's `pr revert`, which pushes at once, is refused for scoped callers. A
pending push request keeps the workspace from unattended removal. Approving a merge
request whose branch moved since it was requested is refused as stale and marks it outdated.
Agents also find problems that belong to another repository, such as a tool the
project uses, so `issue open --repo` files an issue there, on any forge, as the
agent account; every other item action stays within the workspace's repository.
Item commands that need only a repository also run from a registered checkout
outside any space: they act as the repository's agent account under its
settings, open without a proposal, and link or record nothing, since a checkout
holds no links; workspace processes act from their workspace. A chat acts the
same way from its repository's checkout, under its own scope, but records
what it opens as a workspace does: a new issue is linked as opened, or assigned
with `--link`, and its item actions default to its linked items.
A chat holds links as a workspace does: the link tables name the space, a
workspace or chat, with IDs from their shared namespace, and go with it. Its
assigned items neither complete a chat nor clean it up: only its own `done`
records completion, and a watched PR keeps a chat from idle cleanup but not a
done one from completion cleanup, since a chat has no branch whose work an
unmerged PR would keep; and
the activity Shoal polls for them waits on the chat's own cursor, which the
chat's hooks deliver with its messages, waking its agent; a watch in the chat
and those hooks share one delivery, so the newest takes what is pending.
An unscoped client such as Quay names that repository with `--repo` instead of
picking a workspace, whose worktree Shoal would check first: a registered
repository's checkout is the place to act in, so it reaches PR, view and CI
commands from anywhere, and an item URL of a registered repository selects it
outside a workspace and checkout. Where `--repo` already names any repository to
read, a registered one is read through its checkout the same way.
People outside the project see such an issue, so `[permissions] issue_other_repositories`
governs scoped callers separately from `issue_open`: `"propose"` by default,
`"direct"`, or `"deny"`. The workspace's required labels and templates describe
its own repository and do not apply there, and the issue cannot be linked, since
an assignment's cleanup follows the workspace's repository. A proposal records
the repository it opens in.
Configured resources may require human approval for scoped acquisition. The daemon
persists a request with a caller-supplied reason and the effective allocation settings;
only an unscoped caller decides it, or the nearest space above the requesting
workspace with an agent: a workspace approves only what it holds an approved
grant for with the same target and settings and otherwise denies or passes the
request up, as a chat does; the user decides at the top. The decision is sent to the requesting
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
active lifecycle operations and hooks. Each hold records who placed it: the user
or an app hosting an external session, or an agent. Scoped callers hold and
release the workspaces below their own directly; holding their own is an approval
request, which approval carries out, so a coordinator keeps what its children
need. Agents release only the holds agents placed; the user's and host apps'
stay theirs. A hold never keeps a parent or survives its removal. Removal of a
workspace that is not disposable is reversible through checkpoints, so an agent otherwise has no way to keep a
workspace: one that needs the user asks, and an open question, like a hold, keeps the
workspace from every unattended removal, completion and PR cleanup included. So
does an agent launch waiting in the agent queue to start in the workspace.

Workspace completion uses `[cleanup.done] enabled` (default true), resolved through the
normal configuration layers, and an explicit cleanup override. Only an
explicit done signal completes an assignment; issue closure and merges do not,
and merged cleanup removes unused merged workspaces from their state instead.
Agent instructions make `done` the last step of every assignment. Completion
is persisted separately from lifecycle readiness, binds to HEAD, and notifies the
user; it does not assert that work was merged. A `done` that would change
nothing and an `undone` without a completion fail, so a misplaced signal is
reported instead of passing silently. An assignment that ends with a finding
rather than something to merge hands back files with its completion. They are
the workspace's results among its attached files, which stay when cleanup
removes it, for as long as its removal record, so the user or the parent's
agent can read them after the worktree is gone; the workspace's other attached
files go with it. Files an agent sends the user with an alert or a question
stay the same way, since the user may read the alert after the workspace is
gone, and outlive the alert or question as a message's files outlive the
message: clearing or withdrawing an alert and answering a question leave them
among the space's files, which the user already sees and Quay may show, until
`shoal files rm` or the retention removes them. The alert or question names them by their records rather than by path,
so a file `shoal files rm` deleted drops out and a moved repository directory
moves them, and a coordinating agent that receives the alert or question
instead receives their paths as the message's attached files. A chat's `done` records its completion on the chat as a
workspace's does, with cleanup by the same setting and override, but no branch
another could integrate and no parent to tell, so it notifies the user and its
agent keeps running until completion cleanup removes the chat after the grace
period, with the done state shown until `--undo` withdraws it. A workspace or a chat is a space: the user returns to both by name, from
one namespace, so what applies to both is named for
the space, and the flag that selects one is `--space`. Each space also has a
number, the smallest no other space holds when it is recorded, from one
sequence across repositories, so it stays short to type and read; it lasts as
long as the space, survives renames and stops, and is freed when the space goes. A chat owns attached
files as a workspace does, since its
agent hands the user reports and pages too; they stay for the same retention
after the chat is removed, as a chat has no worktree to clean up and its files
are what remain of it. Files are keyed by their owner's ID, which a workspace and a
chat never share, so no kind tells them apart: an owner is gone when neither
has the ID, and its removal or end record says how long its files stay. Each attached file records who attached it: the user, or the
agent whose process tree sent the request, by both its execution, which pins
the run, and the session it reported, which outlives a restart or resume and
names the conversation a client shows the file in. A chat's agent runs
outside any execution, so its requests name the chat they come from, and its
files record the chat as who attached them; like scope, this is cooperative.
The journal records each file's record as it is added and deleted, by store
triggers so no route of attaching or deleting is missed, and carries the
owner's ID from the record because a removed space has no row
left to supply it; clients showing files follow them without polling. A file's optional title
rides on its argument as `PATH#TITLE`, the form forge attachments use for alt
text, because attach flags repeat and a separate title flag could not say which
file it names. Own-workspace withdrawal removes a
recorded completion and preserves associations and merge requirements, but never
defers cleanup: only holds, open questions, a pending PR proposal or push request and queued
agent launches keep a workspace.
The daemon runs `hooks.post_done`
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
cleanup without a PR registration requires clean files and every commit kept,
or a completion marked integrated: another branch has HEAD's work under commits
whose patches were edited, which no check can match, so only the agent that took
them can say so. The mark binds to HEAD like the completion and keeps the branch,
since no remote or default branch has its commits. An existing PR registration
keeps its merge checks.
The daemon stops tracked executions through shared removal and rechecks files and
HEAD, including after hooks. Tracked agent exits wake the cleanup sweep after
their exit hook, without waiting for the polling interval. Changed HEAD retains
the workspace until a new completion signal; failed cleanup retains ownership
and reports why.
Issue linking and unlinking are own-workspace operations; associations are idempotent
and replacing one requires unlinking it first. Unlinking preserves recorded completion.
Issue associations suppress idle cleanup, not merged cleanup.
Item views let agents read issues and PRs without knowing the forge. The daemon
selects linked or explicit items, gives a bare number the kind of the linked
item or else the forge's, and reads them through the forge's API with the
account a read uses, or anonymously without a token, sending a view whose
discussion exceeds a protocol frame in pieces. Views automatically include the logs of
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
out; Forgejo's API reruns jobs only from release 17, which Shoal reads from the
server's version, and Shoal does not sign in to its web UI, so earlier Forgejo
releases and Gitea only cancel, and Shoal acts on no Bitbucket Pipelines job.
Agents may also start a workflow run on their own workspace's branch, on every
forge including Bitbucket. A started run is not a rerun of a PR's checks: on
Forgejo it has no head commit or event, so it cannot replace the rerun Forgejo
lacks before 17, while elsewhere only a run on the PR's head commit reports checks there.
`ci` gathers both: it reruns and cancels one run by its URL, or the checks on a
PR's head without one, and `view ci` reads a run as `view` reads an issue or PR.
Run numbers overlap issue and PR numbers, so only a number after `ci` names a
run. Workspace processes act only on runs of their own branch, which
the daemon reads from the run before acting, and read any run of their repository.
A run or workflow URL of another repository is the user's to act on: the
daemon starts, reads, links, reruns and cancels runs there with the user's own
logins, as it merges, also outside a workspace, and refuses workspace
processes, which stay within their own repository. A workflow file's URL also
names the branch a run starts on.
Forges hold the runs of fork and first-time contributors' PRs for approval, which
leaves such a PR without checks; `view` and `status` report the waiting runs
beside the checks, which stay exactly what the forge reports, so watches and
their wake-ups do not change. Approving lets outside code run, so like merging it
uses the user's login and is refused to workspace processes. Only GitHub has an
approve API; Forgejo approves through a web UI route, which Shoal does not use.
Viewing a run reads every page of its jobs; the daemon sends them, and listed
runs, in frame-sized pages, and each failed job's log, named by job ID, as one
request, like a PR's, so no response outgrows a protocol frame.

Manual and automatic cleanup share one path: establish ownership, stop owned
executions, saving an existing worktree's agent sessions but no commands, run
removal hooks, remove owned simulators, take a checkpoint unless the workspace is disposable, remove the worktree, and release leases with the record. Failures retain what is
needed to retry. Workspaces are cheap to remove because removal is reversible,
except that of a disposable workspace, which holds nothing worth keeping.
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
The removal record of a workspace that is not disposable (base, links and stopped
agents' recovery records, with its checkpoint) persists before any file is deleted, so an interrupted or failed
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
Moving a space to another machine ends it there and continues it here, through
one package whose manifest names its kind and one path for both kinds: the
package carries the agents' sessions and their files the same way, and only
ending and continuing the space differ. Each package uses the lowest format
version that reads it, so older importers still read workspace packages and
refuse a chat's as newer.
Moving a workspace is a removal there and a restore here:
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
Agent conversations are the agent tools' private files, which Shoal never
reads or changes. Shoal tells clients where they are without reading them: the
daemon reports the sessions a space's agents reported, and `shoal::client`
finds each one's files by ID where its tool keeps them, under the client's
environment or one the repository's agents ran with, as an export does. An
export copies each reported session's files unread, and an import puts them at the
same path under the importing agent's session directory, replacing a file there
only with one that continues it, as a session that went away and came back does,
and leaving that to the installed Shoal in a development build; a session whose
files cannot be read is left out with a warning and starts over. The export records the agents' sessions, with the
exporting machine's ID when it has one, beside the moved records, which older
importers read. An import on that machine, one that received the session's
files, or one told to keep sessions, keeps the session IDs, which the agents resume from any directory, dropping another
machine's terminal places and waiting wrappers, and every reopen trusts the
workspace's path for those agents, since resuming skips the launch's trust
setup. Otherwise moved recovery records drop their session, place and wrapper,
and `shoal resume` starts them through the fresh-agent launch, with its trust
setup and guidance, told the workspace moved. Moving a chat removes it here,
stopping its agent, since a session that kept growing here would fork from the one that
continues there, and the user attends a chat, so export asks first; a chat whose
agent reported no session refuses before stopping it, since nothing could continue
it, and so does one with spaces below it, which its removal would take. Import starts the chat through `shoal chat`'s launch with the agent's resume
arguments, under a free name since its own may be taken there, in the background
when no terminal attends the import, as queued agents start. A chat keeps no
record once it is removed, so a failed `move ssh` keeps its package where a workspace
keeps its removal record. Export and import move work
through any byte stream, and `move ssh` runs that pipe over SSH as a shorthand
after checking Shoal answers there; Shoal keeps no registry of machines, and
deciding when and where to move work belongs to a caller outside the core.
A branch is merged when the local default branch or its remote-tracking branch
records HEAD's tree, contains HEAD, takes HEAD's merge without changing its tree
(squash merges), or has an equivalent patch for every commit since the fork point
(rebase merges); merged commits count as retained. Commits whose changes the
branch the workspace builds on has, its base workspace's branch or else the local
branch it was created from, are kept there for completion and idle cleanup, but
are neither merged nor enough to delete the branch. Every removal deletes a branch
whose commits a remote-tracking branch or the default branch all have, since
deleting it loses nothing, and keeps any other: users should not have to decide
about branches. The default branch, and every branch while the default branch
cannot be resolved, is retained. Manual removal asks only before deleting
uncommitted or untracked files or commits on a detached HEAD, which no branch keeps.
Shoal remembers each branch a removal kept, at the commit removal left it on, and
the cleanup sweep deletes it once a remote or the default branch has all its
commits, without notifying anyone. It needs a default branch fetched in the last
ten minutes, fetching it when the background sync has not, and at most every ten
minutes it asks every remote which branches it has; only those answers
count, never remote-tracking refs, which a narrowed fetch mapping or a deleted
remote branch can leave stale. A kept branch that moved or went away, is the
default branch or belongs to a workspace again is forgotten; one checked out, or
the start or target of a rebase or bisect elsewhere, waits. Removal records name the branches kept before Shoal remembered them.
`shoal cleanup` deletes the kept branches the user picks, several at once, after
checking each is still at the commit removal left it on and not checked out.
Automatic cleanup removes only clean, idle worktrees whose commits are all kept,
with no executions, directory users, leases, or
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
stacked workspaces or holds, and no recorded activity, file change or per-worktree Git
change for the idle period. Idle time is persisted, so restarts do not reset it;
it decides only from a default branch fetched in the last ten minutes, by the
background sync or by a sync it runs itself, so a failed fetch keeps the workspace. A link, a watch or a recorded `done` does not keep the
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
user's forge login, resolve numbers against the workspace's origin into stored
URLs bound to that repository. Top-level `link` and `unlink` manage associations
and always name a relation, so no link becomes the assignment by default. `assigned`
items are the work the workspace exists for and drive completion and cleanup;
`opened` and `related` items are records for integrations without lifecycle effect,
and Shoal records `opened` itself for the issues, PRs and CI runs it creates.
A CI run is a linked item of its own kind, never `assigned`, because a run
cannot complete an assignment: watches follow it until they deliver its finish,
since a dispatched Forgejo run belongs to no PR and a run can outlive its PR's
watch. A PR's runs are not linked for it, as its checks already cover them. A
chat links the runs it starts as a workspace does, but its checkout's branch is
not its own: `ci list` defaults to the runs on its linked PR's head, its other
CI commands act on that PR's checks, and `ci run` starts on the default branch
unless `--branch` names another. Unlinking
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
reviews, edits, completed CI checks, new merge conflicts, closure, reopening or
merging. An edit is a change to what the item's own read reports besides its
state: its title, description, labels, assignees, milestone and, for a PR, its
draft state and base branch, named in one update per poll. Comparing what that
read already returns costs no request per kind of event, as following the
forge's timeline would, and works the same on every forge; changes no read
reports, such as requested reviewers, are not followed. A snapshot saved before
details were recorded takes the next read as its baseline. Edits a workspace
makes through Shoal, and Shoal's retargeting of a stacked PR, are taken as seen
on its cursor as its own comments and state changes are: each field the edit
changed, and of labels and assignees only the names it added or removed, so
another edit read in the same poll is still reported. Only edits count, since
what the reads around any other action show changed is someone else's.
Passing checks wait on the cursor while another check on the PR's head commit is
pending, since a pass asks nothing of the agent until the rest finish and each
wake costs it a turn: a check that does not pass delivers them at once, the last
pending one with itself, and a new head drops them. A check that never finishes
keeps them held; `status` shows them meanwhile. Holding happens where the poll
queues activity, so watches and agent hooks report the same updates, and only
for a PR's checks; the `item_activity` stream reports each one.
A conflict belongs to the head and base commits it was seen at, so a push or base
change that leaves the item conflicting is reported again. A forge's conflict is
reported once two polls in a row see it at the same commits, because Forgejo shows
the previous result until it rechecks and a PR it is still checking as not
mergeable. An
unfiltered watch without a linked PR runs the `conflicts` check on each poll as a
cursor item named by the branch, so work without a PR still learns of conflicts. It
leaves out conflicts with a target that already has HEAD's changes, as merged cleanup
decides, since merging could only repeat them, and those of a completion marked
integrated at HEAD; `conflicts` still reports them. A
linked PR leaves ordinary conflicts to the forge, avoiding duplicate reports, and
the watch and the background poll check the branch beside it only for files
changed on both sides, which no forge reports, and only where the repository
lists sensitive files. Each check against the default target is kept per
workspace with its head and target commits. Forgejo reads
come from its REST API and work anonymously for public repositories. The watch shares a
persistent cursor per workspace and item, reporting existing activity on the first
wait and changes between waits thereafter; cancelling the watch discards that cursor.
An omitted timeout keeps the watch unbounded; an explicit timeout remains a bounded
polling interval.
Activity lookups run outside completion serialization; persisting results
rechecks the watch set so cancelled watches cannot recreate their cursors.
Unacknowledged deliveries persist with the cursor so timeout or disconnection
cannot discard unseen updates; the CLI acknowledges after successful output.
They are stored apart from what the cursor saw of each item, so hook deliveries
and wake checks read only queued updates, never an item's discussion history.
Waits share the cursor, so a newer wait in a workspace supersedes the running one
before it takes updates, and an acknowledgement arriving while a newer wait runs
leaves the updates pending for it; competing waits would consume updates the agent
never reads.
Unavailable activity sources are explicit failures, never successful checks or
proof of mergeability; a persisting failure is reported again on an interval so it
cannot silence a wait while hiding a new conflict. An item whose activity has ended,
a merged PR or, once the assignment is done, a closed item, is not looked up again,
so an unreachable forge cannot fail or wake the wait of a finished workspace. Waiting does not resume stopped
agents or grant merge permission; agent instructions direct agents to handle
updates and wait again.
When completion requests cleanup, confirmed merges stop tracked agents through shared removal,
rechecking clean files and HEAD after stopping. Failures retain work and leases;
registered workspaces are excluded from idle cleanup until cleared or removed.
An invalid record retains its workspace without blocking cleanup of others.
A failed sweep step does not skip the others.
Background loops restart after a panic, and a failed daemon log write never stops them.
A step's failure in a workspace shows in that workspace's status, and other
failures or a stalled loop in daemon status and the inbox; each notifies once per
run of failed passes, about its workspace where it has one.

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
migrations available for retry. The daemon binds its socket before this work:
until it ends, requests wait, except that a request its client sends again is
closed unanswered and a status request still waiting after two seconds is refused
with the running step, how long it has run, and a stalled directory or a check that
has waited five seconds; a step that runs past a minute is logged. Startup
audits but never deletes, kills or releases leases, and clears an unknown
execution only when stopped-agent reconciliation proves it ended. The cleanup sweep rechecks only the workspaces the
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
published its environment, or that no ancestor attributes, is read again within a
bounded settle wait before it counts as unreadable, so a short-lived helper launchd
adopted while running a platform binary no longer blocks once it exits or execs a
program whose environment lacks the execution's marker. A reported agent or
command exit first stops its verified processes outside its process group, so
background shell commands do not outlive it; what should outlive a command
belongs in a hook or a demo, and setup instead fails on what it leaves running. A reported command
exit clears its execution and permits setup readiness only when marker, child and
group evidence has no survivors and environment visibility is complete; a settled
empty environment is readable evidence, except a macOS platform binary's, which
reads that way when hidden. A hidden environment takes the attribution of the
nearest ancestor whose environment tells, through any setuid program the user
started, such as a terminal's `login`, which runs as another user and is never
stopped itself. One launchd adopted after its parent exited has no ancestor left
to tell, so the daemon remembers, by unique process ID, what the latest inventory
told about each process's owner, an execution or none, while a live process can
still name it as itself or as its parent. Inventories take turns, so the latest
is the newest, except that an inference never replaces a reading of the process's
own environment. Inventories read the kernel's process table, `proc_listallpids` on macOS
and `/proc` on Linux, rather than run `ps`: stops, waits and status checks take
them often, and each `ps` cost a process start plus more reads than Shoal
needs, which skip a zombie after its first. Such a process takes what is remembered for it, failing that for
its exited parent, and counts as
unreadable only when no inventory saw either with its owner, as for a parent that
exited between scans or before a daemon restart. An unreadable process counts only against executions of the
workspace it works in; one working elsewhere, such as an orphan of a removed
workspace, or whose working directory cannot be read blocks none. The reporting wrapper may remain alive
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
  worktree other than temporary directories, caches, logs, and audit history.
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
