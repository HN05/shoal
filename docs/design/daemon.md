# Daemon

This file covers the daemon process, held in `shoal-daemon`, `shoal-daemon-base`
and `shoal-store`: its connections and limits, operations, store, journal,
startup, service and updates.

The state directory holds only what Shoal keeps to run and to record its runs.
Only Shoal writes, changes or deletes anything there. Shoal never gives the user
or an agent a file there to work with, though it may name its own logs there for
reading. A file the user, an agent or a reviewer works with lives in the
directory of the repository it belongs to, or in `root_dir` when it belongs to
none, in a directory marked as Shoal's.

Creating a workspace, opening a PR and merging start operations, below, whose ID
a client follows for typed progress, then a result or a typed failure. A
subscription sends a snapshot of the records of its topics, then each record
that changed or was removed, comparing again after each stored change, until its
scope token is revoked; a daemon restart closes it and `shoal_client` subscribes
again and reports the new snapshot as a resync.

The daemon raises its open file limit at startup and accepts every connection,
whatever its requests hold. Request/response operations share a fixed limit;
executions, waits and streams take a separate share of the remaining
descriptors, so neither can block the other, and daemon administration takes
neither. A request past the limit is refused at once with the requests that hold
it, and a request that runs past a minute is logged, again when it ends. A
request that only reads state is cancelled when its client hangs up; one that
changes state finishes, so a hangup never leaves it half done. When the
long-lived share is full, a request whose client sends it again after the daemon
closes its connection, as across a restart, is closed unanswered; others are
refused. Accept failures that clear on their own, such as running out of
descriptors, are retried; any other stops the daemon with a log line. Resource
protocol methods use domain-then-verb names matching the CLI operations. The
daemon owns SQLite state, allocation, lifecycle transitions, and recovery.
SQLite runs on one dedicated blocking thread with one reusable connection and a
128-operation queue; callers wait for queue space. Admitted operations run once
even if their caller disconnects, and shutdown drains them before releasing
daemon ownership. Connections retain foreign keys and a five-second busy
timeout; a panic or unfinished transaction discards the connection before the
next operation. Daemon errors retain typed codes and messages on the client
without changing the wire format; unfamiliar codes are preserved verbatim for
version compatibility.

Workspace setup with the post-setup hook, `pr open`'s push and `merge` are
operations: the daemon starts their work in a worker process of its own,
detached as it starts queued agents, so the work goes on when the CLI or app
that asked for it goes. The worker acts with the caller's scope and, from the
CLI, its environment without the secrets an agent launch exported, and runs the
same tracked executions and hooks the CLI ran, under the same checks; what it
and its processes write becomes the operation's output, and it reports typed
steps and its end. A scoped process follows an operation of a space its scope
reaches, and one its own space started whatever became of the space it works in,
as a move's once that space left before the hub lists it at its destination. The
CLI starts an operation and follows it through `shoal_client`, as an app does,
showing its output where it showed it before, so the CLI and apps cannot drift.

