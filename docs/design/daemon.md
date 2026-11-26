# Daemon

This file covers the daemon process, held in `shoal-daemon`,
`shoal-daemon-base` and `shoal-store`: its state directory, connections and
limits, operations, composition, store, journal, startup, exits, service and
updates, and how it proves that processes ended. [Daemon](../reference/daemon.md) says what
each command does; this file gives the reasons.

## State directory

The state directory holds only what Shoal keeps to run and to record its runs.
Only Shoal writes, changes or deletes anything there. Shoal never gives the
user or an agent a file there to work with, though it may name its own logs
there for reading.

A file the user, an agent or a reviewer works with lives in the directory of
its repository, or in `root_dir` when it belongs to none. It sits in a
directory marked as Shoal's.

Writable state stays outside the binary's directory, so replacing the binary
never touches it.

## Connections

The daemon accepts every connection, whatever its requests hold. It raises its
open file limit at startup to make room for them. An accept failure that
clears on its own, such as running out of descriptors, is retried. Any other
stops the daemon with a log line.

### Frames

Each protocol message is one frame of at most 64 KiB. A list too large for one
frame goes in pages, and each frame but the last is marked as continued. A
streamed listing sends its frames, then `ok`. Any other reply too large for one
frame goes as pieces of its JSON, marked the same way, such as a thread whose
surfaces outgrew a frame. The reader joins the pieces and parses them.

### Request limits

Requests that answer once share a fixed limit. Executions, waits and streams
take a separate share of the remaining descriptors. Neither kind can thus
block the other. Daemon administration takes neither, so a full daemon can
still be inspected and stopped.

A request past its limit is refused at once, with the requests that hold it.
When the long-lived share is full, a request that its client sends again after
the daemon closed its connection, as across a restart, is closed unanswered.

The daemon logs a request that runs past a minute, and again when it ends.

### Hangups

A request that only reads state is cancelled when its client hangs up. A
request that changes state finishes, so a hangup never leaves it half done.

### Subscriptions

