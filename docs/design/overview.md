# Overview

This file states Shoal's purpose and boundaries, the principles of its
architecture and the work that remains.

Shoal manages local Git workspaces, executions, and shared development
resources for humans and coding agents. Cleanup is part of resource ownership:
a disposable workspace must not leave substantial run-owned data behind.

Agents cooperate with Shoal's scope and allocations. Shoal does not stop a
hostile same-user process from bypassing them through Git, the filesystem, or
direct resource access; filesystem restrictions are future work.

Caller-specific integration stays outside the daemon core: Superlogical owns
terminals. Shoal runs a workspace's service containers beside it through the
runtime's `compose`, a project per workspace, never parsing the Compose file;
the agent and its tools stay on the host. Shoal does not provision tools, manage
browsers or store conversations, and schedules each machine's work there: its
agent queue, which also admits registered repositories' CI jobs, and triggers,
config that starts agents when an event source fires, such as a timer or a
webhook request to the daemon's own listener, which serves nothing else and
which the user exposes through a tunnel of their choice.

`shoal start` and `shoal open` are thin launchers around the generic execution
path; CLI launcher argument arrays are configurable defaults; session and
desktop adapters retain their tool-specific setup. Shoal does not switch the
user's or daemon's login.

One Rust binary provides the CLI, execution wrapper, and daemon. Each state
directory has one daemon shared across repositories, with a private Unix socket
and a versioned JSON protocol; the ordinary installation is per user. Each
protocol message is one frame of at most 64 KiB. A list too large for one frame
is sent in pages, each frame but the last marked as continued; streamed listings
send their frames, then `ok`. Any other reply too large for one frame, even in
pages, such as a thread whose surfaces outgrew one, is sent as pieces of its
JSON, marked as continued in the same way, which the reader joins and parses.
The protocol is the one protocol for the CLI and
for apps. Apps use it through the `shoal-client` library, which holds the
connection the CLI makes, an operation for each request and the handles that
read from a connection. The requests and responses with the types they carry,
and the logic over those types that needs no connection, are `shoal-protocol`'s,
which the daemon and `shoal-core` use. The forge data they carry, such as item
references, CI runs and review threads, comes from `shoal-forge-types`, which
reads it without a request, so a client builds no forge's requests;
`shoal-forge` sends them and depends on it. Config files are parsed and resolved
in `shoal-core`: the protocol carries a target's repository layers as JSON the
caller reads, and the daemon resolves what a launch needs. A layer's `same_as`
references travel in it, taken out of its file before its options are read and
followed once every layer is merged, so an option takes another's final value.
The CLI and the daemon are the `shoal-cli` and `shoal-daemon` crates, which
depend on `shoal-core`, the code they share, which makes no daemon calls, but
not on each other; the CLI also depends on `shoal-client` and on
`shoal-workers`, the execution wrapper and the workers that run in the processes
Shoal starts and call the daemon. The binary-only `shoal` package joins them.
Below the daemon, `shoal-store` holds its SQLite store: the schema, the
migrations and the row mappers and guards every part of the daemon shares. Over
the store, `shoal-daemon-base` holds what every part of the daemon uses: its
log, who sends a request, the approval requests it makes, the operations it
starts in workers of their own, the global config it runs under, the gates that
order conflicting changes, the journal of events, the signals that wake waits
once what they read changes, the alert record and the user's alerts, the numbers
of threads and approval requests, the checks of file access that can stall and
of directories the daemon writes, the proof of the directories Shoal owns, and
how the daemon reaches the forges: the accounts its requests go out as, which
forge each host serves, and what the forge says of the items that block work.
What these read of the rest of the daemon, such as a space's coordinator or a
repository's retention, is an interface the base declares and the daemon
implements, and the daemon's `Manager` holds the base's types as fields. Over
the base, each subsystem of the daemon moves into a crate of its own, starting
with `shoal-leases`, the ports, resource permits, simulators, containers and
secrets workspaces lease: its `Leases` service, which `Manager` holds, owns what
leases keep in memory, such as the order queued requests take capacity in, and
what leases need of the spaces, settings, agents and approvals elsewhere is an
interface the crate declares, which the daemon implements and passes each
operation. `shoal-reviews` follows it with local reviews and the gate agent,
which both decide in one-shot agent runs: its `Reviews` service owns the running
reviews and their checkouts and the gate's sessions, and the agent queue admits
a run through the same kind of interface, as a handle the run holds until it
ends. `shoal-hub` holds the hub role and membership of a hub: its `Hub` service
owns the links, pairing codes, vouches and outbox, and reads the spaces,
permissions, messages and approvals it routes across machines through the
interface; request dispatch, which answers what the hub forwards here, stays in
the daemon. `shoal-runner` holds the triggers and the CI jobs this machine runs:
its `Runner` service fires the triggers from its timer loop and webhook listener
and launches their agents through the interface, as `shoal start` does, and
registers each repository's runner, queues, claims and ends its jobs, while
their workspaces, permits and executions stay with the daemon, which the
interface reaches. `shoal-links` holds the issues, PRs and CI runs spaces link:
its `Links` service keeps the watches' running waits, finds where a request
comes from and the repository it acts in, reads the forge, links items to spaces
under their relations, issues as assigned with their forge assignees, keeps the
PR watches and merge acknowledgements of spaces, polls their items in watches
and in the background, follows the runs of their merges, starts, reads, reruns,
cancels and approves CI runs, lists and deletes the forge's branches, reads
failed check logs and item views, applies actions to linked items as the space's
own changes, with the warnings, bindings and review gate of their merges, moves
the workspaces stacked on a base once its work merges, opens issues and PRs and
reverts merged ones, or proposes them where a permission asks, decides those
proposals and runs the pushes their approval starts, and delivers the activity
to agents, or through the hub to coordinators on other machines, while the
spaces, permission checks, the agents' records and messages, the approval
requests' records and routing, the reviews and the hub stay with the daemon,
which its interface reaches; the approvals subsystem calls the service to decide
a proposal or finish its push, and whether an idle agent is due a wake is the
agent queue's. Its record functions read and write what spaces link, which other
subsystems call in their own transactions. `shoal-repositories` holds the
registered repositories: its `Repositories` service registers and looks them up,
prepares the `root_dir` that holds them, resolves the config layers of
repositories and their spaces, keeps each checkout's default branch in sync,
builds the seeds and starts new workspaces from their snapshots, empties the
shared caches, removes repositories, sweeps finished contributions and moves
repository directories into a new root. The spaces a removal or move takes,
whether a workspace's worktree may go or move with it, where the chats without a
repository keep their files, the subsystems that work and keep files in a
repository's directories and the agents that trust its paths stay outside the
crate, which its interface reaches; so do the hooks' executables and
environments, which read ports and temporary directories, fetching and
fast-forwarding a default branch, which the refresh of a workspace's branch
shares, and whether the host has headroom for a build. `shoal-agents` holds the
agents the daemon runs: its `Agents` service tracks the executions their
wrappers register and relay, admits, starts and restores agents through the
agent queue, stops, restarts and interrupts them, stops them for overload, disk
space and shutdowns, recovers what a daemon restart left, keeps their sessions,
places and stopped work, records the turn state their hooks report, with the
reports a hook left behind, and the subagents they run, retries the turns an API
error ended, queues and delivers the alerts for an agent once a run of them
settles, follows the user's engagement with each agent, wakes the agents idle
for their mode's period, fires the timers of `shoal sleep`, keeps the agents'
tools' trust in a repository's paths as it moves or goes, and holds the machine
awake while agents have work. The spaces agents run in, their lifecycles and
settings, the alerts that tell of them, the leases, CI jobs, demos, merges,
links and conversions that end or wait with an agent, and the snapshot the
queue's policy ranks, which reads the spaces' labels and areas, stay with the
daemon, which its interface reaches, as do, until they move into the crate, the
launches that create a space for an agent. The crate owns the agents' records,
the executions' and the queue's among them, and reads what the spaces and
repositories record through its interface. `shoal-apps` holds what apps built on
Shoal keep in it: its `Apps` service resolves an app's `[apps.<name>]` table
over the config layers its interface reads, and keeps the app store's entries,
writing each only from the revision its writer read, taking a write's bytes
after its request, answering a hub member's requests and waking the watches of
its app's entries. The socket and the hub link, the scope checks of a running
watch, and sending a machine's writes of shared entries to its hub stay with the
daemon's connections. `shoal-demos` holds the demos executions run: its `Demos`
service records each with its execution, lists them, stops them, runs a demo's
command again, and starts, reuses and retires the viewers of simulator demos,
keeping the locks that let a viewer's spawn and its demo's stop never overlap.
The spaces they run in, the executions that run them and stop their processes,
and the ports their viewers lease are its interface's; relaying a client's
stream to a running demo stays with the daemon's connections.
`shoal-coordination` holds what passes between spaces and the user: its
`Coordination` service sends, routes, reads and withdraws alerts, keeps threads
with their posts, code references and the changes agents make for them, records
approval requests and routes each to the space or user that decides it, computes
a space's permissions and grants, and resolves the caller a scope token stands
for and the spaces its scope reaches. The spaces, their files and holds, the
agents and their queue, the hub, the repositories, the reviews and the links it
routes between are its interface's; confining each request to its caller's scope
stays with the daemon's request dispatch. `shoal-spaces` holds the workspaces
and chats: its `Spaces` service keeps their records and the tree they form,
their marks and progress, their worktrees, checkpoints and the files they send,
their lifecycle, removal and the cleanup sweep's own steps, the startup audit of
what they own, and their restores, conversions, moves and launches. The agents,
links, repositories, hub, CI jobs, reviews, leases and demos it works with are
its interface's, which also gives it coordination's service to call; what other
subsystems record of a space it reads and writes in its own transactions through
its interface's records, and reads executions through the store's shared
readers. The cleanup sweep's order across subsystems, the request dispatch with
each request kind's target check, and the connections requests arrive on stay
with the daemon, which gives an operation that streams its answer, such as a
diff, a file or a followed session, the writer its frames go to. The walks up
and down the tree of spaces, and whether a space on the way has an agent, are
`shoal-store`'s, which every subsystem that follows the tree takes in its own
transactions. `shoal-power` reads the machine's power and the user's presence
from the platform: logind's and IOKit's announcements of a sleep, logind's of a
shutdown, the delay locks that hold either off until the daemon is ready, and
the lid, dock and idle readings that say whether the user walked away. What the
daemon does on each, such as holding the agent queue through a sleep or stopping
agents for a shutdown, stays with it. `shoal-daemon` composes them: its
`Manager` holds each service and implements every interface they declare, mostly
by calling another subsystem's service, so each subsystem builds and tests
alone. It keeps request dispatch, with each request's scope and target checks,
the connections and subscriptions requests arrive on, startup and shutdown, and
the work that spans subsystems, such as the cleanup sweep's order, freeing disk
space, clearing blockers and what a sleep or shutdown does. Changing the CLI
rebuilds neither the daemon nor `shoal-core`, changing the daemon neither the
CLI nor `shoal-core`, and changing `shoal-client` or `shoal-workers` neither the
daemon nor its tests: the daemon depends on no crate that calls the daemon, and
the tests that call one through such a crate live in it, starting the daemon
through its `test-support` feature. Commands print only the library's types with
`--json`, which a development build checks read back, so a program that runs
`shoal --json` parses the output with them. The protocol keeps no compatibility
promise: its version and exact-match rule refuse a client of another release
with `protocol_mismatch` naming both versions, and an app updates with Shoal.

Shoal refuses a request only for a very good reason: the request would act
outside the caller's scope or permissions, would lose or damage work, or cannot
be done at all. A limit of how Shoal is built is no such reason, such as a code
path one kind of caller does not take: Shoal supports the request instead.
Nor is a view of where a setting or action belongs, or of what the user ought
to want. A config file takes every option its schema has, except where the
option's value cannot apply there.

The minimum Rust version follows current stable so the code can use its newest
features; the manifest and CI toolchain advance together, with locked
dependencies.

## Remaining work

Implementation order: CLI/daemon, workspaces, ports, simulators, lifecycle
polish, then filesystem restrictions. Open items:

- **Storage policy:** ownership and retention for run data outside the
  worktree other than temporary directories, caches, logs, alerts, kept
  branches and audit history.
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