Only a planned exit, a restart or an update, keeps work running. The daemon
cannot tell a service command's SIGTERM from a shutdown's, a logout's or a
`kill`, so a signal stops all work with the save-first shutdown stop below, and
the service commands instead ask the daemon over its socket how to exit: `shoal
daemon restart` and an install that replaces an incompatible daemon ask it to
detach its wrappers, and `shoal daemon stop` to stop work as `shoal stop --all`
does, so agents stay down until `shoal resume`, except that queued launches in
existing workspaces stay for the next daemon to start. The request is permanent
like reattachment and answered before the protocol check, since restarting is
how an upgrade reaches a daemon of another release; a daemon that does not
answer gets SIGTERM. Once its work has ended, the daemon answers again on the
same connection with the spaces whose work it stopped and left for `shoal
resume`, a restart's those of the wrappers that could not reattach, which `shoal
daemon stop` and `restart` name in the `shoal resume` they point to, so it
restores nothing the exit did not stop. Work the daemon runs itself, in
background operations, operation workers and connected setup or merges, cannot
detach, so `shoal daemon stop` and `restart` ask the daemon to exit only once
none runs, with the same quiescence an update waits for. That wait happens in
the daemon before it begins to exit, outside the service manager's stop timeout.
Meanwhile it holds back the background passes and requests that would start such
work, whose clients send them again until the next daemon takes them when a
restart brings one; queued launches wait for it too, while restores of running
agents and other requests go on, so agents keep working through the wait.

Use short transactions for atomic claims. Closed Shoal enums with matching display and
wire names use one macro to share explicit spellings across conversions and reject
unknown values. Native simulator states use a
separate adapter enum that preserves unfamiliar strings and native wire spellings;
only confirmed shutdown frees running capacity. Slow operations stay outside
transactions; failures and migrations preserve ownership. `install` preserves compatible daemons and
commands, deferring service changes until restart; incompatible daemons are
verified stopped before replacing their service.

Keep writable state outside the binary's directory. Service setup manages one
per-user launchd/systemd service and refuses to change its state directory until
the existing service is stopped; foreground mode covers environments without a
service manager. The service captures the installing shell's `PATH`. A managed
daemon keeps the installed definition of its state directory current, rewriting
it at startup when its release renders it differently, with the executable and
`PATH` the installed one starts, and reloading systemd without restarting
anything: service managers read only the file `shoal daemon install` wrote, and
a definition in a system directory would need root and a distribution package.

Managed daemons record their stable executable path before startup work and
restart after a replacement, including one during startup, once daemon
operations are idle and no setup or merge runs; commands reattach. Retain the
ownership lock and listening socket across exec so queued requests reach the
replacement under the same service PID. The lock file names the owning process
by PID and birth, so a client refused by the socket tells a live daemon that
does not answer from none running and points to restarting it. Read-only streams
reconnect after the handoff, preserving event cursors, and item waits resume for
their remaining time; when the replacement speaks a newer protocol, they
continue by executing the installed release with that cursor or time. An update
does not wait for clients of the previous protocol. The old daemon refuses newer
requests before running them, so the installed CLI sends them again until the
replacement answers; daemon controls report the mismatch at once. Foreground
daemons keep explicit restart control.

Every space's lifecycle events persist in a separate SQLite journal alongside
lifecycle transitions and cleanup outcomes, survive ownership removal, and keep
increasing IDs. A chat's events go through the same recording path as a
workspace's and take the same shape, naming the space by its ID and kind, with
no event kind of one kind of space for what both record. Retention is per space,
so one busy space cannot push out another's history: a space keeps its events
while it exists and for `cleanup.restore.retention_days` after its removal, as
its checkpoints and attached files, and a restored space, which keeps its ID,
keeps them; only its high-volume events, its agent's turn states, its progress
changes and forge activity other than an item's closing, merging and reopening,
keep a cap of the newest 500 per space. A space's log (`shoal log`,
`client::log::log`) is a filtered view of this journal, not a second store, and
so is Shoal's, every space's events with Shoal's own, which belong to no space
and stay for the global retention: renames, restores, agent starts, stops and
exits, threads, approvals, alerts, and the spaces created and removed below a
space (in that parent's log) are journal events for it, each recorded where the
change is made. A scoped caller reads the logs of its own space and the spaces
below it; a removed space's place in the tree is the one its parent's log
recorded at its removal. Unscoped integrations replay or follow them without
consuming alerts; expired cursors report a gap for resync.

Opening persistence migrates the store, rewriting what it holds into the shapes
this release reads: Shoal promises its earlier versions no compatibility, so
code reads one shape of each record and file, and refuses a package or request
of another version, except from a process an upgrade left running, such as an
agent's wrapper. A database below the store's floor, the schema v0.21.0 wrote,
is refused unchanged, naming that release to start once first, as is one a newer
version wrote. The floor moves to a later release's schema only by hand, which
deletes the migrations at or below it with their tests and names the new release
in the refusal. Under the daemon lock, startup then atomically quarantines
interrupted operations in a separate transaction before ownership auditing,
serving requests, or cleanup; failure aborts startup and leaves committed
migrations available for retry. The daemon binds its socket before this work:
until it ends, requests wait, except that a request its client sends again is
closed unanswered and a status request still waiting after two seconds is
refused with the running step, how long it has run, and a stalled directory or a
check that has waited five seconds; a step that runs past a minute is logged.
Startup audits but never deletes, kills or releases leases, and clears an
unknown execution only when stopped-agent reconciliation proves it ended. The
cleanup sweep rechecks only the workspaces the audit failed and readies one once
its ownership verifies, checking before it reserves the lifecycle so a workspace
that still fails never leaves the failed state; automatic repair goes no
further, since repair would also accept a failed setup the user has not seen.
Survivors are signaled only after verifying PID birth identity and same-user
ownership; acknowledgement cannot override visible live processes. `shoal stop`
asks for it, since an execution without a recorded launch identity cannot be
proven ended even when nothing of it is visible. Recheck unreadable process
identities before treating visibility as incomplete; an identity that has exited
or is exiting no longer blocks the ownership proof, and an inaccessible macOS
identity is treated as a different process because it cannot be a same-user
process Shoal started. One whose exec has not yet published its environment, or
that no ancestor attributes, is read again within a bounded settle wait before
it counts as unreadable, so a short-lived helper launchd adopted while running a
platform binary no longer blocks once it exits or execs a program whose
environment lacks the execution's marker. A reported agent or command exit first
stops its verified processes outside its process group, so background shell
commands do not outlive it; what should outlive a command belongs in a hook or a
demo, and setup instead fails on what it leaves running. A reported command exit
clears its execution and permits setup readiness only when marker, child and
group evidence has no survivors and environment visibility is complete; a
settled empty environment is readable evidence, except a macOS platform
binary's, which reads that way when hidden. A hidden environment takes the
attribution of the nearest ancestor whose environment tells, through any setuid
program the user started, such as a terminal's `login`, which runs as another
user and is never stopped itself. One launchd adopted after its parent exited
has no ancestor left to tell, so the daemon remembers, by unique process ID,
what the latest inventory told about each process's owner, an execution or none,
while a live process can still name it as itself or as its parent. Inventories
take turns, so the latest is the newest, except that an inference never replaces
a reading of the process's own environment. Inventories, and the command lines,
run times, terminals and foreground process groups Shoal shows or checks, read
the kernel's process records, `proc_pidinfo` and `sysctl` on macOS and `/proc`
on Linux, rather than run `ps`: stops, waits and status checks take them often,
each `ps` cost a process start plus more reads than Shoal needs, which skip a
zombie after its first, and Shoal needs no `ps` installed. Such a process takes
what is remembered for it, failing that for its exited parent, and counts as
unreadable only when no inventory saw either with its owner, as for a parent
that exited between scans or before a daemon restart. An unreadable process
counts only against executions of the workspace it works in; one working
elsewhere, such as an orphan of a removed workspace, or whose working directory
cannot be read blocks none. The reporting wrapper may remain alive awaiting
acknowledgement; workspace stopping allows for child registration, stopping and
completion verification; service termination and its command deadlines cover
that drain. Managed stops signal only the daemon through its service manager and
wait for its ownership lock before removing the job, so an already-loaded
service with an older termination limit cannot cut the drain short. Recovery
polls incomplete proof within a separate bounded wait and still refuses live or
unverifiable survivors.