A subscription sends a snapshot of the records of its topics. It then sends
each record that changed or was removed, comparing again after each stored
change, until its scope token is revoked. A daemon restart closes it.
`shoal_client` then subscribes again and reports the new snapshot as a resync.
[Follow changes](../reference/apps.md#follow-changes) says what a subscription
follows.

### Names and errors

Resource protocol methods name the domain, then the verb, as the CLI's
operations do.

The client keeps a daemon error's typed code and message without changing the
wire format. It keeps an unfamiliar code verbatim, so it can show an error
from a daemon of another release.

## Operations

Workspace setup with its post-setup hook, `pr open`'s push and `merge` are
operations. The daemon runs each in a worker process of its own, detached as a
queued agent's start is. The work thus goes on when the CLI or app that asked
for it goes.

A client follows an operation by its ID: its typed steps and output as they
come, then a result or a typed failure. The CLI starts and follows operations
through `shoal_client`, as an app does, so the CLI and apps cannot drift.

The daemon keeps an operation in memory until ten minutes after it ends, so a
client follows it again by its ID from any event. A restart forgets it, and
setup and merges fail with their daemon.

The worker acts with the caller's scope. From the CLI, it also takes the
caller's environment, without the secrets an agent launch exported. It runs
the tracked executions and hooks the CLI ran, under the same checks. What it
and its processes write becomes the operation's output.

A scoped process follows an operation of any space its scope reaches. It also
follows one its own space started, whatever became of the space the operation
works in. A move's operation thus stays visible once its space left this
machine and before the hub lists it at its destination.

## Composition

`shoal-daemon` composes the daemon. Its `Manager` holds each subsystem's
service and implements every interface they declare. The daemon keeps:

- request dispatch, with each request's scope and target checks,
- the connections and subscriptions requests arrive on,
- startup and shutdown,
- the work that spans subsystems, such as the cleanup sweep's order, freeing
  disk space, clearing blockers and what a sleep or shutdown does.

`shoal-daemon-base` holds what more than one subsystem uses. Examples are the
log, who sends a request, the approval requests the daemon makes, the
operations it starts in workers and the global config it runs under. It also
holds the gates that order conflicting changes, the journal, the signals that
wake waits, the alert record and the numbers of threads and approval requests.
It reaches the forges for the daemon: the accounts its requests act as, which
forge serves each host, and what the forge says of the items that block work.
What the base reads of the rest of the daemon, such as a space's coordinator or
a repository's retention, is an interface the base declares. `Manager` holds
the base's types as fields.

`shoal-store` holds the store: its schema, its migrations and the row mappers
and guards that every subsystem shares. It also walks the tree of spaces up and
down and tells whether a space on the way has an agent. Every subsystem that
follows the tree takes these walks in its own transactions.

## Store

The daemon owns the SQLite state, allocation, lifecycle transitions and
recovery.

SQLite runs on one dedicated blocking thread with one reusable connection. Its
queue holds 128 operations, and callers wait for space in it. An admitted
operation runs once, even when its caller disconnects. Shutdown drains the
queue before it releases daemon ownership.

The connection keeps foreign keys on and a five-second busy timeout. A panic or
an unfinished transaction discards the connection before the next operation.

### Transactions

An atomic claim takes a short transaction. Slow operations stay outside
transactions. Failures and migrations keep ownership as it was.

### Closed sets

The `states!` macro gives a closed set of Shoal names one spelling across
serde, `Display`, SQLite and clap, and rejects unknown values everywhere.

### Migrations

Shoal makes its earlier versions no compatibility promise, and the store
migrates only from its floor, as [AGENTS.md](../../AGENTS.md#compatibility)
states. Opening the store therefore migrates it, rewriting what it holds into
the shapes this release reads.

The store refuses, unchanged, a database below its floor, naming the release
to start once first. It refuses a database a newer release wrote the same way.
[After an update](../reference/install.md#after-an-update) names the floor's
release.

## Journal

The journal records every space's lifecycle events in a table of its own in
the store. An event is written in the transaction that makes its change, such
as a lifecycle transition or a cleanup outcome. Events survive the removal of
what they describe, and their IDs keep increasing.

The code that makes a change records its event. A chat's events take the same
recording path and shape as a workspace's, and name the space by its ID and
kind. No event kind belongs to one kind of space for what both record.

Store triggers detect the changes that rows record, such as a rename, a
ready mark or a file's deletion, so that no write path or cascade is missed.
A trigger passes the row it saw to a function the store registers on each of
its connections. The store's journal code builds every event from Shoal's
types and writes it, whether a trigger or the code that made the change
records it. No SQL builds an event's JSON. Queries filter on the columns the
journal code fills from the event, such as its kind, and never read a path
inside a record. A connection without the
function, such as a `sqlite3` shell, reads the store but fails on a change
that a trigger records.

Each kind of event has one shape. Its kind decides the fields it records,
and it records each of them, null when empty. When a kind's shape changes,
a migration rewrites the events stored in the old one.

A space's log is a filtered view of the journal, not a second store, and so is
Shoal's log. Shoal's own events belong to no space. A child's creation and
removal are events of its parent too. A removed space's place in the tree is
the one its parent's log recorded at its removal.

Unscoped integrations replay or follow the journal without consuming alerts.
[Space events](../reference/log.md#space-events) says how they read it and
recover from a gap.

### Journal retention

Each space keeps its own events, so a busy space cannot push out another's
history. A removed space keeps them as long as its checkpoints and attached
files, so a restore brings back its history with them. A cap applies only to
the high-volume kinds of event. [Retention](../reference/log.md#retention)
gives the periods and the cap.

## Startup

The daemon binds its socket before its startup work. Until the work ends,
requests wait instead of finding no daemon. A request its client sends again
is closed unanswered. A status request still waiting after two seconds is
refused with what startup is doing, as
[A daemon that is starting](../reference/daemon.md#a-daemon-that-is-starting)
describes. The daemon logs a step that runs past a minute.

Once the store is migrated, startup marks interrupted operations under the
daemon lock, in a transaction of its own. It does so before it audits
ownership, serves requests or cleans up. A failure aborts startup and leaves committed
migrations in place for the next try. [Startup](../reference/daemon.md#startup)
lists what it marks.

### The startup audit

Startup audits but never deletes, kills or releases leases. It clears an
unknown execution only once the stopped agent's reconciliation proves it
ended, as [Unknown executions](agent-lifecycle.md#unknown-executions)
describes.

The cleanup sweep rechecks only the workspaces the audit failed. It readies
one once its ownership verifies. It checks before it reserves the lifecycle,
so a workspace that still fails never leaves the failed state. Automatic
repair goes no further, since repair would also accept a failed setup the user
has not seen.

## Exits

Only a planned exit, a restart or an update, keeps work running. The daemon
cannot tell a service command's SIGTERM from a shutdown's, a logout's or a
`kill`'s. A signal therefore stops all work with the
[shutdown stop](../reference/daemon.md#signals), which saves each agent first.

### The service commands ask the daemon

The service commands instead ask the daemon over its socket how to exit.
`shoal daemon restart` and an install that replaces an incompatible daemon ask
it to detach its wrappers. `shoal daemon stop` asks it to stop work.
[Daemon restarts](../reference/daemon.md#daemon-restarts) says what each
leaves running.

The request is permanent, as reattachment is: the daemon answers it before the
protocol check, since a restart is how an upgrade reaches a daemon of another
release. A daemon that does not answer gets SIGTERM.

Once its work has ended, the daemon answers again on the same connection. It
names the spaces whose work it stopped and left for `shoal resume`: after a
restart, those whose wrappers could not reattach. The command names them in
the `shoal resume` it prints, so a resume restores nothing the exit did not
stop.

### Work the daemon runs itself

Work the daemon runs itself cannot detach: background operations, operation
workers, and connected setup or merges. `shoal daemon stop` and `restart`
therefore ask the daemon to exit only once none runs, as an update waits.

The daemon waits before it begins to exit, outside the service manager's stop
timeout. Meanwhile it holds back the background passes and requests that
would start such work. Clients send those requests again until the next daemon
takes them, when a restart brings one. Queued launches wait too. Restores of
running agents and other requests go on, so agents keep working through the
wait. [Work the daemon runs itself](../reference/daemon.md#work-the-daemon-runs-itself)
says what a command sees.

### The service manager's stop

A managed stop signals only the daemon, through its service manager. It waits
for the daemon's ownership lock before it removes the job. An already loaded
service with an older termination limit thus cannot cut the drain short.

The service's termination limit and its commands' deadlines cover that drain.
A wrapper that reports its exit may wait for acknowledgement. Stopping a
workspace allows time for a command's child process to register and stop, and
for its completion to verify.

## Service

One per-user launchd or systemd service runs the daemon. Foreground mode
covers environments without a service manager.
[The service](../reference/daemon.md#the-service) says how to install and run
each.

The definition lives in the user's own service directory, since one in a
system directory would need root and a distribution package. Service managers
read only the file that `shoal daemon install` wrote. A managed daemon thus
keeps that file current itself, rewriting it at startup when its release
renders it differently, as
[Keep the service definition current](../reference/daemon.md#keep-the-service-definition-current)
describes.

`shoal daemon install` keeps a compatible daemon and its commands running and
defers service changes until it restarts. It verifies that an incompatible
daemon stopped before it replaces its service.

## Updates

A managed daemon records its stable executable path before its startup work.
It restarts after a replacement of that executable, even one during startup,
once no daemon operation, setup or merge runs. Commands reattach.
[After an update](../reference/install.md#after-an-update) says what the user
sees.

### Requests across a restart

A request holds its operation until its reply is written. An answer that goes
on, such as a stream, holds one only while it writes. A restart thus never
falls between a change and its answer, nor waits for a client that only reads.

The daemon keeps its ownership lock and listening socket across the exec. Queued
requests thus reach the replacement under the same service PID.

The lock file names the owning process by PID and birth. A client the socket
refuses can thus tell a live daemon that does not answer from none running,
and point to restarting it.

Read-only streams reconnect after the handoff and keep their event cursors.
Item waits resume for their remaining time. When the replacement speaks a newer
protocol, they continue by running the installed release with that cursor or
time.

### Protocol mismatches

An update does not wait for the clients of the previous protocol. The old
daemon refuses a newer request before running it, so the installed CLI sends it
again until the replacement answers. Daemon controls report the mismatch at
once.

A foreground daemon restarts only when told to.

## Proof that processes ended

The daemon signals a process, and counts it as ended, only on evidence.
[Process tracking](../reference/daemon.md#process-tracking) and
[Survivors](../reference/daemon.md#survivors) say what the user sees and the
waits each check takes.

### Signals

The daemon signals a survivor only after it verifies the PID's birth identity
and that the process belongs to the same user. An acknowledgement cannot
override a visible live process. `shoal stop` asks for one because an
execution without a recorded launch identity cannot be proven ended, even when
nothing of it is visible.

### Exits

A reported agent or command exit first stops its verified processes outside
its process group, so background shell commands do not outlive it. What should
outlive a command belongs in a hook or a demo. Setup instead fails on what it
leaves running.

A reported command exit clears its execution, and lets setup become ready,
only when the marker, child and group evidence shows no survivor and the
environments it needs are readable. A settled empty environment is readable
evidence. A macOS platform binary's is not, since it reads empty when hidden.

### Unreadable processes

The daemon rechecks an unreadable process identity before it treats its view
of the processes as incomplete:

- An identity that has exited or is exiting no longer blocks the proof.
- An inaccessible macOS identity counts as a different process, since it
  cannot be a same-user process Shoal started.
- A process whose exec has not yet published its environment, or that no
  ancestor attributes, is read again within a bounded wait. A short-lived
  helper that launchd adopted while running a platform binary thus stops
  blocking once it exits, or once it runs a program whose environment lacks
  the execution's marker.

A process with a hidden environment takes the attribution of its nearest
ancestor whose environment tells. That reaches through any setuid program the
user started, such as a terminal's `login`, which runs as another user and is
never stopped itself.

An unreadable process counts only against the executions of the workspace it
works in. One that works elsewhere, such as an orphan of a removed workspace,
blocks none, and neither does one whose working directory cannot be read.

Recovery polls incomplete proof within a separate bounded wait, and still
refuses live or unverifiable survivors.

### Processes launchd adopted

A process launchd adopted after its parent exited has no ancestor left to
tell. The daemon therefore remembers, by unique process ID, what the latest
inventory told of each process's owner: an execution or none. It remembers
this while a live process can still name that process as itself or as its
parent.

Inventories take turns, so the latest is the newest. An inference never
replaces a reading of the process's own environment.

An adopted process takes what is remembered for it, or else for its exited
parent. It counts as unreadable only when no inventory saw either with its
owner, as for a parent that exited between scans or before a daemon restart.

### Kernel process records

Inventories read the kernel's process records: `proc_pidinfo` and `sysctl` on
macOS, and `/proc` on Linux. So do the command lines, run times, terminals and
foreground process groups that Shoal shows or checks. Shoal runs no `ps`:

- Stops, waits and status checks read processes often, and each `ps` cost a
  process start.
- `ps` reads more of each process than Shoal needs, while Shoal reads a
  zombie's record once and skips the rest.
- Shoal needs no `ps` installed.
