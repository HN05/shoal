# Shoal design

Decisions and proposals; usage: [README.md](../README.md), behavior:
[docs/reference.md](reference.md), configuration: [the configuration reference](../crates/shoal-cli/src/cli/commands/configuration/reference.md),
contributor rules: [AGENTS.md](../AGENTS.md).

## Purpose and boundaries

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
browsers or store conversations, and schedules each machine's work there:
its agent queue, which also admits registered repositories' CI jobs, and
triggers, config that starts agents when an event source fires, such as a
timer or a webhook request to the daemon's own listener, which serves nothing
else and which the user exposes through a tunnel of their choice. A trigger is config like every other setting, without commands that
create or change it, except that a contribution's tracked file defines none,
since its repository's maintainers are not the user who runs it; each source is a module behind one interface whose
firings carry a JSON input document, which filters, templates and keys read
without knowing the source, and every firing launches through the operation
`shoal start` uses, as the user, so the queue, `max_heavy_agents` and cleanup apply.
A space starts on the caller's machine, or on the joined machine `shoal start
--machine` names, whose daemon starts it through the same launch; picking a
machine for a start that names none is placement, a proposal. `shoal start` and `shoal open`
are thin launchers around the generic execution path; CLI launcher argument arrays are configurable defaults; session and desktop
adapters retain their tool-specific setup. Shoal does not switch the user's or
daemon's login. `[secrets]` names
user commands that fetch values in the CLI, for an agent's environment at launch
or, for listed tools, through private PATH launchers each time the tool runs;
the daemon fetches only a webhook trigger's, to check each request's
signature, and neither it nor state keeps a value; hooks drop them. A tool secret
may require approval: the launcher fetches it only after the daemon confirms a
workspace grant bound to its command and tools, and fails instead of waiting.
Environment secrets cannot require approval, since they are fetched before the
agent can ask. A setting
that takes its value from outside the config, such as a proxy's upstream key,
shares one form: as written, `{ env }` from the agent's environment, or `{ cmd }`
run like a secret's command when the agent starts, and Shoal prints no written
value. Forge changes Shoal
makes for a workspace act as the agent too, with the host's configured
`agent_token`, and so does the Git of agents and of the pushes Shoal runs for
them over HTTPS to that host, through a credential helper the agent's Git
settings scope to the host after clearing the user's helpers for it, so the
token never enters a file, the environment or argv, and that Git asks before
its first request, ahead of a login of the user's `.netrc` for the host. An agent Git profile
selected the same way reaches tracked agents as Git's environment-level config,
so the agent's commands use it in any repository while the worktree's config and
other executions keep theirs; the push rewrites of the agent's `push` permission
travel the same way. Desktop handoffs have no authentication override
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
the `shoal-client` library, which holds the connection the CLI makes, an
operation for each request and the handles that read from a connection. The
requests and responses with the types they carry, and the logic over those
types that needs no connection, are `shoal-protocol`'s, which the daemon and
`shoal-core` use. The forge data they carry, such as item references, CI
runs and review threads, comes from `shoal-forge-types`, which reads it
without a request, so a client builds no forge's requests; `shoal-forge` sends
them and depends on it. Config files are parsed and resolved in `shoal-core`: the
protocol carries a target's repository layers as JSON the caller reads, and
the daemon resolves what a launch needs. A layer's `same_as` references travel
in it, taken out of its file before its options are read and followed once
every layer is merged, so an option takes another's final value. The CLI and the daemon are the `shoal-cli` and
`shoal-daemon` crates, which depend on `shoal-core`, the code they share,
which makes no daemon calls, but not on each other; the CLI also depends on
`shoal-client` and on `shoal-workers`, the execution wrapper and the workers
that run in the processes Shoal starts and call the daemon. The binary-only
`shoal` package joins them. Below the daemon, `shoal-store` holds its SQLite
store: the schema, the migrations and the row mappers and guards every part of
the daemon shares. A migration runs only the store's code, a copy frozen at the
shape it migrates of anything it once called elsewhere, so a later change
elsewhere never changes what an old migration does. Over the store,
`shoal-daemon-base` holds what every part of the daemon uses: its log, who
sends a request, the approval requests it makes, the global config it runs
under, the gates that order conflicting changes, the journal of events, the
alert record and the user's alerts, the numbers of threads and approval requests,
and the checks of file access that can stall. What these read of the rest of
the daemon, such as a space's coordinator or a repository's retention, is an
interface the base declares and the daemon implements, and the daemon's
`Manager` holds the base's types as fields. Over the base, each subsystem of
the daemon moves into a crate of its own, starting with `shoal-leases`, the
ports, resource permits, simulators, containers and secrets workspaces lease:
its `Leases` service, which `Manager` holds, owns what leases keep in memory,
such as the order queued requests take capacity in, and what leases need of
the spaces, settings, agents and approvals elsewhere is an interface the
crate declares, which the daemon implements and passes each operation.
`shoal-reviews` follows it with local reviews and the gate agent, which both
decide in one-shot agent runs: its `Reviews` service owns the running
reviews and their checkouts and the gate's sessions, and the agent queue
admits a run through the same kind of interface, as a handle the run holds
until it ends. `shoal-hub` holds the hub role and membership of a hub: its
`Hub` service owns the links, pairing codes, vouches and outbox, and reads
the spaces, permissions, messages and approvals it routes across machines
through the interface; request dispatch, which answers what the hub
forwards here, stays in the daemon. `shoal-runner` holds the triggers and
the CI jobs this machine runs: its `Runner` service fires the triggers from
its timer loop and webhook listener and launches their agents through the
interface, as `shoal start` does, and registers each repository's runner,
queues, claims and ends its jobs, while their workspaces, permits and
executions stay with the daemon, which the interface reaches.
`shoal-links` holds the issues, PRs and CI runs spaces link: its `Links`
service keeps the watches' running waits and the forge's remembered
answers, builds the accounts requests go out as, finds where a request
comes from and the repository it acts in, reads the forge, links items
to spaces under their relations, issues as assigned with their forge
assignees, and keeps the PR watches and merge acknowledgements of
spaces, while the spaces, the forge each host serves, permission checks
and the agents' records stay with the daemon, which its interface
reaches. Its record functions read and write what spaces link, which
other subsystems call in their own transactions.
Changing the CLI rebuilds neither the daemon nor `shoal-core`, changing
the daemon neither the CLI nor `shoal-core`, and changing `shoal-client` or
`shoal-workers` neither the daemon nor its tests: the daemon depends on no
crate that calls the daemon, and the tests that call one through such a crate
live in it, starting the daemon through its `test-support` feature. Commands print only the
library's types with `--json`, which a development build checks read back, so
a program that runs `shoal --json` parses the output with them. The protocol
keeps no
compatibility promise: its version and exact-match rule refuse a client of
another release with `protocol_mismatch` naming both versions, and an app
updates with Shoal. An app is the user unless it gives a scope token, which
only the `shoal` command reads from its environment; with one, it is that
workspace's agent, under the scope checks and approvals of its `shoal`
commands. Creating a workspace, opening a PR and merging start
operations, below, whose ID a client follows for typed progress, then a result
or a typed failure. A subscription sends a snapshot of the records of its
topics, then each record that changed or was removed, comparing again after
each stored change, until its scope token is revoked; a daemon restart closes
it and `shoal_client` subscribes again and reports the new snapshot as a
resync.
The hub is a role of the ordinary daemon, `[machine.hub] enabled`, which
the user's other machines join; the hub stays the machine of its own
spaces. One hub per user, without failover and never chained, though
machines are addressed by the ID the hub gives each at pairing, so nothing
assumes one hop. Every machine connects out and keeps one link, opening it
again with backoff; the hub never dials one. The listener serves HTTP and
the link over a WebSocket on loopback by default and handles no TLS, which
a tunnel or proxy in front of it provides; the hub and each member hold
their own Ed25519 keys, exchanged at pairing and proven by both sides
signing a transcript of the other's nonce and an X25519 exchange on every
link, whose keys then encrypt and authenticate each numbered frame, and a
joiner proves its pairing code by an HMAC of that transcript instead of
sending it, so whoever ends TLS can neither pose as a side, use a code, nor
read, change or replay a frame. A revocation counts only when it comes
sealed, and the hub admits a member and revokes one under one lock. The same link runs over any command's standard input and output
through `shoal daemon connect`, which relays them to the daemon's socket;
running commands there as the user proves more than a pairing code, so a
join that way needs none. Link frames are lines or WebSocket messages of at
most 16 MiB. A credential records its kind, a machine or a device, the hub
refuses a revoked one or one another installation's link holds, and
versions match exactly: the hub refuses a member of another release,
reading only the protocol and version of its first request, with its own
version signed over the member's nonce, and records an alert naming the
update, while the member keeps working on its own. A member follows the
hub's version once that signature checks with the key it paired with, so
nothing between them chooses its release: a downloaded installation installs
exactly that release, older or newer, through `shoal update`'s checks, unless
`[machine.hub] auto_update` is off or the release is too old to read the
state, whose schema the signature also covers, and the daemon's restart on a replaced
binary carries its agents through; a Homebrew one is only told to upgrade,
since one package manager owns each installation. Pairing codes live
in the hub's memory and admit one join within ten minutes. Each machine
owns its spaces' records and reports a summary of each to the hub as it
changes, all of them whenever its link opens; the hub keeps a directory of
those summaries, not copies of the records, and serves one view of every
machine's spaces from it, keeping an unreachable machine's with when it last
heard from it. Repositories match across machines by their origin, never by
each machine's ID. The directory lists a space ID under the first machine
that reported it: another machine's copy is left out, routed nothing, and
alerts the user, and takes the ID once the first's space ends, while an
import claims the ID, so a move hands it over without a conflict; a restore
asks the hub first and refuses a space another
machine runs, going on while the hub cannot be asked. Space numbers form
one sequence across the hub's machines, which the hub holds: it answers
each report with a few spares it keeps for that machine, which numbers a
new space from them without waiting for the hub, and once none is left, as
while the hub cannot be reached, above its own highest. A space whose
number another machine's space or spares held first, as after that or after
joining with spaces of its own, takes the smallest number none holds, with
its threads and approval requests, and its machine tells the user and the
space's agent; the space that held the number first keeps it. A machine
reports the numbers it holds without a space, those of a removed space's
issue proposals, with its spaces, and the hub gives them no other space. An import's
claim of a space's ID also keeps its number for the claiming machine while
the hub still lists the space, so `shoal move --machine` and `--ssh` keep it; an exported
package imported after its source is gone takes a new one. A request for
a space another machine owns goes from the caller's daemon through the hub
to that machine, which decides it as its own caller's: no command is
special on the hub. Each machine vouches for its own callers, so its
unscoped callers' requests and a device's go as the user's and a space's
process's with that space's ID, the reach of its scope and its effective
permissions, which the owning machine checks through the operation every
scope check takes, under single-user trust as scope is cooperative; the
owning machine answers it on its own socket under a token standing for that
caller, whose vouch decides that request alone, never another of the same
space's, and never forwards a forwarded request again. A request that can
cross machines names a space, or an item of a space, never a handle only
one machine knows, such as an execution ID, and a long wait is state on the
owning daemon, never a connection held open or a client polling across the
hub. A command's selection is this machine's spaces, and the picker of a
command whose requests reach a space wherever it runs offers the other
machines' spaces from the hub's directory after them; `--machine NAME|ID`,
which repeats, makes it the named machines' spaces from the hub's
directory, a selector that, as `--all` does, takes no named spaces, and
narrows a picker to them, while a named space is selected wherever it runs.
A request whose
answer goes on, as following an agent's session does, comes back on a byte
stream as that machine's daemon writes it, failing at once while the machine
is offline. Streams run on each link beside its calls, many at once, opened
by the side that reads them, and each holds its writer to the credit its
reader granted as it took what came, so a reader that stalls holds up
neither the calls nor the other streams; the hub relays a stream from one
link to the other the same way. Following a session sends its files' bytes
where the agent's tool keeps them, everything so far and then what the tool
appends, unread: apps parse each tool's format, so a tool changing it changes
only the app. A diff of another machine's workspace streams what that
machine's Git writes under its own config for the repository, run as a plain
read that is no use of the workspace, while the caller's Git decides its
colour, width and pager, as it would for a diff run there. Attaching to
another machine's session runs Shoal's own attach there, as the user, on a
terminal that machine makes at the viewer's size, so waking, restoring,
choosing a session and the host's attach stay one code path; a host callback
decides how a place shows to another machine, a client of the host on that
terminal by default, and for Rex, which has none, the viewer's own Rex app,
found by the label it reports. The viewer's keys go there unread, the host's
detach key included, so Shoal has no detach key of its own; a dropped link
hangs that terminal up, leaving the session running, and the viewer attaches
again to the session at the same place, without waking or restoring twice. Apps keep their data in Shoal without Shoal knowing them: every
daemon keeps an app store of entries per app, bytes with a content type under
path-like keys, shared across the user's machines and devices or one
device's own, in its state directory and reached only through the protocol;
Shoal never reads them. Each change takes a revision from one counter, and a
write or deletion names the revision it replaces and is refused once another
change took the entry past it, so devices never silently overwrite each
other; a watch of a key prefix compares again after each change. Without a
hub the local daemon keeps everything, so one PC needs none, and turning the
hub on keeps its entries since it is the same daemon; with one, a joined
machine keeps its own entries and sends requests of shared ones to the hub,
over a stream of its link for an answer that goes on, an entry's bytes or a
watch, while the shared entries it held before it joined stay local. The
hub keeps each device's own entries by its member ID. Limits per entry and
per app come from `[machine.app_store]`; a request fits one frame, so an
entry's bytes follow its write as frames of their own. An app's settings
the user edits are its `[apps.<name>]` table in any config layer, which
Shoal checks only as TOML, layers as it layers its own options and hands to
the app. The hub forwards requests under a `[machine.hub] forward` path
prefix to a local address, such as an app's own server, a device-paired
client that is never part of Shoal, once a paired member signed the request
with a fresh time and an unused nonce. A request that needs
the space's files or processes stays on its machine and is refused
elsewhere, naming it. `shoal start --machine` forwards its launch through
the hub to the machine it names, whose daemon starts it as its own caller's,
so the space's queue, admission, cleanup and leases are that machine's, as
are its mode and the kind of space it implies, which the caller's machine
reads of that machine's modes through the hub, naming the repository by its
origin and sending the config it tracks, which that machine reads them
under where it has none of the repository; the launch carries its parent by ID, its repository by its origin's URL, which
that machine registers as a URL clone where it has none, and its base as a
branch or commit of that origin, or, for a base only the caller's machine
has, as a commit it carries first in a Git bundle of the history the
origin's default branch lacks, over a duplex stream of the hub's links,
which that machine fetches under a ref of Shoal's own and deletes once
nothing needs the commit: its launch failed, or no workspace there waits
to start from it or records it as a base that no branch holds. A start
below another machine's space, on whichever machine, in a repository that
machine has not registered, clones the URL of its origin, which the
parent's machine reports to the hub beside the key repositories match by,
and reads its modes under the config that repository tracks, which the
parent's machine reads for it. A device's start, which is on no machine, and its read of the modes it may start in go to the machine its launch
names, else to the hub's `[machine.hub] default_machine`, else to the
machine the device sent it to. The space tree may span
machines, as when a space moves without its parent or starts elsewhere: what a space passes up reaches the nearest space
above with an agent wherever it runs, through the first space above that
runs elsewhere, whose machine goes on from there; the item activity of a
space without an agent goes as messages its machine's background poll
sends while the hub lists an agent above, each fitting a share of the
coordinator's unread messages, and stays on the space's cursor until the
hub takes it, and approval requests
take deciders on the machines above after the local ones. The hub holds
what can wait for an offline machine, messages, answers, decisions, stops,
events and the removal someone asked for of a space below one they remove,
which keeps that space until it goes, in order until it connects, for seven days, after which the
sender hears it expired; anything else fails at once, naming the machine
and when the hub last saw it, and a machine whose link is down keeps what
its spaces pass up until it opens. A thread or approval request that
waits `[machine.hub] escalate_after_minutes` for a coordinator whose
machine is offline passes to the next space above with an agent on a
connected machine, or to the user. Each machine is `always_on` or
`intermittent`, which `[machine] availability` sets or else the hub guesses
from its link history alone, never from what powers the machine:
`intermittent` below 95% of the last 7 days connected, gaps under 5 minutes
not counting, and for its first day by whether it reports a battery, so a new
portable machine takes no lasting work before the hub has seen it behave. The
hub keeps that history in its own state, counts its own machine from when its
daemon ran with the machine awake, and tells each machine its availability
when its link opens and when it changes, since a machine acts on it. The
numbers are fixed, not settings. A machine also tells the hub which agent
tools it has installed, when its link opens and as its config changes them,
which the hub keeps for its open link alone, since only a connected machine
starts work. Browser sessions are a later step.
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
Workspace JSON records expose canonical linked-item URLs under `links`, with an
`issues` list of assigned issues in the order they were assigned, a list of
watched PRs, and `opened` and `related` lists, each item of these lists with
its kind, URL and known title; acknowledgement-only PR cleanup records have no URL to
expose. Linking the same item again records a newly supplied title, and the
watches' and background poll's reads of an item record the title they show.
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
it through `shoal_client`, as an app does, showing its output where it showed
it before, so the CLI and apps cannot drift. `shoal start`, `create` and apps open a
workspace and start its preparation through one request, `StartWorkspace`,
`StartCreation` or `StartChild`, which take the steps every new space's start takes, whatever
its kind: it refuses an agent that cannot run, an issue that is not open or
a branch that is not a literal Git branch name before creating anything, records
a new workspace, `preparing` without its worktree, or creates a child's with
it, or reopens the one on the branch, PR head or record
it names, assigns the issue, removing a new space it cannot assign, queues
the detached agent the request asks for, with its mode, model, arguments,
fork, issue and caller's place, as a scoped `start --agent` starts its agent,
which waits in the queue for the preparation, and starts the operation, which
creates the worktree, sets it up, links its PR and runs a new workspace's
post-setup hook. A preparation that creates a new worktree waits first, in a
record beside the workspace, for the blockers of the agent's launch, then
creates it on the branch of the done blocker that holds the others' work, or
from the default branch when none leaves one, so it never starts behind; a
start nothing follows also waits while disk protection holds back new starts.
The queue's dispatcher starts it once that allows, in the scope of the space
that started it, and a stopped agent leaves it waiting for an explicit
preparation. Before any process runs an admitted launch's agent, the daemon
opens its workspace for it: the workspace must still be its ready, verified
worktree, a branch that holds nothing of its own fast-forwards to what it
builds on, as `shoal refresh` does, so its agent starts on a current branch,
and the PR the start names is linked, a refusal warning the starter; a copy
is made when its preparation runs, which waited for what it waits for.
A command, merge or unmerge whose start falls in such an update, or in any
lifecycle operation that returns the workspace to ready or failed on its own,
waits for it to end and is prepared again for the workspace it leaves: the
window is Shoal's own, not a reason the command cannot run. The store counts
each workspace's lifecycle changes, so a start whose registration fails after
such an operation came and went since its first look, which can leave the
worktree unverifiable meanwhile, is prepared again too. Removal, a failure
and setup, which may wait for other work first, still refuse it, as
such an operation refuses an agent's registration, which holds the queue's
admission that a stop of its workspace needs.
The request answers once the workspace is recorded, so a
caller gets its number and its agent's entry without waiting for Git or
setup; until its worktree exists, what reads it refuses the workspace, and
removal takes only its record and leases. The way the start's caller hears
other news it does not wait for, a failed preparation reaches the agent of
the nearest space above the workspace with one, or else the user, naming
the restore that retries it and the removal. Every other creation, a split, a
conversion, a restore, a copy, a commit or CI job's workspace and `adopt
--copy`, also records the plan of its worktree with the workspace and creates
it through the same path, only at once, so a failure at any step leaves a
workspace the restore retries. A workspace's record says whether Git created
its worktree: a pending one keeps its plan and what an earlier try recorded,
and the store refuses a created one without its base and admin directory.
Git creates a worktree before Shoal records its admin directory, so a retry or
removal of a pending workspace without one takes the admin directory whose
`gitdir` names its path, unless another workspace marked it; one without has
no worktree. An add Git still marks as under way was cut short before its
checkout finished, so a retry deletes it and adds the worktree again while its
directory holds only the `.git` link the add wrote, and refuses while it holds
files, which may be someone's work; a retry that writes files into a
worktree an earlier try left keeps what it holds in a checkpoint first. A
restore whose worktree fails still opens the space with its links and agents,
the workspace failed, so the retry needs nothing of the removal record. Only a created worktree is checked for a move when it goes
missing, so after a `git worktree move` during a cut-short creation `shoal rm`
forgets the record, keeping the branch and the moved files. The request names an agent, mode or
model only where the caller chose one; the daemon resolves the rest under the
workspace's settings, `agent.default` for the agent, and the agent works on the
start's issue unless the request gives it an issue or a task, so an app needs
no configuration of its own to start one as the CLI does. `QueueAgent` queues
the worker of an existing workspace the same way, on the workspace's issue.
Every start of a new workspace or of an existing workspace's agent, whoever
makes it, goes through one request, `Launch`, which carries
what `shoal start` takes, as its command line gave it, and what another
command's start adds, such as the subject a repository's setup gives its
agent and the setup mode, which no other launch starts in; the daemon records a
client's launch as `manual`, and names the source of a launch it makes
itself. The
daemon alone decides what the launch leaves out, with the steps in
`shoal-core`: the mode, the kind of space and the repository,
which the launch names, its issue or link belongs to, or the checkout or
worktree the start was made in has; and the branch a PR, a branch link, the
typed name or earlier work on its issue settles. It then queues the worker of
the workspace `--in` names, or of the one that has the branch, as
`QueueAgent` does; or starts a new space, a chat for a launch without a
branch or issue, through the steps `StartWorkspace` takes: the kind decides
only where the space goes, a chat's checkout, workspace worktree or
`root_dir` or a workspace's new worktree, and the daemon reads the issue
the same way for both, resolves the agent and mode under the settings
there, names the space as the launch names it, else after its issue's or
PR's title or where a chat runs, records it, links the issue, queues its
agent through the queue's one request, waiting for what blocks it, and
prepares a workspace, the only step that depends on the kind. Every launch
answers with the space it started in, of either kind, as it created or found it, and
how it started the agent there, so a caller reads the space without asking
its kind; a background place's worker reports only where the place runs. The
queue entry of the
launch's agent carries its source to the agent's start, which the journal
records with it. The CLI parses the start, picks on a terminal what a picker
picks there, adds what only its process knows, such as its directory, its
environment and the session `--fork` forks, and what only its terminal can
check, such as a host to place the agent in; it keeps what attaches a
terminal: a start here, a focused start that opens its place beside the
terminal and shows the preparation there, and an agent that runs its own way,
such as Happy's, which starts in the caller's process for a chat as for a
workspace. Such a launch says so with `here`: the daemon queues the agent as
for any start and keeps its entry for the caller, which holds it while it
follows the preparation and then claims it; the claim starts the agent in the
caller at once where the start asked for that, else when the limits admit it,
and otherwise leaves it to the queue's background start. Every queue entry for
a new workspace belongs to its recorded space, and the queue's start runs the
agent in it. Any other start reports the recorded workspace and its agent's
entry without following the preparation, unless `--wait` asks it to. One
`shoal_client` call sends the request and follows the preparation, returning
the prepared workspace or the typed failure with the workspace as it left it,
ready when only what follows setup failed. The CLI makes that call and keeps its prompt for a failed setup
and an agent it launches in the terminal. An interrupt or termination of the CLI cancels the operation, which
interrupts the worker as it interrupted the CLI; a CLI that ends otherwise leaves
it running. The daemon keeps an operation in memory until ten minutes after it
ends, so a client follows it again by its ID from any event; a restart forgets
it, and setup and merges fail with their daemon as before.

An issue, PR or CI run is read once, where it enters, into an item reference
of its repository, kind and number: a command's argument, a protocol field, a
stored link as it loads, or a forge's answer. Code past that point passes the
reference and never reads its text again. Stores key items by their canonical
URL text, read back into a reference as each row loads. A number names an item
only once the repository it belongs to is known.

The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups. It retains buffered daemon controls across start, stop,
and recovery transitions so adjacent frames cannot be lost. A command takes the
terminal as its foreground job before it runs, without waiting for its
registration, so its first read or terminal setting never stops it. The wrapper
starts it with the default stops for background terminal use, whatever the
wrapper inherited, so a command that uses the terminal before it has it waits
instead of failing. After commands it
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
a daemon of another release; a daemon that does not answer gets SIGTERM. Once
its work has ended, the daemon answers again on the same connection with the
spaces whose work it stopped and left for `shoal resume`, a restart's those
of the wrappers that could not reattach, which `shoal daemon stop` and
`restart` name in the `shoal resume` they point to, so it restores nothing
the exit did not stop.
Work the daemon runs itself, in background operations, operation workers and
connected setup or merges, cannot detach, so `shoal daemon stop` and `restart`
ask the daemon to exit only once none runs, with the same quiescence an update
waits for. That wait happens in the daemon before it begins to exit, outside
the service manager's stop timeout. Meanwhile it holds back the background
passes and requests that would start such work, whose clients send them again
until the next daemon takes them when a restart brings one; queued launches
wait for it too, while restores of running agents and other requests go on,
so agents keep working through the wait.

Overload protection is configured machine-wide: memory is enabled by default,
CPU is opt-in and requires sustained aggregate busy time, which leaves out
niced time: more work on a busy CPU only runs slower, and work at low priority,
such as CI jobs or the user's own niced work, takes only cores nothing else
wants. One `load` setting
(`light`, `balanced`, `full`) sets the default CPU and memory thresholds, so a
user chooses how hard to load the machine without tuning each percentage;
explicit thresholds override it. On Linux, memory protection acts on stall
time, not only on available memory: with swap, the kernel keeps
`MemAvailable` just short of any threshold while swap fills and the machine
thrashes, so the threshold came with the OOM, if at all. Pressure stall
information is the main signal; a measure of RAM and swap in use together is
the backstop, for an allocation too fast to stall first and for kernels
without it. Either signal counts: critical when either is, safe only when both
are. Stall stops work only once it has held, so one burst stops nothing. Protection stops work in one stop order, by
what a stop loses, which `shoal-schedule` computes beside the queue order from
the same snapshot, so one policy defines both: CI jobs first, newest first,
since a rerun loses nothing and the newest has done the least; then demos,
which Shoal restarts; then connected tracked agents, stopped through their
execution wrappers, preserving work and leases. An agent comes only after
every running agent below it in the space tree, light ones included, so a
coordinator never stops while its children work. Among the agents free to
go, heavy ones go before light ones, then the queue's rank reversed: the
agent the user interacted with longest ago, or
never, first, since the user is likely working with the one they touched last,
then idle ones, since an idle agent loses no
turn, and working ones only when no idle one is left, each the lowest priority
and rank first, so the work the queue would start last gives way first; within
a space the agent idle longest, or the newest working one. The daemon, places
and workers are never in it. Each consumer takes the first entry that frees what
it needs: load the first holding memory worth freeing, passing over a light
agent or a space with negligible memory for its own sake; disk every agent;
making room an idle agent with a slot and no running agent below it, since
putting an idle agent to sleep must never stop working ones. Stopping an agent
stops every running agent below it first. A CI job load stops ends as one whose
runner outlived its run does and reruns on its forge once readings recover,
through the queue's normal admission, so load never costs a job, only time. On
Linux the kernel's OOM killer follows the same order: it picks its victim by
`oom_score_adj`, which left alone put the daemon's service at 200 above agents'
builds at 100, so it killed a small shoal process before a large rustc. The
daemon maps places in the order onto scores from 900 down to 200 and lowers its
own to the floor its service manager allows without privilege, which differs
between machines, so the unit cannot state one; a CI job's scope has the kernel
kill the whole job, which fails anyway, and so frees all of it. A user's
`memory_limit` caps Shoal's work, not the machine: on Linux the kernel
enforces it on `shoal-work.slice`, with `MemoryHigh` at the limit, so work
above it is throttled, reclaimed and swapped out before anything dies, and
`MemoryMax` a twentieth above it as the last resort, whose kills stay inside the slice, so
the desktop and the daemon keep their memory whatever work does. The daemon
sets both at runtime rather than in a unit file, so the cap follows `shoal
config reload` and nothing outlives the boot. Protection and admission count
the limit as the machine's memory, and read the slice's own stall, so Shoal
warns and stops work before the kernel throttles it; the slice's memory
leaves out its page cache, which throttling reclaims first and which would
otherwise read as full. macOS has no such control, so there the limit holds
only through protection and admission, on the usage Shoal samples. The
wrapper, not the daemon, reads its command's scope, since it learns of the
command's end first, and polls it rather than watching it: a scope the kernel
kills whole is gone in moments however it is watched, so a SIGKILL that comes
with a kill counted on the slice since the last read names the kill instead.
A scope's peak is read as what it holds beside its page cache, not from
`memory.peak`, which counts the cache a build reads and would make every job
look as large as the files it touched. Whether a terminal
shows an agent plays no part, since a host such as Herdr may count every agent
as shown and a user who attaches may walk away. An idle agent with a running subagent, which
its hooks report, or processes outside its process group counts as working,
since their completion may start its next turn, and so does one without turn
reports. A subagent whose stop is lost counts until the agent's session starts
again or it exits, or, where the agent's proxy has passed its subagents'
requests in the current session, until none has for an hour, which leaves room for a long tool call. Its hook waiting for the next
update runs there too, marked so that it does not count. Once the
wrapper reports the stop, the daemon also stops the agent's identity-verified
processes outside its process group, such as background shell commands, since
automatic and manual restores both require the execution proven stopped. When
the proof fails, the waiting wrapper stays connected and restores once it holds,
instead of leaving an unknown execution. Stop and exit
alerts carry the pressure reason and recovery path, distinguishing automatic
restore from manual recovery and missing recovery records. Persist a minimal
agent recovery record with the stop reason before delivering the stop, so a lost wrapper does not lose
the handoff; failure to save warns without disabling overload protection. Agent
session recovery is automatic for built-in Codex and Claude resume commands, or
when explicitly configured for another agent, and load and disk space have
recovered, except that an agent stopped between turns has nothing to do until
an update arrives for it, so that update restores it instead, and its restore
holds no heavy slot until then. `shoal resume` of an agent whose wrapper
waits is the user asking for it, so it uses the wrapper's queued restore,
which then waits only for a heavy slot, rather than refusing the agent as running
or starting a second beside it. A restore, manual or automatic, of an agent stopped mid-turn, or
whose turn is unknown, prompts it to continue its unfinished assignment, naming when and why it
stopped, which every stop that saves an agent records and a last session keeps, and that its
subagents and background commands ended. One that stopped or exited on its own between turns
is not told to continue: its session opens with only the handoff it needs, or with the wake
prompt whose hook delivers the updates that wait for it, or with no prompt, and waits.
Configured restore commands receive the prompt through `{prompt}`, which an argument of
exactly `{prompt}` leaves out when there is none. Never replay the original task prompt. Keep waiting wrappers tracked; they reattach
like running commands, so a daemon restart or crash hands the wait to the next
daemon instead of cancelling the restore. A waiting wrapper's session shows
the agent asleep under what it last showed, and Enter there starts its queued restore, as the user at
the agent's place asks for it; the wrapper asks over its own connection,
which only that execution's wrapper holds, so a scope token it inherited
does not decide it. `attach` to that session starts the restore
too and attaches once the agent runs, under every place recorded for it, and its pickers offer the
workspace and that session as stopped. `attach` wakes an agent whatever the
limits and rank, since the user asks for it: without a session showing an
agent, it starts the restore an asleep agent waits for, and without any
session left, where only a restore gives it something to show, it restores a
stopped agent as `shoal resume --now` does, through that restore, so it never starts a
second execution beside a waiting one. A daemon update waits for a stop
and a restore waiting for load, but not through a wait for an update, which
may last indefinitely; that wrapper reattaches to the updated daemon. Restores wait in the agent queue,
and manual recovery persists across restarts. Saved recovery represents
unfinished work until restored or explicitly forgotten, except that a done
assignment needs no agent, so a done workspace's agent is restored neither
automatically nor by `shoal resume --all`, only by resuming that workspace.
Resuming it says the work is not finished, so `shoal resume` withdraws the
completion, whichever wrapper restores the agent, and a protection stop
restores it again; Shoal's own restores and restarts keep it.
An idle heavy agent, a workspace's or a chat's, sleeps once it has waited for
the user: Shoal stops it as protection stops an agent between turns, and an
update restores it although its assignment may be done, since Shoal rather than
the agent ended it. Load plays no part, so the recovery policy does not apply.
What keeps an idle agent running is the user working with it, not its prompt
cache: a session's second request misses the cache whichever process sends it,
and every later one, the first after a resume included, reads the whole
history, so stopping and restoring an agent costs no cache that keeping it
would save. The user interacts with an agent by a key it reads in its
terminal, which its wrapper finds by the terminal's access time (reading a key
moves it, the agent's output does not), leaving out the keys Shoal types
itself, which Shoal reports before it types; by a prompt they submit; and by
`shoal attach`, following the agent's session, a message they send it, a
restore or restart they start, `shoal start --in` and Enter on the sleep
screen, which the daemon tells by the unscoped caller, whatever machine or
device it came from. Updates,
Shoal's and a coordinator's messages, idle wakes and what Shoal or a
coordinator starts are not
interaction, nor is a terminal showing the agent, since a user may attach and
walk away. The user is engaged with an agent while they interacted since its
turn before the last ended, so a turn whose wait passes without them ends it,
and later turns updates start wait only the floor. An agent the user is not
engaged with sleeps after a floor of seconds, which lets hooks settle and a
follow-up turn start; one they are engaged with waits twice their reply time,
from the later of the turn's end and their last interaction, between the floor
and `[sleep] max_wait_minutes`, which an agent waits without a reply time. The
reply time is the average, over the space's last turns the user was engaged
with, from a turn's end to their first interaction after it, the first key
rather than the prompt so that reading counts, each counted at most the longest
wait, so that one return after hours does not decide the waits after it; a
space without one takes the user's average over every space. Timers the agent
keeps itself would end with its stop, so Claude Code's timer tools are refused
by a hook that reads only the tool's name and names `shoal sleep`'s timer, and
the model, also in a `/loop` the user types, uses the one Shoal keeps. A light
agent uses too little of the machine to be worth a restore, so it never sleeps
for being idle, and `shoal sleep` only ends its idle wakes. A restore resumes
the session from its launch arguments, so what the user changed inside it, such
as its permission mode, approvals for the session or its model, does not carry
over, and Shoal does not carry it.
An agent Shoal stopped on its own and brings back on its own is asleep, and one
that waits for `shoal resume` is stopped, so the user tells an agent that comes
back from one that needs them. Every stop that queues a restore Shoal starts on its
own puts the agent to sleep: protection, idle and blocked stops, signal stops
and making room. `shoal stop`, an agent that quits on its own and
the agent of a removed or converted space are stopped, and a persistent stop
stops the spaces below first, while a parent may sleep before its children,
since their reports wake it. The asleep state is the restore's own: the
restore keeps why the agent sleeps and what wakes it, so lists, status and the
inbox read it without the recovery records, and `shoal attach`, the user
asking for the agent, starts that restore at once. `shoal sleep` is the
agent saying it has nothing to do until something arrives, so it ends the
agent's idle wakes, and it takes the agent's timer, since a timer says the same until a time: one per space, which a
new one replaces, kept as daemon state through stops, restores and restarts
until it fires as an alert, `--undo` cancels it or the space is done or
removed. A loop sets its next timer each turn, so Shoal parses no recurring
schedule. Queued work that
needs a slot or memory may also put an idle agent to sleep before its wait
ends, or where `[sleep]` keeps idle agents running, lowest rank and priority
first and never above the entry's priority, but never one the user is engaged
with, since nothing overrides an engaged user, and never a light one, which
frees too little. An agent nothing will wake sleeps too, since the user's attach
or Enter on its sleep screen wakes it. Schedules only order the queue. A woken agent reports no idle turn until it has worked
one, so it never sleeps again before it acts on what woke it. A space's reported turn state counts the turns its
agent starts, a count that continues for a restored agent or another
execution, and a protection stop is recorded by execution with the count it
was decided at, until the agent runs again or its restore is cancelled. A
stop chosen between turns records itself only while the agent is still idle
in the turn it was chosen in, and a report that starts a turn records none
once a stop is recorded, leaving its update for the restore: each decides in
one transaction, so whichever commits first wins, and the stop reaches the
wrapper only after its record commits. A load or disk stop the store cannot
record, as on a full disk, still stops the agent, as `shoal stop` does,
leaving it for `shoal resume`. A workspace with several agents keeps
the turn of the one that reported last, leaving the others' unknown, which
counts as working. A record an earlier daemon left authorizes no signal: a
reattaching wrapper restores it or, when its restore no longer waits, it is
forgotten. A
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
execution's agent name for that. An unknown execution without a record or a
recorded child or process group never started its command, such as a setup whose wrapper left
first; the daemon forgets it once no process at its wrapper's PID has the
recorded birth and none of its processes survives, and rechecks a live wrapper
when it exits.
Agent metadata is transient, so restart cannot select disconnected survivors.
`shoal restart` starts each agent in a new wrapper: the daemon sends the stop
protection sends, with the reason `restarted by shoal restart` and recovery
whatever the overload policy, and once the agent's processes are proven
stopped, cancels the wrapper's restore, which every release finishes on with
the agent's record kept. The record becomes no last session, and its end no
exit alert; the daemon then records the record's restore as admitted
and starts it through the queue start worker, as `shoal resume --now` does,
in the terminal host of the agent's recorded place. The new wrapper runs in a
new process tree, so a restart recovers an agent whose wrapper's tree broke,
such as one that lost access to an external volume. The record keeps the
agent's slot until the admitted restore takes it over, so the restart neither
queues behind launches nor loses the slot to them; a start that fails or
leaves the queue before the agent registers fails the restart, and a stop
or completion that cancels the workspace's restores cancels it under the
queue gate the restart records its start under, leaving the record for
`shoal resume` either way. The restart, with the runs that asked for it,
follows the agent to the execution the new wrapper registers, which answers
it once the agent starts with the place that execution registered, so the run
can name where the agent runs now. `--in-place` restarts through the same wrapper,
which keeps the agent in its terminal: the daemon resumes the agent once its
processes are proven stopped, outside the agent queue. A chat's agent
restarts through the same path. The daemon owns restarts: a request names a space, whose agent the daemon
picks, and answers once the daemon restarted it, recorded the restart for
later or skipped it, naming why, such as a protection stop that `shoal queue
start` ends at once. Every restart is a store record of its space, how it
restarts, its permission change and who asked, so it outlives the request and
the daemon: the turn ends watcher that conversions use acts on one that waits
for the agent's turn to end or for its place in the machine's line of
sequential restarts, and a failure with no request left to answer reaches the
coordinator that asked or the user. Once the daemon begins it, at once or when
the watcher acts on it, the record stays until the agent's restore applies its
options, as the wrapper resumes the agent in place or a new wrapper registers
it, also while the restart waits for the stopped agent's processes: each wrapper that reattaches to a later daemon
meanwhile continues it with them, and one that reattaches still running the
agent, without the stop, lets the restart begin again. Lists, status and
`shoal restart --cancel` take only a restart not begun yet. A newer request
supersedes the earlier one as if that ran to its end first: permission changes merge key by key, the
newer winning, while when and how come from the newer; a restart under way
takes the newer request until its wrapper settles how and with what
permissions it starts the agent, and after that the newer one is recorded and
runs once the agent does. An agent between its wrappers is listed as the
execution it ran as, and one a new wrapper runs with the executions it ran
as. The
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
The daemon restarts agents together by default; `--sequential` spreads the
load of starting them through its line, starting the next once the restarted
agent runs and at least the admission stagger after the previous start.
Agents without a restore command and executions whose wrapper is gone are left
to `shoal stop` and `shoal resume`; a restart that cannot start the agent leaves
its record for `shoal resume`, naming the space whose process asked for it,
which may then resume the agent as after its own `shoal stop`.
`shoal stop --interrupt` redirects an agent without stopping it: the agent's
own interrupt key cancels its turn and keeps its process, background shells
and subagents, which a stop and resume would end and start again. The CLI types
the key through the terminal host of the place the daemon recorded for the
agent, a workspace's or a chat's, so a client driving Shoal need not know each
agent's key or each host, and the daemon records the turn's end, which no hook
reports. The daemon keeps the interruption with the turn state the agent
reported until a turn starts or ends again; `shoal resume`
continues the interrupted turn, as it continues stopped work, so a
client pairs one command with each.
The daemon also samples what each workspace's processes use, attributed by
the workspace identity in their environment, so the user can see which
workspace loads the machine; admission learns a settled agent's memory from
it, and protection and warnings do not use it yet. A workspace's containers
count in its usage, read from the runtime's stats by the project name Shoal
gave them, so a settled agent's memory includes the services its work
started; other processes that leave the workspace's environment, such as
work handed to a shared server, count for the machine only.
Warnings come first: below each enabled protection's stop threshold, the daemon
queues an alert for every workspace with a running agent, so agents
know of the load before anything stops. A memory or CPU warning is a heads-up
that asks for no action, so agents keep to their work. Warnings repeat per signal and workspace only
after an interval and never stop, clean up or notify. A memory or CPU warning
never wakes an idle agent, since it gives the agent nothing to do and a woken
turn costs tokens; it waits for the agent's next update or turn. A disk warning
wakes it, since deleting files frees space, but only in its own worktree and
only whole build outputs: other workspaces' files and the preset files beside
them are in use by agents it cannot see, and part of a build's output leaves
records the build tool takes as fresh for files that are gone.
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
launch waits to start in. `shoal repo move`, which starts the stopped work
again after the move, keeps them. User commands
save their arguments, which a restore reports once, as the restored agent's first
prompt or else to the user, and never reruns, because replaying an arbitrary
command is not known to be safe. Scoped callers stop only the executions of the
spaces their scope reaches but their own. A hangup
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
in flight, and its restore keeps its workspace from no cleanup meanwhile unless a
`shoal sleep` timer will wake it, which gives it work at that time; `shoal resume` asks for the agent itself, so its restore waits for
nothing but admission.
The request is its own file so that forgetting it, as a stop does, never
replaces a record a restore holds. An
agent that exits on its own before its assignment is done leaves its workspace
a last session for `shoal resume` instead of stopped work: the user may have
quit it on purpose, so it takes no heavy slot, keeps nothing from cleanup and
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

Plain Git creates and removes worktrees through the `git` module with hooks
disabled: `git worktree add` on a new or existing branch, at a commit, or without
checking files out for a copy, and `git worktree remove`, then `git branch -D` when
the removal decision deletes the branch. Before `git worktree remove`, Shoal
marks the worktree's admin directory, so that no decision reads Shoal's own
deletions as unsaved work. A removal stopped partway checks the `.git` link and
tracked files it deleted out again. If it cannot, it fails the workspace with a
`removal stopped` error, and startup or the next removal restores them once the
admin directory proves the worktree is the workspace's. A workspace readied or
failed otherwise since has lost that error: its marker is dropped and its
deletions count as its own. Typed removal outcomes preserve
unfamiliar strings; only confirmed branch deletion sets the response deletion
flag. Invoke external tools with argument arrays. Captured subprocesses share optional deadlines, whose
error names the command line and directory, and a
single diagnostic limit, terminate on cancellation, and drain output while sending
input. Keep writable state outside the binary's directory.
Service setup manages one per-user launchd/systemd service and refuses to change
its state directory until the existing service is stopped; foreground mode covers
environments without a service manager. The service captures the installing shell's
`PATH`. A managed daemon keeps the installed definition of its state directory
current, rewriting it at startup when its release renders it differently, with
the executable and `PATH` the installed one starts, and reloading systemd without restarting anything: service managers read only the
file `shoal daemon install` wrote, and a definition in a system directory would
need root and a distribution package. The unit and every scope Shoal starts set
`OOMPolicy=continue`, so an OOM kill ends only the process it chose and the
daemon's own death alone fails the service, except that a CI job's scope has
the kernel kill all of it with any one of its processes, as overload
protection describes.
Every process the daemon starts in a session of its own runs in a transient
systemd scope of its own in the service's slice, which
`workers::detached_command` adds through the launcher the managed daemon finds
at startup, so that systemd's stop of the service after the daemon exited or
failed to start leaves agents, places and CI jobs running for the next daemon.
The command a wrapper starts for work runs in a scope of its own in
`shoal-work.slice` instead, which the wrapper starts through `systemd-run
--scope` wherever it runs, so an agent in a terminal host's place keeps that
terminal yet leaves the host's control group for one Shoal controls: the
slice holds Shoal's work, and only it, for the memory limit and the kernel's
accounting. `systemd-run --scope` runs the command in its own place, so its
PID, process group, terminal and environment stay as a direct start leaves
them. The wrapper finds the launcher once, trying a scope; where none starts,
it starts commands directly.
`KillMode=process` is rejected: it would leave them in the stopped service's
control group, which the next start reports as left over, sharing its memory
accounting and OOM kills. A live daemon's SIGTERM still stops agents. Where
the daemon's startup finds that no scope can start, workers start directly; a
start that `systemd-run` fails later is not retried directly, since its exit
status cannot be told from the worker's own.
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
and automatic fallback. Repository code audits are manual read-only inspections per focus over the
entire application and open one issue per new finding. Failed or incomplete attempts
open no issues; audit results are advisory and do not gate merges.

Distribution is a Homebrew formula in the shared HN05 tap: releases install
checksummed prebuilt Linux and macOS binaries on x86_64 and arm64. The formula
has no head channel, so every Homebrew installation runs a CI-signed release.
`shoal update` replaces a downloaded binary or app bundle with the latest GitHub release and
leaves Homebrew installations to Homebrew, so one package manager owns each
installation. A shared release-target table drives cross-builds,
archive names and Homebrew platform selectors.
Named configuration templates in `crates/shoal-core/configs/` are embedded at build time, so
installation needs no source checkout or network access.
The installed global config sets nothing: it shows every setting commented
out with its default, since an installed file never changes, so a live value
would pin the default of the release that wrote it and break the file once a
later release renamed or removed that key. `shoal config set` adds a setting
above the comments, which stay as its reference.
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
A release runs no tests of its own: the version commit changes only versions, so
it accepts one once the `rust` and `musl` checks passed on the `main` commit
below it, and checks the version change with fmt and Clippy, before opening the
PR and again only when the merge changed that tree. CI runs no checks on the
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
execution wrappers and every command share the bundle's grant. The bundle and
the bare binary carry different signatures, so each has an archive of its own:
the bare binary's keeps the name earlier releases' `shoal update` reads, for
downloaded installations, which stay at one path, and Homebrew and `shoal update`
of a bundle read the bundle's, named so that earlier releases skip it. Packaged files
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
opened (`internal place`, `session`, `detached` and `demo`) therefore replace
themselves at startup with their own binary, started with responsibility
disclaimed: the agent and everything it starts use Shoal's grant while the
wrapper runs, whatever started it. Only certificate-signed builds claim, since
an ad-hoc build's grant lasts one build and test builds would prompt. A process
that replaces itself with another program, such as `internal launch` or
`chat`, never claims: that program would take the responsibility over.
A wrapper replaces itself with a copy of its bundle rather than the bundle
itself: Homebrew removes the previous version seconds after an upgrade, and
macOS then denies the accesses of the agents whose wrappers still run it. The
copy lives at `<state>/releases/<version>-<hash>/`, cloned where the
filesystem allows, with `<hash>` from the main program's SHA-256, so two
builds of one version never share one; since grants follow the bundle's
identifier and requirement, it uses the existing grant. Each wrapper holds a
shared lock on the copy's `lock` file, beside the bundle, and the cleanup sweep
removes a copy it can lock exclusively, including one of the installed release,
which the next wrapper copies again. The wrapper hands the copy the path it was
started through and that file's identity, so it still finds the installed
release and sees an upgrade. The agent's hooks run Shoal through that path,
even after an upgrade replaced the file there, so they reach the release the
daemon restarts into, also for an agent restored in place while the daemon
still ran the earlier release. A copy that fails leaves the wrapper running its
release, with a warning; a bare binary, whose grants follow its path, runs as
it is. A wrapper running a copy that continues from the
installed release after an upgrade, as an in-place restore does, replaces
itself with a copy of that release and keeps the lock on the copy it started
from, since macOS may go on judging the process by the program it started
with.

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
root is used as found; only `repo move` changes it, by default to the link,
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
once as an alert and in Shoal's `status` until a check there finishes. A check
can also run that long under heavy memory pressure with no prompt waiting, so
the report says the check timed out and, only on macOS, names a prompt as a
possible cause. The directory comes from the path alone, since reading the filesystem could stall
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
leaving staleness unknown, so `shoal attach` and `shoal list` answer from the
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
`repo move` stops the commands and agents running there as `shoal stop`
does and queues the agents' restores after the move, trusting the new paths
for them: a stop keeps sessions, which resume by ID from any directory. The
chats without a repository, which run in `root_dir`, go with the root: their
agents stop the same way, their temporary directories, attached files and kept session files move
into the directory that holds the repositories, and their records name the
configured root, which is that directory or links to it, so a root that
becomes a link is empty once they have moved. Registration is idempotent by normalized origin URL,
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
repository directory's `.tmp` under its ID, or `root_dir`'s for a chat without a
repository, which the daemon marks with its state directory so another daemon
sharing `root_dir` leaves it alone. The ID never changes or comes back
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
left for the unattended grace period: no running `contribute`, whose daemon
connection holds it in memory from its registration until the command ends,
resuming on a new one after a restart, while every contribution counts as held
for the first minute the daemon runs, no
workspace, queued agent, chat, pending
issue proposal, kept branch or borrowed resource, nothing `shoal resume` could
use, no files a removed space keeps, and no work in its checkout that the remotes' advertised branches and tags
lack. The final
check records the removal in the same transaction, under the gates creation
takes, and queue and chat records refuse a repository being removed. Removal
never deletes the fork: Shoal does not record whether it created it, deleting a
forge repository cannot be undone, and the fork may hold the only copy of a
PR's branch. An existing registration is never turned into one.

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
Decision: Shoal reads a repository's location, HEAD, refs, remotes and config
in process with `gix` rather than starting a Git process for each fact, since
those processes dominated the daemon's background load. Each read opens the
repository afresh, so a moved or replaced worktree is caught as before, sees
the user's and system config as Git does and none of the repository variables
Shoal's Git commands drop, and runs on the blocking pool. A repository `gix`
would not read as Git does, such as one whose refs are in a reftable, a partial
clone, whose missing objects Git fetches, one that enables an extension
`gix` lacks, or one whose config includes a file on a condition `gix` evaluates
otherwise, such as one on the branch, and a process whose environment points Git
at another repository read through Git. Writes, network access, `status`,
`ls-files` and `diff`, whose ignore and fsmonitor behaviour must match Git
exactly, revision syntax Git interprets, such as a caller's `--base HEAD~2`,
beyond HEAD, a full ref name or a full object ID peeled at most once, which mean
the same to both, and the ancestry, `rev-list` and
`cherry` checks behind removal and cleanup decisions stay with Git, where a
difference could cost work. Branch names are checked in Rust by Git's rules.

New-branch creation accepts literal Git branch names. A space's name is a
free-text label that may repeat, so it says what the work is rather than
identifying anything: a new space takes `--name`, else what it was started for,
a CI job's name or the linked issue's or PR's title, else for a workspace the
branch as given, so the label a list or tab shows is the most telling one known
at creation; only `shoal rename` changes it, so a label never shifts under the
user. The workspace's portable directory name derives from the branch, suffixed while another workspace of any repository has that
directory name, so a normalization collision touches no existing work. Branch
conflicts of a name Shoal derives or splits get numeric suffixes on the blocking
component only, never changing the workspace's name. `start --branch` takes the
name as typed, as `git switch` does: an existing branch opens, in the workspace
that has it when one does, through `--in`'s path, and a new one is created under
exactly that name or refused, since a suffixed branch is not the one the user
asked for; one flag both opens and creates, so the user never chooses between
spellings for a branch that may or may not exist. Names Git reads as a revision
instead of a branch, in either supported object format, get a leaf suffix on
creation and an error on existing-branch selection. Ownership of a worktree's
Git admin directory is proven by a marker naming the workspace, written there on
creation, adoption and verified repair: Git keeps it across moves and drops it
when the worktree is re-created, so device renumbering, restores and copies keep
ownership while replacement does not. A byte-for-byte copy of the admin directory
is therefore accepted as the same worktree. The marker is the only proof of a
worktree whose creation finished: an unmarked admin directory requires explicit
reclaim. Moved or replaced
directories are never adopted silently.
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
before cleanup can remove it, retargets stacked PRs still targeting the head
branch of one of its merged PRs,
and queues a watch update; agents rebase their own branches, since Shoal moves
a workspace branch only by the fast-forward `shoal refresh` makes when asked,
or the start of an agent the queue admits there makes. Retargeting goes through the same REST transport as
issue and PR actions. Adoption
takes a path; the CLI infers its repository from the owning checkout, then a unique
`origin` remote match, then a picker, with `--repo` as the override. Explicit
adoption accepts an unlocked linked worktree root on a local branch of the registered repository with no ownership
conflict, preserving dirty files and Git settings and recording it ready without
setup or hooks. It takes normal cleanup ownership. Reopening verifies its record;
adoption cannot repair a moved managed worktree, and repairs one whose metadata was
replaced only by reclaiming it at its recorded path. Never adopt a main checkout.
`adopt --copy` records a preparing workspace before creating a fresh worktree
under the repository's Shoal directory, copies the source worktree's files while
leaving the source in place, removes checkout files the source deleted at any
depth, and adopts the copy on an available branch derived from the source branch; a failure keeps its plan, whose retry copies the source again and fails naming it once it is gone.

A workspace may sit at a commit instead of on a branch: a worktree at a detached
HEAD with no branch of its own, created from a commit, tag, branch or PR head, a
fork's included, so comparing with another commit, researching older code or
reviewing a fork's PR never disturbs another worktree. It takes an agent of its
own as a branch workspace does, through the same launch, queue, ready, completion
and cleanup paths; that agent's opening says the workspace has no branch, so it
cannot push or open a PR from there. Without an agent, whoever created it runs
things in it: a CI job, a reviewer, the parent's agent or the user's shell, with
`shoal run` and `shoal demo`. Everything else goes through the workspace paths: the seed clone, executions, leases, scope tokens, children, holds,
checkpoints and removal. `create --no-branch` creates one, below the caller's
space or as the user's top-level one, and a fork PR's head opens in one, placed
the same way, that is reused per PR and moved to its new head while it has nothing of its own. Its
starting commit counts as kept on removal, since a fork's head or a local commit is
on no remote, while commits made on top count as unsaved work; restore reopens it
without a branch, and branch-only commands refuse it. `--skip-setup` and
`--disposable` are general creation options of `start` and `create`, not special cases
for a runner: one skips what gates readiness, with the removal hooks that tear it
down, and the other makes every removal delete the workspace without inspecting it,
asking or keeping a checkpoint.

A workspace can start from a copy of another's current files or from one of its
checkpoints, with `--copy` and `--checkpoint` on `start` and `create`: two flags,
since spaces are also named by number, and neither combines with `--base`, which
keeps meaning a Git ref and so committed work only. A copy is an ordinary
workspace on a new branch, or at a commit with `create --no-branch`, with one
editing agent of its own, a fresh one or with `--fork` one forked from the
source's. Copies are built on checkpoints. A copy of current files gets a linked
worktree created without checkout, the source's index and cloned files, ignored
ones included, which replace the seed and `setup.copy_ignored`; a plain folder
copy would share the source's `.git` file and index. It then takes a checkpoint
of itself, private at that moment, so the record matches its files even when the
source's agent kept editing during the clone. A copy of a checkpoint writes its
files into a new worktree as a restore does. Either way the copy's starting
checkpoint keeps every unsaved file, and the copy's
record keeps it from expiring, so commits up to its HEAD count as kept and files
that match it count as unchanged: what a copy was given is never its own work,
and an untouched copy is cleaned up like an empty workspace. That decides only
whether cleanup may remove it; its branch goes only once a remote or the default
branch has its commits, as any branch does. It takes its
source's diff base and worktree Git config, and nothing that belongs to the
source's work: links, leases, holds, threads, ready marks, checkpoints or the
session. Agents copy only within their scope, and a removed workspace's
checkpoint, like reopening it, stays the user's.

`shoal refresh` brings a workspace up to date with what its record says it was
made from: its upstream or the branch it builds on, a PR's head or a branch's
tip for a workspace at a commit, or a copy's live source, whose files it copies
in again in place after a checkpoint keeps the copy's. It moves a branch
workspace's branch only by fast-forward, while a copy's branch follows its
source's HEAD even after a rebase, since the copy holds no commit of its own
and the checkpoint keeps its old HEAD. It refuses work of the workspace's own, naming it and the command
that deals with it, rather than rebasing, merging or carrying it over: resolving
those is the agent's or the user's work, and a refresh must never lose any.
Ignored files are nobody's work, as cleanup counts them, so a copy's are
replaced by its source's, whose setup outputs they mostly are. It
runs only when asked, since a checkout moving under a working agent would
surprise it, and like restore it refuses while another command or agent runs
there.

A space changes kind with `shoal convert`, since only its agent learns what the
work is, and a merged workspace's conversation can still be worth keeping. The
space stays the same: one transaction changes the kind its `spaces` row
records, with its ID, number and name, and swaps the row of its kind's table,
so what belongs to the space by ID, its links, tree, threads, files and
progress, stays. A chat
becomes a workspace through the creation path every workspace on a branch
takes, with the chat's record turned into the workspace's at insertion and
given back if the worktree cannot be made; the worktree directory comes from
the branch. A workspace becomes a chat through the shared removal path, which
keeps a checkpoint and the branch and the spaces below, and commits the chat
in place of the workspace's record. A conversion moves its agent only once its
turn ends, so the command that asked for it finishes: the daemon records the
pending conversion, which survives a restart, and one task watching agent
states stops the agent and starts it again in the new place from a fork of
its session, as `start --fork` and a copy's restore do, since a session
resumed in another directory would not find its files there. A start without a
branch begins as a chat in its mode, whose agent converts it once it knows the
work, instead of a workspace on a placeholder branch whose directory would keep
a meaningless name.

`shoal create` makes a workspace as `shoal start` makes one, from the same
arguments that choose what it starts from and where it goes, and starts no
agent there: `create` is the way to a workspace without one, at the top from
the user's terminal or as a child
inside a space, so an agent whose work needs another PR gets another branch
and worktree instead of switching branches or creating worktrees Shoal does
not own. The daemon places it as a launch's new workspace and records it
through the steps every new space's start takes, with nothing to queue. The
parent is a workspace or a chat of a repository, as for any child. A child goes
in its parent's repository on a new branch from the default branch or a `--base` that
stacks it as usual, records the parent, and links an `--issue`. Any space opens
a child of either kind the same way: `shoal start` run inside a space puts the
new workspace or chat below it, or below a space its scope reaches that
`--parent` names, in its repository, and a scoped caller creates nothing else,
while the user's terminal creates top-level spaces.
The daemon records every new chat, below where it goes and linked to what it
starts on, before its agent runs anywhere, in one operation whatever runs the
agent: the caller's terminal, a background place, the queue or an app only
runs the recorded chat's agent, so a start that names nothing to link sends no
link request. The queue starts every admitted agent the same way, opening a
place for any recorded space, and every agent start has an entry, admitted at
once where no limit applies. Every start sends one launch, a chat's that
runs here or beside its terminal included: the daemon records the space with
its agent's entry, which a caller that runs the agent holds, and the CLI
runs the recorded space's agent through one placement step for both kinds,
here, beside, in a background place or detached, a workspace's once its
preparation ends. One place worker runs any recorded space's agent in a
place, in the space the daemon opened for it, so no other process reopens a
workspace or links what its start names, and one builder makes its command from the space's records, so only
what the record holds differs by kind, such as a chat's home and the
instructions its agent receives first. A start that fails fails its queue
entry. What linking a new space, of
either kind, warns of goes to the agent of the nearest space above it with
one, else to the user's alerts, never only to a terminal the agent then takes over. A space
whose place never starts its agent is kept, whatever its kind, with its
launch as a failed queue entry, which nothing admits and which keeps no
space from cleanup; its failure goes to the starter the same way, naming
`shoal resume <space>`, which queues the launch again as it was, and
`shoal rm <space>`. A new launch in the space replaces it, and the space's
removal drops it. A start the user makes by hand from their own terminal, of
a new space or an existing workspace's agent, and a restore they run in their
terminal record their interaction with the space's agent; a trigger's or a
scoped caller's do not.
A chat an agent starts opens in a background place, which the agent queue
opens once it admits the chat's agent, since the agent cannot hand over its
terminal, unless `start --here` runs it in the caller's own; the place runs
the chat's agent in the chat's own scope, and the start returns once the
daemon has recorded the chat and queued its agent. A chat run in the caller's terminal is the
chat's own execution, as a workspace's agent is, which the caller's stops
leave running; places drop the marks and scope token of the space
that opened them, so what runs there is the new chat's. Spaces form one tree through one parent link,
`space_parents`, which records a workspace created below another and one a chat
opened alike: every space has at most one parent, a ready workspace or a chat
of its own repository, or a chat without a repository, never one below it, and
any other child in another repository is refused: spaces across repositories
share a parent only in a chat without one. Every space has one row in
`spaces`, with its kind and what every space has, which its workspace or chat
row extends with what only its kind has; a conversion keeps it, and deleting it
removes the space with what each table of spaces holds for it. A removal reserves the space first, in memory whatever its kind, and a
workspace's worktree also through its `removing` state, which startup
recovery reads. A hold keeps a space of either kind and stays through its
conversion; a reserved space takes no new hold, so a removal that leaves the
user's holds in place checks them once, when it reserves the space. Every
reader takes one walk up or one walk down that link, with no branch on kind,
and asks each space one question, whether it has an agent: a workspace's
agent, a chat's running agent, or a queued restore of either. A scope reaches
its space and every space below it, and a chat without a repository's every
space, which one operation names for every check, the CLI resolves a scoped
command, a workspace's or a chat's, to the workspace below whose directory it
runs in, else to its own space, and agent hooks always report for the agent's
own space. So a chat's agent that changes into a workspace it opened acts on
that workspace, as a workspace's agent acts on its child. The parent's agent, a workspace's or a chat's, receives the
issue and PR activity of the spaces below it without an agent of their own,
and theirs in turn, each update naming its space, through the same cursors,
deliveries and acknowledgements as its own; a child's own agent, which reports its turn state or which Shoal tracks
there, as it tracks custom and Happy agents that report none, takes over its
subtree, and so does one whose restore is queued, since it receives what
arrived once it starts. A child keeps its parent only from merged cleanup,
which would otherwise take a child's open work because the parent's own
branch merged, and never outlives it:
the parent is cleaned up by its own rules, and every removal of it, manual or
unattended, first removes the spaces below it, deepest first, through the
shared removal path, with a checkpoint and kept branch for each workspace but a
disposable one, whatever their work, holds or threads; one that cannot go
retains the parent, and the journal and
the removal's output name those taken. While a parent's completion with cleanup
stands at its HEAD, its children of either kind go too after the grace period,
whatever their own work, unless one of their own keep-reasons, or a space still
below them, keeps them; a child's own `done` follows the usual rules. The parent owns its
children, so its processes may remove and hold them, except that the user's
holds anywhere below refuse the removal. `list` shows children below their
parent, and `status` names the parent and children of either kind. `shoal start`
is the only command that starts an agent, so `create` creates worktrees alone: a
`start` run in a space, a workspace or a chat, gives a child an agent of its own
through the agent queue, and `start --fork` gives it the parent's agent
from a fork of its session, the one the caller's agent or else the parent's only
agent reported last, so the parent need not restate what it learned; the daemon starts every launch in an existing
workspace itself, detached, unless `start --here` has the caller start it, and the agent
holds its own space's scope, never the caller's, and
scoped callers may queue only for the workspaces their scope reaches but their
own, and cancel and prioritize the entries their scope reaches: those of such
spaces, a CI job's through the spaces its run is for, and a new space's launch
through its parent. A scoped caller's
start that a blocker holds records its workspace below the caller at once, as
any start does, and its waiting preparation records the caller's space, in
whose scope it runs once the work is done. A space's
coordinator is the agent of the nearest space above it that has one, whatever
its kind, which one operation queues a message for: the events about
it that notify the user, the child agent's messages, threads and completion
and its agent's exits and stops, plus its ready marks, become that agent's
messages instead, falling back to the user without a coordinator or when its
messages are full. Events a process causes in a child without an agent of its
own are the coordinator's own work and reach the user. The coordinator answers
the threads opened below it or forwards them up, and may
stop, restart, resume and convert the spaces below it and forget their stopped
work, since only it knows whether a child agent still has work. It resumes a
stopped agent whoever stopped it, the user, Shoal or a crash: agents through
the queue, so the daemon starts them detached, and chats in a background
place, as it starts them. It confirms that an execution Shoal cannot prove
ended did as the user does, while the check of a survivor's birth identity
still keeps unknown ownership from authorizing a kill. Every such check asks
whether the caller's scope reaches the target, and a refusal names the space
outside it.
Stopping a space stops the spaces below it, workspaces and chats, unless
`--skip-children` says otherwise, since their agents work for it. Approval
requests go up the same tree: to the nearest space above with an agent, of
either kind, since each knows best what its children need. A parent space,
workspace or chat, approves what a permission governs where its own
permission for it is `direct`, anything else only where it holds an approved
grant for it, and a hold on its child, and otherwise denies or passes the
request up, unless its `decide` permission lets it decide any request below
as the user would, a request a permission governs still only where it holds
that permission at `direct`; the user
decides at the top, and a chat whose agent stops or a
deciding space that is removed hands the user what it had not decided, since
neither can decide any more. A space lists the requests it decides beside its
own, whatever its kind, since it must find them to decide them.
When a subagent works in the child's directory instead of an `--agent`, the
child has no agent of its own, so its activity reaches the parent, and the
subagent reports to the parent directly, supervision Shoal need not relay. Shoal does not track those subagents: their hooks report under
the coordinator's scope from its directory, so they cannot tell which child a
subagent works in. A child in which no agent of its own has ever started,
which the workspace's recorded mode tells, is instead its coordinator's work:
it lists as `parent working` while that agent works, and idle cleanup keeps it
while that agent runs, in any turn state, since subagents may outlast a turn
and a coordinator may wait for the user mid-delegation. A chat below it whose
agent stopped is its work the same way, so idle cleanup keeps that chat too.
Claude Code's worktree
hooks make the worktree it creates for a subagent such a child, so its own
isolation needs no worktree Shoal does not own.
A subagent inherits its agent's scope, and under Codex its instructions and
directory, so a `SubagentStart` hook names the workspace it works in; without
it, a Codex subagent finishes its agent's assignment as the instructions say.

`shoal branch rename` changes a ready workspace's checked-out branch, keeping its
name, in one durable operation; its path, identity, resources, and associations stay
with the workspace. The default branch, a branch checked out elsewhere, or a
name collision is refused. No execution other than the scoped caller may be
recorded, and interrupted intent blocks reuse until explicit repair or verified
deleted-worktree cleanup.

Named Git profiles live in global config; repository or global `git_profile`
selects one for newly created worktrees, overridden by `start --git-profile`.
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
records, and `start` keeps the workspace preparing until setup exits cleanly with no
survivors; manual removal also takes a preparing workspace with no execution, whose
setup an interrupted command never started. Failures preserve files, branches, and leases; process-verification
failures retain the workspace and report the command's exit status and the
blocking evidence. Other failures let interactive callers choose retry, delete,
ignore (which repairs verified state first), or keep; a setup that never
started offers only retry or keep. JSON callers get a nonzero exit.
`setup` reruns setup explicitly, and `resume` refuses a workspace whose setup
failed, naming it, since a setup is not an agent; nothing retries
automatically.
The agent a start launches in a workspace whose setup fails waits in the
queue for that setup instead of holding its slot, queued with the workspace's
record for a start the daemon carries out, or by the preparation, which
carries the agent a CLI would run itself, so no follower is needed; one that
starts now starts once the setup releases it, regardless of the limits. A start whose preparation ended without queuing it, such
as when its worker died, goes back to the queue as a new entry, which no hold
on the old one keeps back, with its plan opening the workspace. A persisted
dependency that only a preparation getting past its post-setup hook releases
holds it, so a retried setup starts it without the user restating its mode,
task or permissions, and a failed hook keeps it waiting.
While the daemon registers an execution it sends the wrapper keepalives, and
the wrapper's limit bounds the daemon's silence rather than the whole wait, so
setups queued behind each other's Git gate never fail for being slow; a
registration whose wrapper left is forgotten rather than recorded as a failed
setup.
Tracked setup starts by copying the repository checkout's ignored files unless
`setup.copy_ignored = false`, so local files like `.env` arrive without a setup
script and failures gate readiness; `.worktreeinclude`, a convention other worktree
tools share, selects what to copy, and `setup.copy_ignored_exclude` leaves entries
out. The copy is Shoal's own, ported from Worktrunk's `wt step copy-ignored`.

Every process Shoal starts, an execution, a hook, an export or a CI job's
step, names its space in `SHOAL_SPACE` whatever its kind, and only a space with
a worktree gets the worktree's variables, so a chat in a workspace's worktree
carries none of the workspace's. Hooks are deliberately untracked user processes with the space identity
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
tracked executions, those that started them, and terminal
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
alert; `shoal status` lists them for the user.
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
A manual removal answers its request once the workspace is reserved and passes
the checks removal makes before changing anything, and goes on in the
background, under the gate a daemon update waits for, unless the caller waits
for it; a later failure retains the workspace and reaches the requesting agent,
or else the user as an alert. Chat removal stays synchronous.
Idle cleanup requires unchanged
HEAD, activity and file metadata; while the timer runs, sweeps compare HEAD and
activity and check recorded state and directory users, and inspect Git state and
walk the tree only when the timer starts, when it expires and after removal hooks,
since change times only move forward and cannot be set. For the same reason,
merged cleanup walks a worktree again only once the newest change its last walk
saw is older than the idle period. A sweep lists directory
users with one scan of the processes' working directories, read from the kernel,
shared by every workspace it checks; removal scans again.
Status reports the first reason cleanup keeps a space from the checks the sweep
runs, so it cannot disagree with the sweep. A HEAD changed by removal
hooks keeps its branch when only that branch has the new commits.
Post-remove runs after ownership is released,
from the repository checkout with its copy of the executable, retaining the old
workspace path in the environment. Its command is selected before removal.
It is best-effort: failure is a warning and alert, never a failed removal
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
a workspace from it stays plain Git, an explicit `shoal refresh`, or the
fast-forward an admitted agent's start makes. Commands that only read the default branch
never sync, so none waits on a fetch or handles a failed sync; workspace
status shows a failing sync, and one failing for 30 minutes of awake time, which
a brief outage or a wake before the network does not reach, needs the user and
joins the inbox with one alert. Git already merges and rebases a workspace onto the
default branch, so Shoal has no command for that direction. `merge` is one
command for a space's work, whichever way it merges: the daemon decides from the
space's links, merging the one open PR it links as assigned or opened on the
forge, and otherwise a workspace's branch locally. It reads each linked PR's
state from the forge, since a poll follows no opened PR, and refuses several
rather than guessing one. There is no flag to merge locally past an open PR,
which would bypass its review: closing the PR is that decision. Because it
merges a space's work either way, it is top-level rather than under `pr`, and
reads a PR it is given as the cross-kind verbs read items. `[merge] method`
sets the method of both kinds of merge, so a repository merges the same way
with or without PRs. A PR merge without a method takes the repository's
default, which each forge's merging implementation reads or leaves to the
forge; GitHub has none, so it takes the first method the repository allows. A
squash merge's message goes to the forge only where the method is known to
squash. The local merge, the guarded
substitute for a pull request, merges the workspace branch into the default
branch by any of a PR merge's methods: a fast-forward or merge commit, one
squash commit by whoever merges, made with `merge-tree` and `commit-tree`
without touching a checkout, or the branch's commits rebased in a worktree of
its own, keeping their authors; the default branch then fast-forwards to the
commit either makes, and the workspace's branch stays as it was, as a forge's
squash or rebase merge leaves a PR's. Without a message, a squash of one commit
keeps its message and of several takes Git's own. It first refreshes a configured upstream while preserving an ahead branch and
refusing divergence or fetch failure. It pushes the default branch to that upstream
only on request, checking the upstream before merging and keeping the merge when the
push fails. The default branch cannot be
held by a managed workspace, and any checkout of it must be clean. It aborts a merge
that does not apply cleanly, leaving conflicts to a Git merge of the default branch into
the workspace, or for a rebase merge, whose replay a merge does not resolve, a
rebase of the workspace onto it. Merging holds the repository Git gate for the tracked execution; the
daemon validates, resolves the push target and refreshes, and the CLI worker merges.
Pushing publishes the merge where a local reset cannot reach it, so a merge
that pushes asks the user first, after the daemon's checks and before it
changes anything; it names the direction, since `git merge main` in a workspace
merges the other way. A
local merge asks only when the last check found sensitive files changed on both
sides, which the same question names. An agent's approval request carries the
same details, the method and a squash's message among them, so an approved
merge commit lets no squash through, and is its confirmation; the agent still answers the question about
sensitive files with `--yes`, so the approver decides on them.
A repository that merges only through PRs turns local merges off with `[merge]
enabled = false`, which refuses agents' requests for them too.
Because a local merge can be undone, `unmerge` is a command of its own rather than
a flag that flips `merge`. The daemon records each merge with its method when its
execution ends, from what the default branch holds rather than what the worker
reports: a squash as the one commit whose tree is the merge of the two, a rebase
as the chain that replays the branch's commits; and drops
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
restored with `shoal restore` first; records follow the branch, not the
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
a snapshot, so a running build never reaches a workspace. A clone of a large
build tree takes minutes, so creation clones after its Git operations, outside
the repository Git gate, and the workspace is neither ready nor set up until the
clone ends. The daemon also keeps one spare clone of the snapshot ready:
creation moves it into the new worktree, a background task clones the next, and
creation clones only when no spare is ready. Clones read the snapshot under a
shared lock of the seed's own, which a swap or deletion of the snapshot takes
exclusively, so starts and the refill clone at the same time and only taking the
spare is exclusive. A new snapshot is cloned and the old one deleted outside the
Git gate, which it takes only to swap them. Creation restores the
seed's modification times on tracked files its commit left unchanged, and on
the directories holding them where it changed no path inside, since
mtime-based tools otherwise treat every freshly checked-out file as new, and
Cargo's `rerun-if-changed` on a directory reads the directory's own time.
Clones of a file still cost time per file, so on btrfs each output is a
subvolume, whose snapshot takes the same time for any number of files: a freeze
turns a plain output directory into one, once a probe shows Shoal can remove the
subvolumes it makes, and removal destroys them before deleting the rest. Other
filesystems use reflinks or APFS clones; ZFS datasets and APFS snapshots cover
more than one directory, so neither serves. Platforms without clones skip the
seed rather than copy. Seeds are best effort:
failures notify and never fail creation, and builds and spare clones wait under
host pressure so they never add load an agent would feel.
A repository's `presets` name the build tools it uses, and each sets up what
that tool needs to stop repeating work across workspaces, so common stacks need
no script. Tools whose build outputs survive a move into another path get a
seed: Cargo, whose seed covers it better than a shared compiler cache such as
sccache, which needs another tool and gives up incremental builds, and the Node
package managers. Cargo's seed also runs `cargo test`, which builds a proc-macro
member with other settings than `cargo build --all-targets` does, `cargo
check`, which makes the dependency builds `cargo clippy` uses too, and `cargo
clippy`: Cargo keeps clippy's builds of the workspace crates apart from
`cargo check`'s, so without its own step every workspace type-checks them from
scratch for clippy. Clippy's lint arguments are rustc options, and rustc
discards incremental state built under other options, so the seed passes none
and serves clippy runs that fail on warnings through Cargo's `build.warnings`
instead; a step without them also denies nothing, so a default branch with
warnings still seeds. A failure of the clippy step alone still freezes the
snapshot and notifies, since its outputs serve only clippy runs and clippy is an
optional component a toolchain can lack; a timeout or cancellation there fails
the build as in any step.
Cargo never deletes the outputs of a unit whose hash changed, which
releases, lockfile and toolchain updates and crate moves do, so the seed
`target` would keep every earlier build and each freeze, spare and clone would
copy them. The Cargo seed therefore runs `cargo clean --workspace` first, which
keeps third-party units, when its toolchain, settings or a file those hashes
read has changed since the snapshot's build, and only then: most refreshes
rebuild a few crates incrementally, while one that changes unit hashes rebuilds
most of the workspace anyway.
A seed serves the workspace crates only if their incremental state survives
the move, but rustc reuses it only under the same compiler options, which
include the working directory, and reuses generated code only for the same
output directory. So the Cargo preset runs the workspace's own crates through
a wrapper that adds `-Zremap-cwd-prefix=.` and passes the output directory
relative to the workspace root. Cargo's `trim-paths` does not help: its flags
name each package's directory. Stable rustc takes the unstable flag only for
the crates `RUSTC_BOOTSTRAP` names, so for a rustc that refuses it the wrapper
names each crate it builds and allows it no unstable features; a nightly rustc
gets only the flag, so crates keep the features they enable. Dependencies,
which Cargo builds without the wrapper, and Cargo's target queries, whose
printed cfgs a grant would change, stay as the toolchain builds them, and the
wrapper drops the flag for a rustc that no longer takes it. `cargo clippy`
replaces the workspace wrapper with `clippy-driver`, so the preset also sets
`RUSTC_WRAPPER`, which Cargo runs before `clippy-driver` for the workspace
crates and before rustc for everything else, to a second script that hands
only `clippy-driver` runs to the workspace wrapper. That replaces a user's own
`RUSTC_WRAPPER` or `build.rustc-wrapper`, such as sccache, whose work the seed
already covers; one set in `[env]` wins, as every
variable there does, and clippy then runs without the mapping. CI runners set `CI`, under which
Cargo leaves incremental compilation off and so misses the seed's builds of
the workspace crates, so the preset gives Shoal runner jobs
`CARGO_INCREMENTAL=1`, the only setting that overrides `CI`, and not agent
workspaces, where it would make release builds incremental too. It reaches
the steps through the runner's own environment, which a workflow's `env`
replaces on both forges, while Forgejo's `runner.envs` would replace that
`env` and GitHub's `GITHUB_ENV` would override it. Preset seeds run the default branch's own build code, as
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

A preset also brings the MCP servers its tool's agents use, as `xcode` brings
Xcode's bridge, and `[mcp.NAME]` adds, changes or turns off servers by name in
every layer, as `[env]` does a preset's variables. Shoal gives each workspace
and chat agent its repository's servers in the agent's own syntax, beside the
servers the user's config names, so a server only some repositories need
belongs to their config rather than to the user's, which every session starts.
Shoal never edits the agent's own config to do it, and an agent whose
integration has no such syntax starts none.

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
another commit. Polling finds jobs, which works without the webhook listener
and the tunnel it needs; it runs every ten seconds while a job could start and every minute
while none can, so the queue still shows new jobs and drops those other
runners took, and conditional reads keep unchanged answers off the rate
limit. Each job runs as a tracked execution in
a disposable workspace at its commit, which goes once the forge's runner
exits, at low CPU priority so that a build takes only the cores agents and the
user leave idle. A job outlives the daemon, which only decides when it starts:
a crash, a failed start, a restart and a signal, which the daemon cannot tell
from a service stop, leave it running for the next daemon to take its wrapper
back, and its wrapper runs apart from the daemon's service, as every detached
worker does, so the service manager's stop of the service leaves it running
too. A job ends only on the forge's word, so stopping it, as `shoal daemon
stop` and a system shutdown do, cancels its run there, as does its wrapper
through the daemon, which keeps the forge's login, when a logout's or a
shutdown's signal reaches it, and disk protection leaves it running. Load
protection alone ends a job without
cancelling its run, which would end the run's other jobs too: it stops the
job's processes and its runner, the forge fails the job once the runner
reports the stopped step or gives up on the runner, and the job reruns once
the run has finished, so the PR's check shows the job failed until then. Once the forge has finished the run,
a runner that outlives it by a grace period, held open by a process a step
left, is Shoal's to end with every process of the job's workspace. Every space that links a run's PR, and the workspace that owns its branch, lists the job in its status, and a
waiting one with what holds it back, but
none of them owns it, lends it a build nor reaches it with its scope. The runner is
registered per repository with the user's own login, never for a user,
organisation or instance, or on GitHub one runner per job, and what the forge
does its own way is a `Runners` operation, so another forge adds an
implementation, not daemon code. Container jobs and forges other than Forgejo
and GitHub remain proposals.

## Scope and user interfaces

Tracked executions and processes started with exported workspace environments
inherit a daemon-validated scope token that
confines them to their own workspace and the spaces below it:
status, inspect, execute, setup, and resources.
Creation other than of a child of the caller's space, removal other than of a descendant the user holds nothing below,
reconciliation, other workspaces, repository administration, and service control
need an unscoped caller; `config reload`, which applies the config on disk and
stops nothing, does not. A scoped `merge` is an
approval request bound to the branch and its HEAD unless the caller's `merge`
and `push` permissions are `"direct"`, and a scoped `unmerge` one bound to the merge it undoes;
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
environment is readable. `internal env` exports a space's identity, and for a
workspace its ready, verified worktree's variables and current port variables,
with a fresh token, for any space the caller's scope reaches.
Exported tokens persist across restarts until explicitly revoked or the space
is removed; they register no execution and do not prevent cleanup.

Root help groups built-in commands by task; space maintenance commands such as
`status`, `log` and `rename` are top-level, repository administration under `repo`, moving workspaces to another
machine and repositories to another root under `move`, forge inspection and API access under
`forge`, CI runs and the checks on a PR's head under `ci`, and
shell integration under `shell`. `run` lists configured commands, runs one by
name, or executes an arbitrary command after `--`; `--workspace` selects the
workspace in either form. `done --undo` withdraws completion and `ready --undo`
withdraws review marks. Threads use `thread open`, `forward`, `resolve` and `list`, and
posts in them top-level `send` led by the thread's number; alerts use `send` alone, and `alert list`, `withdraw` and `clear`. Alert rows, in `alert list` and the picker, show
the time for today's and otherwise how long ago they came in whole days, weeks,
months or years, such as `1 day ago` or `2 weeks ago`.
Approval snapshots carry their stable request key through the CLI so reused
numbers cannot decide a replacement. Implicit thread replies and approval decisions
require stdin, stdout and stderr on a terminal, so redirected details cannot
hide the decision being made.

`terminal.herdr.enabled = false` turns off every Herdr call, so a Herdr pane behaves as a
plain terminal; `new_tab` only governs opening new tabs.
The CLI owns Herdr tab handoffs after issue lookup and interactive workspace and
agent choices and labels the tab with the space's name by default, which a
template's `{name}` also gives. The user may rename the space or ask the
agent to choose a name, a fixed alert refused without a running agent,
which would leave it unread until removal. A rename relabels the places the
space's agents run in and those Shoal created for it, wherever the command runs: each through the host
that opened it, on the server its record names, as closing reaches it. An optional `terminal.herdr.tab_name` template
follows normal configuration precedence, substitutes repository, branch and issue
fields once, and updates the branch after allocation. The resolved template travels
with the launch plan so the worker preserves the caller's naming choice. Shoal
renders the label in its generic terminal layer; a host only applies the label it
is given, so naming rules and their daemon and forge lookups live in one place.
The plan travels in the tab's environment, naming the starting launch, whose
issue the place reads from the queue, rather than carrying its unbounded body,
and preserving
state/config selection and literal arguments without hook scripts or state files.
By default, agents that start with an issue prompt or forwarded arguments stay in
the background, and tabs that wait for the user's input receive focus; an
explicit focus setting, the mode's over the terminal's, or `--focus`
overrides this choice, so each mode can decide whether its spaces take the
terminal, and the built-in chat modes, whose agents the user talks with,
are set to; `repo
setup` and `config setup` always focus the agent the user works with straight
away. A launch the user waits for starts past `max_heavy_agents`, as `--now` does:
one `--focus` focuses, or an interactive call's agent waiting for input that
the setting does not send to the background, unless `--add-blocker` makes it wait for work; the setting alone does not, so it
cannot switch the limit off. Commands say only what they
launch; one terminal layer chooses the placement and the backend that carries
it out, so focus behaves alike everywhere and another backend plugs in with
its own primitives without touching commands: a host that opens places, such
as Herdr, focuses one or opens it in the background, and one that keeps agents
in sessions the terminal attaches to, such as zmx, attaches the caller or starts
a detached session running the plan, which it receives in its environment as
every place does. A restore takes no new input, so it restores agents where they stopped
or in the background; only `--focus` restores one in the caller's terminal.
The worker drops the plan before starting children; a retained tab's shell keeps
it, inert, while a non-default state directory is passed only to the worker.
A place that keeps a shell, such as a Herdr tab, remains open after tracked
agent exits, and its shell enters the workspace through its own exported
navigation file, followed once at a prompt after the worker writes it.
Completion that keeps the workspace closes a place Shoal opened for it, for an
addition, an attached agent or a background resume, only after tracked executions
have resolved or the agent's turn finished, since interactive agents stay running
at their prompt, and never while stopped work waits for resume, so the place keeps
the stop's cause and `shoal resume --all` can restore the agent there; removal
closes it immediately, so completion cleanup leaves the agent its grace period.
A restored agent that starts in another place than the saved agents it
restores ran in, as a restore whose place is not back at its prompt does,
closes the places Shoal created that they left once it runs, unless an
execution is recorded there or another saved agent waits there: such a place
serves nothing, and `attach` offers only the sessions that show the agent.
The wrapper names the records it restores from, since a workspace may save
several agents.
The turn comes from the agent's hooks, and from the host only for an agent
without them, such as Herdr's `idle` or `done`; zmx reads none, so its sessions
of such agents close with their execution. The rule, the watcher and its marker
are generic: a host only closes a place and reports whether it is open, what
its agent is doing and whether closing it ends another place, as closing a Herdr
pane's tab ends its other panes, where a batch removal or a streaming export
then refuses to start; generic code tells the Shoal it starts in a new place
whether to watch it, while a restore in the pane its agent stopped in leaves that
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
space's sessions when it removes the space, a workspace or a chat, whatever
`terminal.close_when_done` says, and startup and cleanup sweeps close those whose space record is gone,
retrying failures without restoring ownership. Sessions from before the label
have no known origin and are left alone. A session zmx lists as unreachable,
such as one a zmx of another version started, shows neither labels nor
instance, so every lookup of it, and every sweep while one exists, fails
naming it instead of taking it for another owner's or for gone. Relabelling replaces obsolete ownership
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
serves runs work in place, and a queued start or a demo without another
fallback fails. Rex cannot set a new terminal's
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
reads from its data, and ID. A record names its owner, a space of either
kind, and every removal of a space closes its places through one operation,
so no kind of space keeps a place it cannot close. An agent of
another workspace or a chat that starts in a recorded place takes it over, as
relabelling does for a zmx session, while a nested command takes nothing; a
place Shoal opens for a chat is the chat's from its start, which the chat
learns from the place's environment; place watchers close recorded places
through the daemon, which reads the owner and closes under one lock, or forgets
the record and leaves the close to the watcher for a host it cannot close
places of. A close ends only that block or pane, leaving the user's other
blocks in its window and panes in its tab; Herdr closes a tab with its last
pane. Rex's own
`own_block` arbitrates terminal size between clients and is not ownership.

Shoal reports each space's status to the terminal its agent runs in through
the Program Status Protocol (OSC 7501), so any terminal that reads it, such as
Rex, shows which agents work and which wait on the user without a plugin. It
writes the record `id=shoal`, never the root record, so it never overwrites
an agent's own report, and every report carries `app=shoal` and the space's
name as its title, since a report replaces its record whole. Each agent
integration declares whether its agent reports its own turns there, as Claude
Code does; for one that does not, Shoal also reports the turn its hooks
report. The daemon decides each report from the space's records and sends it
over the wrapper's existing connection whenever it changes, and the wrapper
writes it in one write to the terminal it holds: the daemon never opens a
terminal, since pty device names are reused and terminal I/O stays in the
wrapper, and hooks write nothing there. One write keeps a report whole
beside the agent's own output, which the terminal never splits within a
write; it can still fall between two writes of one escape sequence the agent
draws, which garbles that frame until the agent next redraws, a cost Shoal
accepts for status a terminal shows outside the agent's screen. The wrapper
reports the agent's exit itself, also while the daemon is away. Reports go only to a terminal that
answers the protocol's query: before it hands the terminal to the agent, the
wrapper sends the query followed by a primary device attributes request,
which every terminal answers, so a device attributes reply arriving first
means the terminal does not read the protocol, and a generous time limit
guards only against a terminal that answers neither. tmux, zmx and Zellij
answer only that request, so Shoal sends nothing into a host that would drop
it; no host declares support and there is no option. Each launch queries
again, a detached one sends nothing, and input typed during the query, before
the agent starts, may be lost. A thread or approval request that waits on the
user blocks the space, and one a coordinating agent holds does not, since
`blocked` means the user must act; completion and ready marks show as `done`
between the agent's turns, so a new turn replaces them.

tmux is an opt-in host for users who already work in it, so agents run in their
tmux panes without a zmx session nested inside one. tmux draws panes through its
own emulator, so images and notifications reach the outer terminal only through
its passthrough, off by default, and an attach redraws tmux's screen; Shoal
documents these limits rather than working around them. `terminal.tmux.enabled`
lets tmux serve its panes and, unset, follows whether `terminal.default_host`
names tmux, so choosing the default host opts in everywhere and tmux users who
set neither keep zmx. Inherited variables cannot tell which of nested hosts is
innermost, so hosts claim a terminal in a fixed order, the opt-in multiplexers
last: a place of a host that claims a terminal before tmux, such as a zmx
session, keeps its own behavior inside tmux. A tmux session is a
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
zmx. A launch refuses a zmx older than 1.0.1, as it does an old tmux or Zellij:
since 1.0.0 a zmx reaches only servers of its own version, and 1.0.0's macOS
release could not reach its own sessions. Shoal does not restart agents left in
sessions an older zmx started; it reports those sessions as unreachable. The tracked wrapper
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
session joins no group, so it goes to `terminal.default_host`. Launches in a place of an enabled
host run in place: a zmx session cannot show another session to its terminal,
and a host such as Herdr reads agent state from the place's process. A caller without a terminal, such as an
orchestrating agent's tool call, starts the agents of `start` and `restore --now`
detached through the daemon, as a scoped caller does: run in place, the agent
would hold the call until it exits and die with an interrupt of the caller.
An agent Happy runs leaves the call at once anyway, so such a caller runs it
in its own process once the workspace is prepared, as a start here does.
`start --here` keeps the in-place launch, for a script that waits on the agent's
exit code and for a scoped caller, such as a master session's terminal, that runs
an agent there; `restore --focus` keeps it for callers outside Shoal. A scoped
caller's `--here` claims its own launch's entry as the user's does; with no
slot free it leaves the agent queued and returns rather than holding the
terminal, since only the user's terminal starts agents beyond the limits. Its
agent's execution is its own space's, with that space's scope: an agent's wrapper,
and what it started, never count among the processes of another execution that
started it, and the wrapper leaves its starter's process group, which the
starter's wrapper signals as it stops, taking the terminal along when that group
held it, so the caller's stops leave the agent running. zmx reports no exit status, so a session launch
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
through the terminal hosts, since a demo is something the user opens and reads
and the agent reads and types into; a start no host takes fails rather than
run a demo without a terminal. Shoal knows how a demo reaches
the user only through presets, which reserve ports or lease a simulator and say
how to open it; the repository's `open` command replaces that on the demo's own
machine. A serving demo prints where it listens in its own place before each
run, so a terminal host that forwards a `localhost` link from another device,
such as a Rex app, opens it there, and opening it from a plain SSH session
names the `ssh -L` forward. Starting, reading and stopping
a demo are own-workspace actions without approval: the agent can already run
the command, and a demo only keeps it running and tells the user.
Shoal carries a demo's bytes to a device or another joined machine, rendering
nothing and parsing no demo's protocol. One request opens a stream to a demo:
it names a space and a demo, never a raw port or device, so a client reaches
only what a demo exposes, and the hub routes it to the space's machine as any
other request, failing at once while that machine is offline, naming it and
when the hub last saw it. The user and devices open any demo and an agent the
demos in its scope, without approval, as they read and type into one. The
demo's kind decides what the stream carries. A `server` or `web` demo's stream
is one TCP connection, which the owning machine opens to the demo's reserved
port on loopback, `127.0.0.1` else `::1`, and relays both ways under the
stream's flow control, each direction with credit of its own; Shoal never
reads HTTP, so WebSockets and hot reload work. A UI listens on a local port
and points its web view there, while the listing keeps the demo's own URL.
`shoal demo` on a serving demo another machine runs does the same from the
command line, on the demo's own port number when it is free, until Ctrl-C;
an agent may run it in the foreground for a demo in its scope. A `simulator`
demo's stream carries the port of a viewer of its device, as `web`. The
viewer is an external tool, serve-sim unless the demo's `viewer` names another
command: Shoal starts it and relays its bytes, never parsing its protocol, so
only the UI that speaks it changes with it. The daemon starts it on the
demo's first open from elsewhere, bound to loopback on a port reserved under
the demo's name, with the demo's execution's marker, so stopping, removal and
recovery stop it with the demo's other processes; it outlives the command's
runs and the daemon's restarts, which know it by the process recorded with the
demo, and a demo nobody opens runs none. serve-sim's HTTP mode suits the
hub's TCP streams, where WebRTC needs UDP. A missing viewer leaves the demo
running and fails the open, naming its install command; Shoal installs
nothing. Agents drive the device with serve-sim's or AXe's own commands, which
Shoal does not wrap. A terminal demo's stream is an attach to another
machine's session pointed at the demo's place: the owning machine runs a worker
on a terminal it makes at the viewer's size, which shows the place through the
host callback that shows a session there, and carries the same frames, its
screen one way and keys and resizes the other, so the host's own detach key
detaches and a device renders the bytes in its own terminal emulator. An agent on
another machine building and running on this one stays out: it needs leases
across machines.
The journal records a demo's start at its command's first run, when a start
reports it, and its stop when its record goes with the execution, so clients
showing demos follow them without polling; restarts between runs are not
changes. The listing carries each demo's kind and a web demo's URL, which
the daemon derives as opening does, so clients neither infer the kind from the
preset nor rebuild the URL.

`max_heavy_agents` admission belongs to the daemon, so concurrent additions and its
dispatcher share one count. Slots derive from what the daemon tracks: connected
agents, executions awaiting reattachment, resume records and starting queue
entries, excluding completed workspaces. A one-shot run such as a review is
short and holds no slot, so no count limits reviews, but its memory counts
toward dynamic admission and it waits for room and disk as a start does.
Agents therefore count however they started. A mode's `weight` alone decides
how its agent takes capacity, whatever the kind of space it runs in, since a
heavy chat uses the machine as a heavy workspace agent does: an agent of a
`light` mode counts nowhere, starting at once, holding no slot, and never
stopped for load or on idle, so a mode whose agents use little of the
machine, such as a plan run in a workspace, is not held up while heavy work
fills it, and a heavy chat's agent waits for a heavy slot and holds one as a
workspace's does. Critical or missing readings, low disk and its tool's
upstream limit hold every start, light ones too. The built-in modes that
start chats are light, so they stay so in a workspace. A chat without a
repository resolves its mode, and so its weight, in the global config, and
its slot counts toward `machine.max_heavy_agents` alone. A heavy chat's start with
no free slot follows a new workspace's: the chat is recorded at once, below
where it goes, so the caller can address it, its launch attached to it. A one-shot run's
admission is its in-memory request, which ends with the run, or with the daemon
for a run whose processes could not be proven stopped, so none outlives a
restart. `machine.max_heavy_agents`
limits the machine; the repository option `agent.max_heavy_running` limits each repository,
with a global default like any other option, so one name never means both caps.
The machine setting defaults to `dynamic`, which
admits from measured memory and disk instead of a slot count, so the machine
runs as many agents as it can without running out of either. CPU reserves
nothing: builds and tests use every core that is free, so a run's peak CPU is
not its cost, and more work on a busy CPU only runs slower, while memory and
disk do not stretch and are what break a machine. Agents start one at a time
and staggered; an agent reads files before it edits and compiles, so a recent
start reserves the memory a settled agent's workspace uses, learned as their
average with a floor, until its own is measured. The target is the warning
thresholds that `load` sets, so the user picks how hard to push the machine
rather than the mechanism's windows and intervals, and memory protection
stays the safety net when settled agents grow later. An idle machine always
admits one agent; critical readings admit none. When memory is short, work
that fits, such as a small CI job, starts ahead of higher-ranked work that
does not, so the machine stays busy; once that work has waited 15 minutes, a
fixed value, nothing it outranks starts until it fits, so a large start is
not passed over for good. An AI tool whose upstream a proxied agent finds
rate-limited or out of quota holds that tool's queued agents, which would only
fail, while other tools' start; a limit without a named end lifts after 15
minutes, a fixed value, so a new agent finds out whether it still stands. Dynamic is machine-wide, so a
repository's `dynamic` adds no limit. A positive number fixes a repository's
limit and `none` disables it. The machine's two limits take a number from 0
instead, and no `none`: a machine that should run no heavy agents, such as a
hub box, or no CI jobs sets 0, and one that wants no limit a large number,
which still keeps the disk rule below. Fixed limits keep dynamic admission's disk rule, since an agent
started on a nearly full disk is soon stopped by disk protection. Per-agent
memory profiles remain a proposal. CI jobs go through the same admission and
order: `machine.max_jobs` limits them apart from agents, and under `dynamic` a
job is expected to use the memory its recent runs reached, which a job
repeats far more closely than an agent, or an agent's before it has run, and
takes the one staggered start an agent would. A job reaches its peak late,
while it builds and runs tests, so until it ends it reserves what it has yet
to use, its expected memory less what its workspace uses now: counting it
only while it is young let each pass admit another job against memory the
running ones would soon need. No count limits jobs by default, since the
reservation already admits one only where its whole expected memory fits.
A job beside running ones also waits while the CPU, niced time included,
since jobs run niced, has stayed at or above the CPU warning level over its
window, since further jobs on a saturated CPU only slow every job down; the
first job never waits for it, so agents' load cannot starve jobs, and
agents never do. The reading comes from the overload monitor's samples,
taken while dynamic job admission applies, so no second sampler runs.
Automatic restores are typed queue entries ordered with launches, queued when
the stopped agent's processes are proven stopped. One daemon operation decides
what a queued restore waits for, from the agent's recovery record, whatever
carries it out, so the two cannot drift apart: a free slot once the user asked
for it; nothing blocking the space for an agent stopped because it was blocked;
an update for one stopped between turns; the recovery interval's healthy
readings, one restore per interval, for one load or disk space stopped mid-turn,
which the daemon marks in the record before the stop; and a free slot for any
other stop. A restore of an agent stopped between turns holds no slot and
holds back nothing, since that agent has nothing to do until an update
arrives; until then it also keeps its workspace from no cleanup, whose
removal cancels it and keeps the agent stopped for `shoal restore`. Woken, it
is admitted like a launch once free disk space has recovered, and an update's
admission holds through later stops. A restore waiting for healthy readings in
its wrapper keeps the agent's slot, so fixed limits let it take that slot back
rather than counting it twice, which would deadlock a full machine of stopped
agents, while dynamic admission measures its memory like a new start; disabling
recovery leaves it for `shoal resume`. Who carries a restore out is decided
after admission: the agent's waiting wrapper while it holds its link, which
it takes once the agent's processes are proven stopped and through which
admission hands it the restore, otherwise the daemon, which starts the agent in
a new place. A wrapper whose execution has ended can no longer reattach, so
the daemon takes its restore over then rather than dropping it, and a wrapper
whose session ends while it waits leaves its restore to the queue as a running
agent's does. An entry waiting for anything
besides a slot, such as those readings, an update or a blocker, keeps its
place without holding back work queued behind it: the stopped agent's slot still
counts and dynamic admission measures memory, so the limits, not the order, keep
new starts from undoing the protection. Admission binds the recovery epoch, which stops, reloads and
readings turning unsafe advance, so any of them before the agent restarts
returns the restore to its place. The slot passes to the
agent when its restored process registers. The entry is keyed by execution, so a reattached wrapper keeps its
place and a restart cannot restore it twice, and cancellation or failure leaves
the recovery record.
`shoal resume` queues restores instead of starting every stopped agent at once,
which overloaded the machine; `--now` starts them at once, as `start --now` does.
No wrapper waits for such a restore, so the daemon starts it through the queue
start worker as it starts a launch in an existing workspace: its stopped agent
holds no slot while it waits, so stopped agents past a limit cannot keep each
other from restoring. Its entry names the execution
without referencing its row, which ended with the agent, and leaves the queue
once the recovery record or last session is gone. A last session's admitted
restore counts as a launch, having no slot to take over.
`shoal queue` starts and cancels entries, and the daemon owns their order:
there is no hand-placed order, which would fight the rank, and positions count
waiting entries machine-wide, as admission does.
The queue starts the work that gets the most done with the fewest agents
running: work close to finished before new work, and work that other work
waits on before the work waiting. One ordering policy, a `Scheduler`
over a snapshot of the entries, spaces and what waits on what that reads
nothing else, orders admission and the listing alike, and owns the whole
order, so a policy is replaced without touching admission; a policy is code
in `shoal-schedule`, never a script or a loaded plugin. A space's priority, 0 to 100 and 50 unless set or mapped from its
issues' labels, goes first, so the user decides what matters. A space's agent
sets at most its own, as it gives permissions, but a label's priority is the
repository config's, so the labels an agent edits on its issues are not capped. A queued launch
carries its own until its space exists.
`machine.schedule` picks the policy. The built-in ones share one ranking and
differ only in what goes first within a priority: `balanced` the
tiers below, `speed` the work with the longest chain of work waiting on it, the
critical path, so a machine with room finishes sooner, and `cache` restores
whose prompt cache is still warm, so an agent continues before its cached
context costs a full read again, the one stopped last first, since its cache
is surest to be warm when its agent starts; the others then follow
`balanced`. An entry that has waited an hour ranks as if it led as well as
any other, so no schedule passes it over or starves work within its
priority, and its tier orders it among them as under `balanced`. Admission,
with `max_heavy_agents`, `max_jobs`, `load` and the rest of what measures the
machine, stays with each machine's daemon, the only one that measures it, and
no policy replaces it, so a schedule changes only the order. Within a priority, its tiers are `finishing` (a space with an open PR or a ready mark, CI
jobs on PRs and the default branch), `started` (restores of agents that have
worked) and `new`; an hour's wait raises an entry a tier, so nothing starves
within its priority, but never a priority: lower-priority work waits while
higher-priority work keeps coming, as the user chose. Priority and tier each flow along one dependency graph from
waiting work to what it waits on:
blockers, a workspace parent to its children, a space with a waiting thread to
the space that holds it, a stacked workspace to its base, and a space queued for a lease to
its holders. A chat parent passes on only a priority of its own, set or from its issues'
labels, or passed down to it, so an
orchestrator's children compete with every other worker. CI jobs and reviews have no rank of their
own beyond a job's tier: each takes the rank of the spaces it serves and goes
ahead of their agents, which wait on it. Within a tier, a launch of a new
workspace whose issue shares fewer areas, the labels with `[issue]
area_prefix`, with existing workspaces of its repository goes first, so that
queued issues run beside unrelated work rather than conflicting with it; an
area only orders, never holds a launch while a slot is free, every area counts
the same, the hourly rise keeps a launch that shares areas from starving, and
only new workspaces move, since other work's branch already exists. Then plan steps done, the space's age and arrival decide, and
`shoal queue` explains a rank taken from other work, for its priority and its
tier apart, and the areas a launch shares, with the spaces holding each.
For the same reason
every caller sees the whole queue, while acting on an entry keeps the scope
rules. Each waiting entry carries what held it back at the last admission
pass: memory, with what it needs and what is left, disk, a limit, a full
permit pool or a critical or missing reading, so a wait never goes
unexplained; the stagger is too brief to report, and the work an entry waits
for stays in `waiting_for`.
The queue
holds agents, not workspaces: a new workspace is prepared once its launch is
recorded, before its agent is admitted, so a waiting agent waits in a
workspace that exists. A
starting entry keeps its slot, with a short grace period for handoffs to the
place a host opens and daemon restarts, until its agent registers; a dead
start cannot hold a slot. The daemon starts admitted entries through a detached CLI
worker carrying the queuing terminal's origin, since terminal launch logic
stays in the CLI: a background place in the group of the place it was queued
from, such as a Herdr tab in its workspace, otherwise one of
`terminal.default_host`. A detached zmx session there is given a shell that
runs the worker, because zmx gives commands it runs itself no terminal input. That shell is zmx's bash, so the
session ends once its agent or workspace shell has run, whatever the exit
status, and only a failed start replaces it with the user's login shell.
A queued launch may be blocked by spaces, queued launches and issues, including
the blockers its issue's forge records, and admission passes it over until they
clear. A space clears once done at its current HEAD, not merged: a new
workspace's preparation waits for them too, then branches from the done blocker
and stacks on it, so blocked agents start before
review, and the existing restack on merge carries them to the default branch. A
commit after `done` is new work and blocks them again. One launch builds on one
branch, so done blockers on separate branches wait until all but one merge. A
launch blocker becomes its workspace once it opens one, and removed work
clears, since nothing is left to wait for.

Single-workspace actions select an explicit target, otherwise the caller's scoped
workspace or the workspace containing the current directory, then an interactive
picker; a deleted current directory provides no workspace context. Explicit misses
and other refused arguments fail without fallback, before any prompt for the
omitted ones; `start` first sends the daemon its launch, with what the pickers
choose still unset, which the daemon settles through its own start's steps up to
where they would create or fetch anything, so each input its start refuses is
refused before a picker.
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
workspace's or a chat's agent session here, such as `start` or `claude`, record
the space in `attach`'s history once the session is shown and still runs: when the terminal
detaches, or right away for a host that shows it without attaching the terminal. A
focused `start` that opens a place records the workspace once the place hosts it,
since the host may show the place before the workspace exists, and records a chat,
which it recorded before opening the place, once it shows it. So `attach -` returns to the
session last detached from, not one another terminal still shows or one that ended.
Every command records and selects workspaces and chats through the same history,
passing over removed spaces, and `attach` over chats that no longer run, which
have no session left; a command that acts only on workspaces keeps a chat it refuses
out of its history, so its `-` still returns to a workspace.
Bringing back an agent and bringing back a removed space are two commands, each
with one subject: `resume` brings back a space's agent, workspace or chat alike,
from whatever state it is in, and `restore` reopens removed spaces and returns a
workspace to a checkpoint. A removed space's agents resume in the same run that
reopens it, through `resume`'s operation, so it comes back in one command;
`restore` refuses a space that is not removed, naming `resume`. `resume` uses
the current workspace only when its agent can resume; its picker offers the
spaces whose agent can resume, with what each would resume, and `restore`'s
the removed spaces, each listing them without a terminal; completion also names
the workspaces with nothing to resume. Its agent picker names agents by name,
stop time and reason. Shell
navigation refreshes the Zsh line-editor prompt after changing directories,
while Bash uses its prompt command hooks.
An omitted required argument that names an existing record opens a picker on a
terminal; noninteractive and JSON calls require it.
An agent whose executables are not on PATH
is refused before any work starts and left out of pickers, the workspace menu
and completion; a start's picker offers only agents, since every start starts
one, and the review picker offers “No agent” to run no review. `config show` reports effective repository values with their winning layers and uses a registered checkout
before a workspace exists, then the picker; `--repo` names a repository's checkout directly. `status` shows any space,
a workspace or a chat, running or stopped, through one path: links, watched PR state,
completion, sessions and what awaits the user for both, and for a workspace also
lifecycle, fork-point changes, active work and leases; the daemon looks PRs up on each
request and keeps a failed lookup on its PR. Human output uses `Display` for enum values and a shared
semantic palette at the CLI presentation layer; machine output and stored values
stay unstyled. The output format resolves once after parsing, from `--json` or
`--text`, then `SHOAL_OUTPUT`, then text, so results, errors and prompts agree.
The format changes only the output, turning prompts and pickers off with it;
where and how a command does its work follows whether it has a terminal.
Internal commands name their format, and hooks run without the setting.
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
on: a plain fzf picker whose inbox view lists threads waiting for the user, pending approvals,
agents that need input, failed workspaces and ready marks, and whose
spaces view lists workspaces, followed by a view of recent alerts. It opens
on the inbox when that has rows, searching for the current workspace's items
when it has some; Tab reloads the next view in place, and each view binds only the keys that act on
its rows, with a key meaning the same wherever it is bound and its header listing
them by how often each is used. Acting on a row
runs the command that handles its kind. `inbox` and `list` open the same picker
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
and literal extra arguments. `shoal run <name>` is their only name, so a
configured command never competes with a built-in: there is no `shoal <name>`
shorthand, and an unknown top-level name prints clap's error, with its
suggestions of similar built-ins, followed by one fixed line pointing to
`shoal run <name>`, without reading configuration. `run` lists the resolved
arrays with their layers. Without a current or explicit workspace,
repository-only commands pick from the workspaces whose configuration defines
them. An unknown name reports a command error and never opens a picker.
Workspace fields expand once within individual
arguments; a standalone `{args}` places the caller's literal arguments.
`{diff_base}` lazily uses `diff`'s daemon lookup. Built-in `edit` runs the
`edit` command, otherwise `$VISUAL` or `$EDITOR` with the workspace path, through the same
wrapper, so the editor gets scope and port variables and counts as activity.
`review run` chooses between the `review` command and a local review, which
follows the review bot. The daemon runs the reviewer once, read-only in a
detached checkout of the committed HEAD, so the review judges what the agent
committed and never sees or changes its uncommitted work. The reviews of a
workspace, or of a PR, continue one reviewer session, so a later review keeps
what the reviewer understood and reuses its cached prompt; an AI tool finds a
saved session by the directory it ran in, so the checkout has one path per
workspace and PR. The session adds context and decides nothing: each review is
given its instructions and history in full, and a change of agent starts a new
session. The
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
agent run, which any feature can use: an AI tool's command without a terminal,
which may resume a session an earlier run reported, in its own process group, killed on timeout, cancel or a daemon stop and
found again by its marker after a crash. A one-shot run waits for admission at the
rank of the workspace it serves, ahead of that workspace's agent, which waits on it.
Protection stops a one-shot run before
any agent, since it has no running session to lose; it asks for admission again and waits
for healthy readings, as a stopped agent's restore does. A chat has no worktree or branch, so it
reviews no changes of its own: it names the workspace it reviews, or reviews
its one linked PR as a PR target, and its reviews are those of its linked PRs. A PR target, named by URL or `--pr`, resolves a same-repository PR through the daemon's
forge reads, as the agent account for a scoped caller and the user's logins otherwise, and
reviews its head branch against the PR base, reusing the owning workspace after
fast-forwarding it to the pushed head; local commits ahead stay, divergence is refused.
The daemon fast-forwards under a lifecycle reservation and the repository's Git gate,
after verifying the recorded worktree and its branch, so a switched or replaced
checkout is never moved.
Refining an issue before implementation is the plan mode's, which settles it with
the user in a chat. `repo setup`
likewise starts one agent, in the built-in `setup` mode in a workspace on the
`shoal-setup` branch. A setup agent asks the user about each setting before it
writes it, reading the options from the configuration reference, since only the
user knows which of the settings a project could use they want; it then asks
whether to commit the repository config, so it arrives as a reviewable commit that every clone
shares, or, for a repository that won't take that file or settings for one user,
to save it as the local config in the repository directory's `.shoal/`. Every
workspace agent already works in that repository directory, so one agent
serves both answers; a local result applies as soon as it is saved,
the agent tests it in its own worktree and its `done` removes the workspace and
its empty branch. `setup` takes the rules and reach of a base mode and adds only
its instructions, so config cannot change what it does. `config setup` starts
the same mode for the global config in a chat without a repository, whose reach
lets it read how every repository is set up. The user starts either to wait
for its agent, so neither waits in the agent queue. A `review` command such as tuicr owns its own
storage, exports and forge authentication, with explicit feedback handoff to
agents.

AI tools are providers: built-in ones plus `[agents.<name>]` entries, one table
per tool holding its launcher, resume command, launch mode and argument
templates, merged option by option across layers so a repository can replace
`command` and keep the global `resume`. Agents are not commands: every agent
starts through `shoal start` (or `contribute`), and `resume` and `restart`
bring stopped ones back, so a launch always passes the queue, takes a mode
and meets the one-worker rule, which a command per agent would skip.
`shoal run <name>` runs only `[commands]`, whose names may be agents', and an
agent's name needs no distance from the built-in commands; for an agent's
name without an entry, the error names `start --in`.
What a tool needs of the machine, its skill directory and proxy, sits under
`[machine.agents.<name>]`. Pickers and completion offer providers only. Agents run
as tracked agents with scope, an agent Git profile, and exit alerts. Their `{prompt}` argument combines general instructions
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
is one alert.
The retrying proxy lives in `crates/shoal-proxy`, which knows the wire protocols
and depends on no Shoal crate but `shoal-time`, as `shoal-forge` depends only on
`shoal-forge-types` and leaf crates such as `shoal-subprocess` and `shoal-time`. Each tracked agent that names one gets its
own, run by its wrapper, so a daemon restart never cuts a stream and the proxy
ends with the agent; Shoal points the agent at it through the integration, which
knows how that agent takes a base URL. An upstream is a server root that every
tool shares, each integration naming where its API is under it; a proxy without
one forwards to the base URL the agent would use without it, which the
integration reads from the launch, so it only adds the retries. All its requests share one outage budget,
so the agent's own retries never multiply the proxy's. Claude Code keeps its prompt cache for 5 minutes outside
a claude.ai login within its usage limits, while an agent waiting for CI or a
review idles longer, so a proxied Claude agent gets the 1-hour cache unless its
environment or settings choose a lifetime.

Agent launches use the execution wrapper: by default Codex CLI and Claude run with their
own sandbox and approval settings. Every Codex launch, chats included, turns on its
workspace-write sandbox's network access, which reaching the daemon's socket needs since
Codex allows a single socket only along with the network, whatever the configured
command. Every Claude agent, chats included, gets permission to run the Shoal commands that
finish its assignment, so completion never
depends on Claude Code's auto-mode check. A workspace agent's settings also tell auto mode, in
an `autoMode.environment` entry after Claude Code's own, that the workspace's
branch, commits and PR are the agent's own work, since the classifier reads
settings but not the agent's instructions; it then allows the
`--force-with-lease` push of a rebased branch. The classifier still judges each command, rather than an allow rule
passing every push. Every agent, chats included, gets the
configured `agent.dirs`, `/tmp` by default, as working directories, so it reads what the
tools it runs leave there without a permission prompt. A workspace's agent also
gets the repository's workspaces directory as one, so it works in the children it
splits there without leaving its sandbox or permission rules; chats get only
their temporary directory, plus the local config directory when a
chat sets up local config. Both trust the repository's checkout,
which Claude Code keys every worktree's trust by, and its shared Shoal workspaces
directory, or an explicitly placed workspace outside it, in their user config before
launch, creating the file if needed; Happy
launches and Claude and Codex app handoffs do so too. The user config is the one the agent
reads: a tracked agent's follows the workspace's `[env]`, while the app and chats
run with Shoal's environment. Agent guidance and templates become native
CLI instructions or a first-message prefix for Happy Codex; desktop handoffs carry no instructions.
Agents always start as CLI agents; only `shoal open --app` hands a space to
a desktop app, in the directory its agent works in. The Claude app opens through the `claude://` link that `claude --desktop`
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
association, then an existing local branch with the derived branch name, rather than
suffixing a new branch; conflicts fail before the agent picker. Associations are idempotent and cannot
be replaced. Status and inspection expose them.
The derived branch is the issue number and at most three title words, short
enough to read in full in branch lists, while the workspace takes the issue's
title as its name, which says what the work is.

`start <link>` recognizes issue, PR and branch URLs and selects their registered
repository by origin identity, offering interactive registration when missing.
Issue links and numbers open the issue's workspace; numbers use `--repo`, the
current checkout/workspace, then a picker.
PR links open their head against the refreshed remote base and link the PR once
the workspace is ready, as issue links associate their issue; branch links open
an origin branch. Both use existing-branch ownership checks and retain an owned
workspace's diff base. A fork PR has no origin branch, so its head opens in the
PR's workspace at a commit instead, with the PR's base as its diff base, where
`--agent` starts its agent, and `--path`, `--git-profile` and `--base` are refused. Interactive `start`
without a branch, base or issue also offers the repository's open issues and PRs,
which open as their links do.
Every `start` starts an agent, one rule for a new workspace, a new chat,
`--in` and `contribute`, whatever else it names: `--agent`, else
`agent.default`, else a picker on a terminal, which shows only where no
default is set, and otherwise a refusal before anything is created that names
`shoal create`, which makes a workspace without one. The daemon applies the
default and the refusal, so a start from any client follows the rule.
`start --agent` launches only after
creation, setup, and the post-setup hook
succeed, or after an explicitly ignored setup failure, and retains the
workspace whatever the agent does. Desktop handoffs (Claude and Codex apps, T3) provide no
tracking or scope. A host, such as zmx or Herdr, runs Shoal's tracked agents,
chats and demos and shows them; an external app, such as Quay, is a UI that is
not a host and reads and acts on Shoal's state through `shoal_client`. It holds
a workspace while it shows it and queues agents, which the daemon starts
detached in `terminal.default_host`, which never names an external app.
Happy sessions are an optional integration behind the `agents.happy.enabled`
setting, off by default. Happy is an agent backend like any other: `shoal happy` is gone, and its
agents start as other agents do, with `--agent happy-claude|happy-codex`. They are the phone-driven flow: a console session that
Happy's daemon started in a non-workspace directory creates workspaces and starts
sessions in them. Shoal launches `happy <agent>` with the daemon's own flags so
the session registers with Happy and appears in the app, detached from the
terminal but inside the tracked wrapper (a background `shoal` process holding the
execution), so it stays stoppable and visible to cleanup. Happy's Codex mode takes
no prompt argument, so Shoal delivers prompts the way the app does: it creates the
session on Happy's server with the machine's own Happy login, encrypted as
happy-cli would, attaches the CLI through Happy's reconnection variables, and posts
the first message once the session is alive. A session that cannot receive its
first message is no working launch, so a launch Happy cannot seed is refused and
one whose delivery fails is stopped; nothing is left on disk for the user to
send. Shoal reads Happy's credentials only for this and stores none. A Happy-side
pre-spawn hook asking Shoal for a workspace was considered and not adopted.

Every space's lifecycle events persist in a separate SQLite journal alongside
lifecycle transitions and cleanup outcomes, survive ownership removal, and keep
increasing IDs. A chat's events go through the same recording path as a
workspace's and take the same shape, naming the space by its ID and kind, with
no event kind of one kind of space for what both record. Retention is per space, so one busy space cannot push out
another's history: a space keeps its events while it exists and for
`cleanup.restore.retention_days` after its removal, as its checkpoints and
attached files, and a restored space, which keeps its ID, keeps them; only its
high-volume events, its agent's turn states, its progress changes and forge
activity other than an item's closing, merging and reopening, keep a cap of the newest 500 per space.
A space's log (`shoal log`, `client::log::log`) is a filtered view of
this journal, not a second store, and so is Shoal's, every space's events
with Shoal's own, which belong to no space and stay for the global
retention: renames, restores, agent starts, stops and
exits, threads, approvals, alerts, and the spaces created and removed below
a space (in that parent's log) are journal events for it, each recorded where
the change is made. A scoped caller reads the logs of its own space and the
spaces below it; a removed space's place in the tree is the one its parent's
log recorded at its removal. Unscoped integrations replay or
follow them without consuming alerts; expired cursors report a gap for
resync. External branch changes are observed during daemon sweeps without
changing recorded ownership. Issue and PR association changes append `linked` or
`unlinked` events in the same transaction as the association mutation, carrying
the item's kind, canonical URL and relation so integrations can follow links without
polling workspace inspection.
Item selectors make the `pr`/`issue` word optional for numbers: a linked
item with that number decides the kind, otherwise the forge's issue read where
issues and PRs share one sequence. Where they do not, an unlinked number stays
ambiguous and needs its kind rather than a guess between two reads.
Ready workspaces and the chats of a repository with linked issues or PRs are
polled every 30 seconds, whether or not their agents run, through one path, so
an asleep space hears what wakes it; the
daemon keeps a separate activity snapshot for each item and appends an
`item_activity` event for each newly observed comment, edit, completed check,
conflict, closure, merge, reopen, or lookup failure. The first observation
establishes the baseline, and watch acknowledgements do not affect this stream.
The same poll queues each change on the space's watch cursor, holding
passing checks as the watch below describes, and wakes a running watch, so an agent receives activity without watching for it. A
poll reads spaces, their items, an item's independent parts and the agent queue's issue
dependencies with bounded concurrency, so one slow forge does not hold back the rest. Polls
read through the forge crate's shared request layer, which remembers each
account's last response per URL in daemon memory and repeats the read as a
conditional request, so an unchanged item costs a `304 Not Modified`, which
GitHub does not count against its primary rate limit. The memory is not
persisted: a restart reads everything once in full. The same layer reads each
forge's rate-limit headers and sends nothing to a forge that limits the account
until the limit ends; polls wait for it and the user gets one alert,
because a lookup failure would ask an agent to act on what only time fixes. A
429 is such a limit, never resent within seconds as a 503 is.
The layer sends a read again after a failure that may pass, since reading twice
changes nothing and one refused connection would otherwise fail a whole poll, and
a source's lookup failure reaches the agent at most once per report interval,
since an intermittent failure varies its error and passes between polls. A
failure that repeats reaches it once, until its source is read or fails
differently: each repeat would start an idle agent's turn with nothing new. A
failed read of a linked item itself is such a source: it keeps the item's last
snapshot and reports `lookup_failed`, so an outage never also fails the watch
or the Stop hook's wait that reports it. A watch of an item named on the
command line still fails, so a mistyped item does not wait silently.
Decision: with `[pr] watch_merge_runs` on, a repository has Shoal follow the
default branch's CI runs for the merge of each PR a space links as assigned
or opened, since an agent that slept or finished never heard that its merge
broke the default branch, and two agents then found and filed the break twice.
The background poll reads an opened PR, which no watch follows, only until it
sees it merge or close, and neither reports nor logs its other activity, since
an opened link does not ask for it. The first read that records the merge,
a watch's or the background poll's own, whose record no delivery the agent has
not taken holds back, starts a persisted lookup of the merge commit the forge
reports, never one Shoal guesses; the background poll lists its runs for a fixed ten minutes,
since a run that has not started by then is not coming, and links each as
related, so the linked-run watch reports its end through the paths every update
takes. The lookup and its unfinished runs keep the space from unattended
removal, so a done space stays to hear them, however long a run waits for a
runner. A `shoal merge` that pushes the default branch starts the same lookup
for the tip it published, once the tip holds the merged branch and the
upstream, as Git tracks it after the push, holds the tip, so a retry that
publishes a merge an earlier push failed to is followed too. It lists the runs
on the forge of the first URL Git pushed to on a network host, so a push only
to local repositories starts none; a poll that cannot find that host's forge
tries again until the window ends, then ends the lookup, so a host that
serves no forge never keeps the space. A repository that
merges locally thus hears of a broken default branch through the same path;
lookups are keyed by their commit, which a PR's or a push's names.

Messages to agents and alerts to the user are one record, an alert: a
one-way notice that records its typed kind, its sender (the user, a space or
Shoal) and its recipient (the user or a space), so one set of commands sends,
lists and withdraws them in either direction, and later threads build on it.
Shoal sends its own, naming the space they concern: what a user would
otherwise miss (busy resources and who holds them, port conflicts, exits of
agents launched in place, workspaces it removed or retained on its own) and
what an agent needs (answers, decisions, reviews, wakes, machine load). One
daemon operation decides whom an alert reaches: without a target it goes up,
to the coordinator or else the user, and with one it goes down to spaces the
sender's scope reaches, so configurable routes can replace that operation
later. Alerts stay in the terminal: the CLI shows the user's once, unless
asked to keep them unread, on request, in a picker or as a followed stream
that also raises the terminal's own notifications (OSC 9); `status`, in a
space and outside one, only counts them. Recording Shoal's own never fails
the operation it describes, repeated polled conflicts and identical alerts
collapse until read, and scoped processes read the user's only as their
`notifications` permission allows. An alert is separate from completion:
telling the user a PR is ready neither ends the assignment nor affects
cleanup. What a finished assignment found travels with its completion
instead: `done --summary` records one line under the rule for an alert sent
up and ends the completion notice with it, and longer findings are files
handed back with it. The sender may withdraw an alert while it is unread;
once read, it stays, because deleting it would not unsay it, and a
correction does. `hooks.post_agent_exit` exposes a space's tracked-agent exit
alerts to user integrations while it is a ready workspace or a chat that no
removal has, using normal configuration precedence and daemon hook rules with
the agent name, reported code, and process-completion status. Failure notifies
without changing the exit result or marking completion; removal uses its own
hooks. Desktop or push delivery was considered and not adopted.

An alert to a space waits until its agent reads it, which marks it read as
reading marks the user's. An alert sent up is one line, since it may reach
the user's terminal notification; one sent down may span lines, because
instructions to an agent often need several.
Agents read alerts through Shoal commands, and a pending watch
ends for a new alert. Alerts and item activity are pushed rather than polled by the agent,
because agents forget or misuse watches: built-in agent launches register a hook
that delivers both after each tool call and prompt, the only point where a
working agent accepts new context, except a subagent's tool call, whose context
the agent never sees, and replaces a running watch so nothing is
delivered twice. A space's alerts reach its agent once they settle: a run
queued in quick succession, such as the user deciding several approvals, would
otherwise cost a wake or a hook output each, so deliveries hold unread alerts
until none has arrived for a configured window, bounded so a steady stream still
arrives; reading them with `alert list` does not wait. The hook never waits for cleanup or an item change, which can
hold Shoal's item records for minutes: an agent discards the output of a hook
that outlasts its timeout, so the activity waits for a later delivery. Claude Code also runs a background hook, after a turn ends normally
(`Stop`) or on an API error (`StopFailure`), that waits for the next update and
wakes the idle agent with it through `asyncRewake`. The hook
tries a failed wait again instead of exiting, because a hook that ends leaves the
idle agent with no wait until its next turn; waits outlast a daemon restart,
including the time before the next daemon listens and knows the agent's scope. Codex
has no hook that can start a turn, so its tracked wrapper types a fixed prompt
into the place it runs in, where its host types into its places, once Codex is idle with updates, at most once
per idle stretch, and the prompt's hook delivers them. Claude Code's wake hook
does not run after an interrupted turn, so its wrapper prompts it the same way
while it idles after one, and also once no wait for its updates has run for
three minutes, longer than the hook waits between attempts, since an idle agent
whose hook ended for any reason would otherwise wait for the user. The daemon
logs how a wake hook's wait ends without updates, and the hook reports the
reasons only it sees, such as its agent's exit or a signal, so a missed wake
can be traced. Since a missed wake, a needless question or a restore can
still leave an agent idle with work outstanding, a mode's `idle_wake_minutes`
backs these wakes up: an alert wakes an agent idle that long, by the state
its hooks report rather than separate detection, and again after each further
period without backoff, so it reaches the agent through the same delivery as
any update. An agent that legitimately waits says so with `shoal sleep`, until
its next turn or after every turn, which keeps the timer from waking it but no
update, and lets Shoal stop it as above;
a done space is never woken. Only `worker` sets it: the chats wait for the
user by design. Besides that prompt, Shoal writes to an agent's
terminal only the interrupt key `shoal stop --interrupt` sends and the fixed
prompt with which `shoal resume` continues the interrupted turn: the daemon
never does, the prompts carry no forge text, and they are appended to anything
typed but not yet sent. Shoal does not
pre-trust hooks where the agent requires review. Identical unread alerts collapse.
An alert may attach files, which the daemon copies from paths the CLI checked
into the repository directory beside its workspaces: agents get that directory
as a working directory, so a sandboxed agent reads them there without a
prompt, and a copy in the worktree would be an untracked file that holds up
cleanup. A copy outlives its alert, whose path the agent's transcript keeps,
until the workspace is removed, and is named by the alert and the files'
contents, so an identical alert with the same files still collapses.
A workspace being removed takes no alert, since removal decides from the
alerts it has and deletes them with the workspace.
`alert list` reads alerts while `send` sends them down to one space,
a workspace or running chat, or, with `--all`, to every agent in workspaces
and chats, such as to tell them the network works again, or, with `--repo`, to
those of one repository, such as to have them rebase onto a fix: running
agents and those stopped, queued or waiting for their restore, which read it
once they start, since such a message is meant for all work in progress; a
workspace without any agent would hold the alert until removal. An alert
to a chat goes the way the user's own messages to it go, with its files kept
among the chat's. Scoped
processes may read their own space's alerts and send alerts down only to
the agents of the spaces below their own, which they coordinate.

Threads are how a space or the user asks for something whose answer is text,
separate from approvals, whose answer is yes or no. A thread is a
conversation between its opener, the user, a space or Shoal, and its holder,
the user or a space, whoever is asked now, with posts from both sides, so a
question can be discussed before it is settled, asked downward by the user or
a parent of a child, and passed on to another agent. A thread has a typed
kind, which decides the outcomes it takes: a `question` ends `answered`,
optionally on a choice, a `request`, which asks its holder to do something,
`done` once its opener accepts the work, and every kind `duplicate` of
another thread, `withdrawn` or `closed`, which only Shoal sets. Unknown kinds
and outcomes are refused rather than kept as text. Editors draw a thread with
a state that comes from what Shoal records rather than a field of its own:
`queued` while its last post is its opener's, `waiting` while it is the
holder's, else its outcome.

A space opens a thread with its coordinator, else the user, or with a space
below it; the user opens one with a space. A thread lives on the machine of
the space it is numbered in, while its holder may run on any machine of the
hub, which carries the posts between them. An empty thread notifies no one
and keeps nothing, so `thread open` only reserves its number. Each post
reaches the other side as an alert, through the alert delivery hooks already
use, waking an idle agent; the alert is queued in the transaction that
records the post and names the thread as its subject. A post may offer
numbered choices with a recommendation and a reason, or pick a choice the
other side offered by number, never by parsing text, so `--choose 2` and a
post that says "2" cannot be confused. `--wait` waits for the other side's
next post and prints it, marking its alert read, for agents without Shoal's
hooks; a wait an upgrade interrupts resumes after the same post.

The holder forwards a thread down, to a space its scope reaches, or up, to its
coordinator or the user, and the new holder reads what came before and
receives the posts from then on. A forward is a post that names the new
holder, with its note as text, so the thread's history shows who held it, and
it does not count as an answer. The opener resolves its thread, or the holder
of one Shoal opened, and the user may resolve any; withdrawing takes back the
holder's unread alerts of it, so nobody is asked to decide what no longer
matters, and the other side hears of any other resolution once it has heard
of the thread. `--undo` reopens a resolved thread. A space reads a thread it
took part in, as the space it is numbered in, a holder or an earlier holder,
or one a space its scope reaches took part in, and may link it to receive its
new posts; reading delivers nothing.

A thread waits while it is open and its last post came from the opener's
side. A waiting thread keeps the space that opened it from unattended
cleanup, as an agent waits on it, and ranks that space's queued work after
the holder's. The inbox lists the threads whose turn is the user's. `done`
closes every open thread the space is numbered in or holds, since finished
work waits on no answer, and tells an opener other than the space that the
holder finished; removing a space does the same. `done --undo` and a restore
reopen the threads closed for that cause, without Shoal's closing posts,
unless something resolved them since.

A post may point at code: a file or its lines in the worktree of a space the
poster's scope reaches, or of the workspace its chat runs in, since a
reference only reads the worktree. References belong to posts rather than threads,
since a request about several places, a reply that points elsewhere and an
answer that says where something should go all attach code mid-conversation;
the opening post's references are the thread's place in code. A worktree's
file is referenced, not copied, so a reference keeps the version of its file
as a Git blob, HEAD then and a snippet of its lines, which shows once the
code is gone. Shoal follows a reference as its code changes, when a read finds
its file differs from the version it last reached, through a diff of that
version against the file now, and records where its lines went, never mapping
from the original: many small diffs keep a range exact, where one diff after
heavy editing can misalign it. There is no background pass. The mapping is a
pure function over two texts in `shoal-protocol`, so an editor's language
server maps a range onto an unsaved buffer as the daemon does once it is
saved. A file renamed whole is found through Git's rename detection; code
moved into another file is not searched for. The repository keeps the
versions under one ref per space, whose commit's tree holds each blob the
space's references keep, since a ref per blob would slow every Git command
that lists refs; the expiry pass rewrites it as references go with their
threads and prunes as checkpoint expiry does.

An agent posts in its chat with `send <thread> --in-chat`, and its turn's
final message, which the `Stop` hook passes, becomes the post. A user talking
with the agent reads it as the agent wrote it, while a long command line in
the chat is hard to read; a user who is away gets it where they look. Shoal
cannot tell from a host whether the user is there: zmx counts its own
short-lived probes as clients, and an attach from a connection left open
counts while the user is away, so routing posts by attachment left them
unanswered for hours. Agents are therefore not told about attachment, which
only orders overload stops. A prompt the user submits in the chat becomes
the user's reply to the post, carrying its text, since the user answered
there, while one Shoal submits does not.

`send --in-chat` without a thread makes the turn's final message an alert,
for the same reason, through the path every alert takes. Its recipients and
files are resolved and checked when the command runs, so a mistake fails at
once, and the files are copied when the turn ends, as the agent left them.
A turn has one mark, post or alert, so a second one replaces the first and
says so rather than sending both. A prompt the user submits during the turn
drops an alert mark, since the final message then answers the user, and
tells the agent, which may send it again; a prompt in the chat marks the
chat's in-chat alerts to the user read, without recording a reply, since
alerts are one-way.

Threads and approval requests are named `S.N`: the number of the space they
belong to, then the smallest number no thread or request of that space holds.
A thread belongs to its opener's space, or to the space the user or Shoal
opened it with. Both kinds share the space's sequence, so a number names one
item and says which space it belongs to, and counting per space keeps the
numbers short because the user types them. Inside a space a bare thread
number names one of that space, the common case for an agent; elsewhere, and
for approval requests, a bare number or one of the other kind is refused with
the form or command to use rather than guessed. Their records keep a separate
internal key that is never shown. A thread keeps its number until its space
goes, resolved or not, so a late post is refused instead of reaching a later
thread, and a wait follows its thread's space by ID, which a renumbering
leaves alone. A
keep or merge request that no longer applies stays as outdated until the
workspace goes, so its number is never reused and a decision meant for it cannot
approve a newer commit. A proposed issue outlives its space, so its space's
number stays reserved until the last such proposal is decided, keeping the
number the user was told. An agent may
withdraw a pending request it no longer needs, as it may a thread: it stays as
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
`agent_token` per host; without one it reads with the user's token, as the
agent itself would, but changes nothing, so no change goes out as the user
unless `agent_token = { same_as = "user_token" }` says so. A change to a host
with no token for the account is refused before it is sent, naming the setting
and the reload the daemon needs, since a forge answers an unsigned change only
with a bare 401.
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
daemon, where scope applies: they act as the workspace's agent account, except
the user's own login among an issue's assignees, while
merging and a review's verdict stay the user's
decisions unless a permission gives them to a scoped caller: `merge` its
merges, as the agent account, and `pr_review` its
verdicts, as the host's `review_token`, a login of its own, since agents open
their PRs as the agent account and forges take no verdict from a PR's
author; the user's own act as the user's login.
A PR merge, now
or automatic, names the head commit a scoped request's approval bound, else the
one Shoal read before merging, so the forge refuses a head pushed since instead
of merging a commit nobody approved; Bitbucket takes none. A forge checks that
head when an automatic merge is set, and only GitLab keeps checking it until
the merge, so `auto_merge_at_head` names the forges where a later push still
merges. A forge that refuses the user's verdict, such as on its author's
own PR, says so itself rather than Shoal guessing first; a scoped verdict is
refused before any request when `review_token` is missing or its account
opened the PR, so the refusal names the setting to change. A review's line
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
reads pages only until the limit is met. A search for words matches them both
ways by default: GitHub's search by meaning misses exact matches, such as an
error message or a flag, that keyword search finds, so `both` adds its matches
to the keyword ones, and gives the keyword ones alone when that search fails,
such as under the stricter rate limit GitHub keeps for it or on a server
without it, so the default never fails a search keyword matching answers. A forge's rate limit for some requests, such as
GitHub's for searches, holds back only those requests, never the watches and
reads the others serve; a pause the forge puts on the whole account, such as a
`Retry-After`, holds back every request. `shoal forge api` passes any request under
the repository's forge API root through the daemon the same way, for reads no
command models. The daemon
cannot tell what another method changes, so a scoped caller sends only GET
unless its `api_writes` permission accepts that its writes may merge, approve or skip
required labels, or `"propose"` asks for each write, bound to its method, path
and body, and only GET in another repository, which that setting does
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
assigned only on request: a workspace's assigned issues are its work, while an issue
an agent opens is usually follow-up work. Browsing across repositories and review
tooling stay with clients.
An assigned issue shows on the forge who works it through its assignees, since
every forge has them and a label would need one per repository. The agent
account is assigned while an agent works the issue: a space linking it as
assigned has an agent of its own that runs, is queued, restarts or is asleep, or
an agent made the link and no agent of the space's own has ended since, as for a
child a coordinator creates for its subagents, and the space's work is not done. It comes off an open issue once none does, at
`shoal done`, a stop that waits for `shoal resume`, an exit, an unlink or a
removal, and goes back on when an agent starts or is restored or `done --undo`
withdraws the completion. Every Shoal acting as the same account shares that
assignee, so each records the issues it put or kept the account on and takes it
off only those, leaving one another Shoal's agent may work; it reads the forge
for whether the account is there, and settles it in the background, so a slow
forge holds back no start or stop. Once the background poll sees an issue lose
an assignee, the spaces linking it settle the account again, so one another
Shoal took off an issue an agent here works goes back on. The user's own login,
under `[issue] assign = "user"` or `"both"`, joins on every assigned link, whoever makes it, and
leaves through the link and unlink paths, removal among them: Shoal records each
login it added, per issue, so it takes back only its own and only once no space
links the issue for it, never one the user assigned themselves. A closed issue
keeps its assignees. A space's links carry whether an agent assigned each issue
and the user's login Shoal added, so a restore or a move assigns as before and
keeps Shoal's ownership, and a moved space leaves the agent account to the
machine it moved to. Assigning never refuses or undoes the link or the
transition: other assignees and forge failures come back as warnings or
alerts.

`shoal pr open` pushes to the branch's push remote, then finds or opens the branch's
PR and assigns it in one daemon request; a push remote on the origin's forge host
other than `origin` is a fork the PR comes from, so a PR Shoal opens is left unlinked only when removal started
after its check, which the result reports as a warning, or as opened when it
created the PR and assigning it failed. A new PR is ready
unless `--draft` or `[pr] draft` asks for one, because opening a ready PR is
what requests review.
An explicitly supplied description is preserved verbatim; when no description
is supplied, each assigned issue is referenced with `Closes #<number>`.
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

Ready-for-review marks are the agent's signal that it needs the user before
it can go on, in every space: its work is ready for review, or it has a
decision or answer to ask them for, as a plan chat does once it has checked
its issue. The shared `work` rule says so, so every built-in mode marks the
same way. They are a status signal for integrations, separate from messages
and completion: they never notify, complete, or change cleanup. A
mark covers a linked issue or PR, or the workspace when nothing is linked, and
carries no commit: what it waits for is the user, not a revision, and a chat's
checkout rarely moves, so it lasts until `shoal ready --undo`, unlinking its
item or removing the space, new commits leave it as it was, and marking again
refreshes it. Only an open PR whose
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
it marks each linked open PR whatever its head on the forge, and its issue or
itself. Its marks are journal events as a workspace's are,
and they show it `ready` in lists and the inbox.
A working agent never waits for the user, so its own `working` state outranks
its space's `ready`, as `needs input` does, and the inbox leaves out the marks
of a space whose agent works: the mark stays recorded and shows again when the
turn ends, so a space keeps no `ready` status while its agent works on the
user's reply, and the agent need not mark itself again.

Agent state is the turn state a space's agent reports from its hooks:
`working`, `waiting` for the user mid-turn, or `idle` before its first turn
and after each. Lists name them by what the user does: `working`, `needs input`,
and `waiting`, since an idle agent waits for the user or for activity that wakes
it; a ready workspace whose agent or command reports no state, or a running
chat whose agent reports none yet, is `process running`,
one where nothing runs `inactive`, a ready mark `ready`, and a recorded
completion `finished`, which only an agent needing input outranks. A done
space with stopped work is `finished` too: its stopped agent has nothing left
to do, and `stopped` would ask the user to resume it. A row's marker groups statuses by what they ask of the user, in a
shape that reads without color. Like
ready marks it is a status signal for integrations that never notifies,
completes, or affects idle cleanup, though a report from a space's own
process that the daemon does not receive alerts the user with the error, which
the hooks discard. The hook keeps its report on disk until the daemon answers,
so neither the alert nor the state depends on the call that failed or on the
hook outliving its time limit: the daemon finds the reports left, alerts, and
records each late, in the order its hook made it, unless a report made after
it was recorded first.
Every report counts from when its hook made it, and a chat's late report
only while the chat runs the agent that made it; overload protection reads it only to stop
idle agents first. A workspace keeps the
latest report; a report from a tracked execution shows only while that
execution runs and ends with its record, while one from outside lasts until
replaced or removal. Changes and
endings of a workspace's state are journal events; repeating the current state is not a change and
keeps its start time. Shoal's terminal Claude Code and Codex launches, resumed
ones included, add the state hooks to the launch option that carries their
message hooks (Happy sessions do not), so the agents' own configuration stays the
user's; an interrupted Claude Code turn has no hook and ends with Claude Code's
idle-prompt notification, while an interrupted Codex turn stays working until
its next turn ends.
An idle report that ends a turn also starts the agent's prompt cache lifetime,
which each request renews, so the `cache` schedule knows which restores find
their cache warm. The AI tool's configured lifetime wins, then the one the
agent's own settings give, which only its hook reads from the environment the
agent runs with, then the tool's default: for Codex 45 minutes, the least its
OpenAI models keep a session's cache, which a resume in a new process reads
too, though some keep it for hours, and a 30-minute guess for other tools.

Progress is where a space's agent is in its work, beside its turn: a status
line in the agent's own words and the steps of its plan. Agents already keep
their steps in their plan tools, so Shoal reads them through `PostToolUse`
hooks instead of asking agents to report them: each integration only turns its
hook event into a change, whole-plan tools replacing the steps and task tools
changing one task by its ID, and the daemon keeps the list. Agents without a
plan hook set the steps from a task list. Progress outlives the agent until the
space goes, so a finished space shows where its work ended, and like agent state it
never notifies; coordinators read it on demand. Each change is a space's
journal event, kept as its turn states are since plan tools change it as
often.

Blockers are part of progress, since they say why the work does not move:
each sits on the space or on one step and names a space, an issue, a PR or a
CI run with a one-line reason. Threads and approval requests already cover
waiting for a person, so nothing else blocks. Plan tools set steps through
hooks that cannot carry blockers, so agents set them with `shoal progress`
flags, and a step's blocker follows the step by its tool's key, or its name
without one, and goes with it. A space is blocked while a blocker of its own
or one on every open step remains. One resolver serves queued launches and
spaces: the item poll looks each forge blocker and each blocking run's
workflow on the default branch up once, and a space blocker clears as
admission decides for a launch. A CI run blocker clears once a later run of
its workflow on the default branch passes, so a broken default branch that a
merge fixes releases every agent it blocked without anyone touching them.
Becoming blocked is the one blocker event that notifies, through the path
that reports `done`, so a coordinator learns of it once. A blocked agent has
nothing to do, so once its turn ends Shoal stops it as an idle stop does and
its restore waits until nothing blocks the space instead of for an update;
`shoal done` removes the blockers and cancels it. A chat comes back the same
way through its automatic restore.

With `[pr] progress` on, an open draft PR the space opened or was assigned
shows its progress in a section at the end of its body, which belongs to
Shoal: the daemon writes it through the same forge-edit path as the space's
other changes, recorded as its own so the watch does not report it back, at
most once a minute per space with the latest progress written in the end. The section follows the PR's draft
state, not the ready mark: a draft is the work in progress, while a PR out of
draft is read by reviewers, who read the description, not a live plan. Each
publication reads the PR and writes the section only to a draft, removing it
otherwise, so leaving draft through `shoal ready`, `shoal pr edit` or the
forge, which Shoal's own edits and the background poll report, takes one path.
It is opt-in: a status line written for `shoal list` reads as noise in a
description written for reviewers, so only someone following agents on the
forge, who opens their PRs as drafts, turns it on.

A chat is a conversation with an agent about a repository, not an assignment,
so it runs in the registered checkout instead of in a workspace: coordinating
work is one thing a chat may do. Every chat is scoped, as a workspace's
processes are: its agent carries a scope token, kept with the chat across
stops and restores, that covers the chat and the spaces below it, so only the
user's own terminal acts unscoped. The agent follows the workspace-agent rules
without what needs a worktree: it opens workspaces and chats only below its
chat and in its repository, never takes over one outside it, and renames only
its own chat. It proposes issues where its permissions say so, as a workspace's agent
does, recording no branch or commit, since it has none; approval opens the
issue from the chat's checkout and records it as the chat's. It starts at once from the default branch the
background sync keeps current, so an offline or diverged checkout still gets its
chat. Its
instructions describe the discussion only, from the `chat` rule that
repositories can extend like the other rules.
A chat's agent
runs under the execution wrapper as a tracked execution of its chat, through
the path a workspace's agent takes, so launches, stops, restores, wakes, exit
reports, turn state and restarts are one code path, and the chat runs while
that execution is recorded. Its agent's weight decides its capacity as for a
workspace's agent, including a restore between turns waiting for an update,
which the user who wants it sooner gets by attaching; only its clean exit
differs by kind, notifying no one, since the user is in that conversation. A
stop that keeps the wrapper restores the agent in place; one that ends it
leaves its stop record for `shoal resume`, or for the agent queue when the
agent is to come back on its own, which starts the chat's restore in the
place the chat ran in. Once the chat's agent ends, the requests the chat
decided go to the user, since no one attends them there. A stopped chat keeps
its record, and with it its name, links, files, messages, threads,
workspaces and temporary directory, until a restore runs its agent again. Only
removal ends a chat: `shoal rm`, or space cleanup, which treats a chat as a
workspace whose worktree is always clean, pushed and merged, its running agent
as use, a queued restore as a queued agent and its saved stopped work as a
workspace's, and takes the idle time of merged cleanup from when its agent
stopped. `shoal resume` resolves each target to a space in one lookup over the
shared namespace, so its picker, `--all` and a list of names act on chats as on
workspaces, an interrupted turn continues in a chat as in a workspace, and
`shoal restore --discard-agent` discards a chat's stopped agent as a workspace's.
Starts, renames and removals are journal events, and its agent's starts and
stops are logged as a workspace agent's. A workspace a chat opens is its child in the one tree of
spaces, unless it was created below a workspace, where it stays; reopening
it outside a chat for its issue or a picked branch returns it to the user, so
Shoal knows which chat coordinates which workers, while a start of its own
agent, with `--in` or the `--branch` it has, leaves it where it is. Removing the chat first removes the spaces below it through the
removal path of any space's children, and a stopped chat passes their events
on up, to the user at the top, unless it waits for its automatic restore. A
chat opens workspaces only in its own repository.
A chat may run in a ready workspace's worktree instead of the checkout, to
debug, audit or review that workspace's live work, committed and uncommitted,
with the commands that read a branch, rather than reading another worktree by
path. A workspace has one editing agent, its own: a chat in it reads and does
not edit, which its instructions say and nothing enforces until modes decide
what an agent may do. The daemon keeps the workspace's own agent one: it
refuses to queue a launch in an existing workspace while that workspace's
agent runs, waits in the queue or is stopped awaiting `shoal resume`, so the
CLI, coordinators and apps get one answer; a last session gives way, since its
agent ended on its own. A stopped or asleep agent gives way only to a start
with `--replace`, which discards it through the same operation as
`shoal restore --discard-agent`, so replacing needs the right to discard; a
running one never does, so a start never cuts off a live turn. A start on a
terminal asks instead of refusing, since the user is there to choose. It stays a chat, with its own identity, scope,
temporary directory and messages, and is not the workspace's child, since a
child's threads and approvals go to its parent's agent while the user attends
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
A chat's messages, including the user's request to rename it, wait and reach
its agent as a workspace's do; a removed space, of either kind, gives the user
the alerts its unread alerts stand for.
A chat passes a thread up to the user, who attends it. The approval
requests of the spaces below it come to it when it is the nearest space above
them with an agent, and it approves them by its own permissions or grants, as
a parent workspace does, and otherwise denies or passes them up; those it
has not decided go to the user when its agent stops or it is removed, as with
any deciding space. A repository may have several chats, so each call starts a new
one, placed as other launches are. A chat has a name as a workspace has, a label that may repeat; its files are
kept under its ID. A chat starts under its default name without asking
the user for one: its agent learns what the work is and renames the chat with
`shoal rename`. Renaming changes the record, not the zmx session, which zmx
names only at creation. A place that
hosts a space, a workspace or a chat, takes a new label only while it still
shows the label Shoal gave it, since another label is the user's: a chat's its
old name, a workspace's the label its previous title rendered. A place labelled
before its workspace's template, issue or branch changed keeps that stale label
rather than risk overwriting the user's. The daemon records the chat
before the tab or session that hosts it opens, which carries its name, and a detach
from its zmx session names its number; the chat's record names
the session it runs in, so `shoal attach` finds it there, not by a session name
zmx may have suffixed, and the chat labels the session with its ID, so the
chat's removal closes a session Shoal created and no workspace's end does. A chat waits for the user to speak first unless it opens on a
task: an agent that takes instructions apart from the first message gets them
so, and another gets them as its first message, which starts a turn and so asks
only for a short reply or carries the task.
A chat's launcher runs where only the checkout's path has a value, so one that
uses another workspace placeholder is refused rather than given the literal text.
A Happy chat has no terminal to focus: like a workspace Happy session it starts
detached, as Happy's daemon would start it, so the user continues it in the
Happy app.
A chat's agent runs the turn-state hooks a workspace's agent runs, without the
worktree hooks and lifecycle permission rules, which need a worktree. The
`agent-state` and `turn-failed` commands act on the space they run in and record
the chat's state as a workspace's, and the execution's session files keep the
session so the chat can move. Its state ends with its agent's execution, as a
workspace agent's does; keep-awake reads it as a workspace agent's.

A chat may also belong to no repository: a mode with `repository = false`,
such as the built-in `orchestrate`, starts it in `root_dir`, with the global
config alone, as a master session whose scope reaches every space on the
machine. It starts work below itself in any repository, receives what its
subtree reports as any parent does, and acts on other spaces as a coordinator
acts below it, while what the scope keeps from every agent stays the user's,
so the user can hand coordination to a tracked, scoped agent. Every registered
repository counts as its own, so its forge actions take the repository they
act in and their permissions as that repository's config resolves them, and
its proposals belong to that repository; it links nothing, since its links would name no one
repository.

A mode may have a forge label, `[modes.NAME] label`, which says that an issue
carrying it takes that mode next, such as the plan mode for one that needs the
owner's choices and the worker for one whose body holds a plan an agent
implements without asking. The label lives on the mode, so any mode, built in
or the user's, joins the flow the same way; labels are configuration, so Shoal
assumes no label scheme, ships none, and refuses two modes with the same
label, and issues with no mode's label stay outside the flow.
The labels decide the mode of every launch on an issue or PR that names
none, a `shoal start` without `--mode`, with or without a terminal, a
trigger's or an app's, since the label already says which work comes next and an
agent starting work on an issue means the same as the user; a script that
wants a fixed mode names it. An item with no mode's label gets the
picker on a terminal and the default mode without one; one with the labels of
several gets the picker on a terminal and is refused without one, naming them,
since no default is right. A bare `--issue` or `--pr` picks the item first,
among those carrying the label of the mode `--mode` names where it has one,
so its labels can decide the mode when none is named; a named mode alone
never opens that picker. The daemon reads the labels, so every caller gets
the same mode, and `shoal start` asks it for that mode before it offers its
picker.
The plan mode settles a plan in a chat, not a workspace: planning changes no
code, needs the user's attention and their authority to rewrite the issue, and
the chat coordinates the implementing workspace when the user has it start
one; the agent does not offer to. The plan goes
into the issue body because the body is all an implementing agent's prompt
carries. The planning task is the plan mode's built-in instructions, which a
repository extends like any mode's text, and it
keeps the discussion in the conversation: threads and forge comments serve
agents blocked during implementation.

One skill, `shoal`, describes what Shoal's commands do, what they refuse and
what follows from them, for workspaces, chats and sessions outside Shoal alike,
and states no practices: those are user rules, which a user can replace, while
an installed skill is kept in step with the binary. It is installed at user
scope, for every tool whose skill directory already exists unless a tool is
named, independent of the daemon and never from a scoped execution; its
availability registers nothing. Each skill directory records what Shoal installed there;
Shoal owns a skill only while it matches that record, so it updates or removes
only those, and treats files from before the record as its own and an
unrecorded file beside a record as the user's. Unscoped
commands other than the daemon refresh directories that still hold a Shoal
skill, so upgrades need no reinstall; the daemon may not share the user's tool
environment. Debug builds are development builds that share the user's home
with the installed Shoal, so unless tests in a temporary home set
`SHOAL_DEV_USER_FILES=1`, they change no user-level files outside their state
directory, skills included, and refuse the user's own state directory, whose
daemon serves every running agent, however it is selected, and writes of the
global config and the files beside it in the user's own config directory, from
which running agents read their settings; unit tests refuse that directory
whatever they inherit; the password
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
skips linked ones. Homebrew launches the binary directly, without a shell.

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
built-in default. A worktree file the current format rejects counts as absent,
since a branch from before a format change keeps its old file and must not stop
cleanup or anything else the other layers settle. The layers name the
rejection, which `config show` warns about, and the daemon tells the log and the
user once per error while it runs. A file that cannot be read still fails the
request. Lease hooks are the exception, since a lease is kept until its release
hook ran: they fail to resolve while the file is rejected, and removal resolves
the release hook only for leases it releases.
One declaration of each option, the field of the repository schema with its
kind, generates its built-in default, merge, resolved form and provenance, so a
reported layer is the one whose value is in use; the daemon resolves a workspace's settings
once per request and every use site reads them rather than layering on its own.
Every agent starts with `shoal start`, in a mode: a named entry that lists the
rules its system text carries, holds the instructions its first message ends
with, and says which kind of space it starts in and whether the space belongs
to a repository, only a chat ever belonging to none, so one command replaces a
command per kind of start. Without `--mode`, a start on a terminal picks the
mode, since it decides what the agent is told, and lists the start's default
first so Enter keeps it; without a terminal it takes the default, so scripts
and agents start as before. The picker follows the checks of the arguments
given, as every prompt does: one function in the CLI settles every start,
`--in`'s, `repo setup`'s and `contribute`'s included, refusing what the
arguments alone refuse, finding the repository and settings without asking
and checking the agent, then sends one `CheckLaunch` before its first
picker. The daemon settles that launch as it would start it, short of
recording anything: the whole placement of the mode's kind once the mode is
decided or the arguments name what only a workspace takes, and otherwise
what every kind shares, the repository, parent, given permissions and named
blockers. The pickers then follow what the launch leaves open, not a kind:
the mode, the repository where the mode starts in one, a new workspace's
source, a plan's issue and the agent, so a start that creates a workspace
asks its mode before its source. The mode alone decides the
kind of space, with no flag overriding it, so a mode's rules and instructions
always describe the space its agent is in. With `--in`, the mode's kind
gives the agent its role in that existing workspace: a mode that starts in a
workspace starts the workspace's own agent, creating no space, so a workspace
keeps its place, name and links across agents, and a chat-kind mode starts a
reader beside it in the worktree; there is no other way to start a new agent
in an existing workspace. The space records the mode
its agent started in, carried with the agent's name and model through every
restore, so a restored or moved agent is told its own mode's rules again. Rules are named and shared, since several modes need the
same guidance, while instructions say how to do one mode's job. Rules go
through the agent CLI's system channel so they hold for the whole session,
across compaction, and a restore passes them again, since a resumed CLI
starts without the arguments that carried them; instructions follow the subject once, in the
first message, so a restore never repeats a task the session already has. The
subject, an issue or the prompt given, is worded the same in every mode, so modes
differ only in what they add. Modes and rules are named sections whose options
layer one by one; their texts select repository additions by local/worktree
precedence and append them to the global text. Empty repository values suppress
only the addition. Repository Markdown files, at the root or in `.shoal/` like the
repository config and refused in both, and global files beside `config.toml` sit
below inline TOML values at each level; provenance names the highest configured
layer and the file it read, and reports the combined text.
What an agent is told comes in layers, in the order it receives them:
the skill, which says how each command works and which a space rule has it
load first; the facts of its space; the space rules; the user rules its mode
chains; and the task, its subject and mode's instructions in the first message. The facts and
space rules make up Shoal's guidance, which is built in and precedes every
agent's rules, in a workspace or a chat and in any mode, so adding rules never
freezes or replaces it. They depend on the kind of space, a workspace, a chat
in the checkout or a chat in a workspace's worktree, never on the rules a mode
lists, so a custom chat mode is told what a built-in one is. The facts state
only what is true of the space and hold no practices. The space rules are the
practices Shoal's own mechanics depend on, sent to every agent and kept minimal
and vague: what a command does and when its conditions hold belong in the skill
and the user rules, so a space rule never has conditions to keep in step with
them.
Practices beyond these belong in user rules, which a mode chains and a user
or repository may replace. The built-in modes list the `work` rule, one short
sentence per practice of Shoal's workflow. General work habits belong in a repository's own instructions, not in Shoal's
rules. Nothing is
seeded into the config directory: a seeded copy froze the default for whoever
had it. Rendering substitutes known fields once without evaluating their
contents, and never renders the subject, which the forge or the user wrote.
The former prompt templates are refused with what replaces them rather than
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
all permits remain owned until removal succeeds.
Simulator leases and permits record their acquirer: the user for an unscoped
caller, else the agent execution the caller runs or was started by. An agent's
lease ends with it, since capacity it no longer uses would otherwise keep other
workspaces waiting and its workspace from removal: one daemon operation releases
an execution's leases through each kind's release once its end is recorded or
its stopped agent is proven ended while its execution waits for a restore. A
lease whose workspace another lifecycle or resource operation holds then, such
as the stop that ended the agent or a resource hook, waits for it to end; the
stop then releases the leases of ended executions and of agents stopped while
their executions wait in its workspace, and the cleanup sweep, which also runs
as the daemon starts, in every workspace. A failed release hands the lease to
the user, so a failing hook does not rerun on every sweep. The user's lease
stays until released; the user's acquire takes an agent's lease over, never the
reverse. A demo outlives the agent that starts it, so a simulator demo takes
over the lease an agent acquired for it, and what a demo's command acquires is
the demo's; both end with the demo. A workspace's container project is such a
lease: recorded with its runtime before any `compose` call, so Shoal only ever
stops or removes a project it recorded, stopped on release with its volumes
kept, and taken down in the shared removal path. Each call reads the file,
with the workspace's ports in their variables, while the worktree has it, as
podman-compose needs; a stop or removal that fails with it runs again by the
project's name, so a deleted worktree or a broken file never keeps a project. One setup starts for
the workspace's own agent stays the user's until that agent starts and takes
it over. Ports, holds and secret grants keep their own lifetimes. User scripts own integrations
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

Permissions belong to spaces, not repositories, because a top-level chat the
user talks with may be trusted with more than a worker several levels down. A
space's set comes from its agent's mode: `[modes.NAME.permissions]` over
`[permissions]` over the built-in mode's defaults over each key's default. Each
key is then the narrower of that and the parent's effective value, so a child
gets no more than its parent from its mode. The modes that coordinate default
`issue_other_repositories` to `"direct"` so they can decide their children's;
`worker` keeps the key defaults, and so does a mode the user defines. `shoal start --permission KEY=VALUE` gives an
agent a permission, recorded with its mode since it belongs to that agent as
its mode does: restores and restarts keep both, and a new agent in the space
brings its own. A given value applies as given, whatever the mode or the
parent allows, because it is a decision about that agent, not a default: the
user may trust one worker to merge, or a parent give a child less than it has.
What bounds it is the giver: a value is accepted only from one who holds it,
the user, who holds every permission, or a space's agent up to its own
effective value. The daemon checks this wherever a scoped process brings a
value in; a restore or registration may also bring back what the space
recorded, but nothing more, so an agent cannot raise its own permissions by
replaying a record, and a reattaching wrapper keeps the value its execution
recorded. A value recorded before given values applied as given, when each
check capped it at the parent's, applies as given since: the records name no
giver to check it against, and none was ever wider than its mode. `shoal resume` and `shoal restart` take `--permission` too and change
the given values per key, `KEY=mode` dropping one, so a parent changes what a
child may do without ending its session or restating the rest; the mode stays,
since changing it would change the agent's rules mid-session. A dropped key
falls back to the space's value without one, which the giver must hold as it
holds a value it sets, so a drop cannot hand a child what a space between them
was given. The change goes
into the record a restore reads, so a restore here, in the background or from
the queue runs with it alike. A wrapper that restores its own agent, after an
in-place restart or a protection stop, restores it under the values that
record holds then, as a new wrapper would, rebuilding its push rewrites when
they changed; the daemon claims the record until the agent starts, so no
change lands unseen. A wrapper of a release before that would keep the push
rules it launched with, so its restore of a changed agent fails rather than
half apply it, leaving the record to a new wrapper. Effective
permissions are computed at each check from the current config, the recorded
modes and given values and the tree, never stored, so a config change reaches
running agents; only the push rewrites, which are
Git environment fixed at launch, wait for a restart. A check uses the acting
space's permissions, so a parent acting on a child acts under its own.

What only the user could do, such as administering Shoal, stopping the
daemon, releasing the user's holds, reading alerts, starting agents
beyond the limits, approving CI runs, merging PRs and deciding any request
below, is an action a permission key governs, because which spaces an
agent acts on is a question of reach and which actions it takes is one of
trust, which the user sets per mode. A master session gets every key at
`"direct"` but those whose action stops or bypasses every agent, `daemon`
and `api_writes`, which it proposes; other modes propose the keys whose
action a parent can judge and are denied the rest, so the user keeps an
action by setting its key to `"propose"` or `"deny"`. With `"propose"`
the action becomes an approval request bound to what it does, which lets
the same action proceed once, so an approval never covers a later, different
action. Deciding is a key of its own, `decide`, so a master session
decides what reached a space between it and the requester, grants
included, as the user would, but never its own requests, and approves an
action only where it holds that action's key at `"direct"`, so a key it
proposes never passes through a child it starts. The daemon checks
every request it receives, so a protocol client cannot step around the CLI;
the CLI asks the daemon before what a command does itself, such as writing
the config, so the two never disagree. Since config sets permissions, a
scoped process edits it key by key and sets a permission only to a value it
holds, as it gives one with `--permission`; replacing the file could drop a
narrowing, so that stays the user's. A setup, whose agent edits the file, starts
as `admin` allows, so a space that does not hold `admin` at `"direct"` leaves
it to the approval of whoever decides for it. A command sends several requests, so
the approval of the command grants a scope token its requests carry, which
takes that key's actions without asking again for an hour at most and dies
with the daemon. Forge writes these keys allow go out as the agent account,
so the forge shows who made them.

In a shared repository everything an agent opens is seen by other people, so
the `issue_open` and `pr_open` permissions can make a scoped `issue open` or `pr open`
propose instead of open (`"direct"` is the default; unscoped callers always open).
A proposal is an approval request whose record carries the command's options with
their defaults resolved, the workspace, branch, commit and agent, and copies of its
attachments in its repository directory's `.proposals` until no pending proposal
uses them, which a move of that directory carries over; the command
reports it and exits 0, because the agent goes on with its work and hears the
decision as a message. A proposal is routed like any request, to the nearest
space above with an agent and up to the user. Publishing to a forge other people see is the review
the setting asks for, so a space above approves one only where its own
permission for it is `direct`: it may then publish the item itself, so
reviewing a child's is within what it may do. A parent without that permission may deny it, with a reason or as a
duplicate, or pass it up; a duplicate only adds a finding to an existing issue
or proposal, which opens nothing. Approval carries data: the decider may replace the title and body, and the daemon then opens the item as the agent account through the same path
`issue open` and `pr open` use, recording and linking it. Denial may carry a reason,
and an issue proposal may be denied as a duplicate of an existing issue, which gets
its text as a comment, or of another pending proposal, which posts it once opened.
A decided proposal is deleted, since the message carries the outcome. An issue
proposal belongs to the repository rather than the space that made it, because
agents file follow-ups just before `done`; it opens through the repository's
checkout and agent account once the space is gone. A PR proposal is bound to the branch and its
commit like a merge request: the push still happens unless the `push` permission
holds it back, and a renamed branch or another
commit, locally or where the push went, outdates it at approval or when the agent
proposes again, and a pending one keeps the workspace
from unattended removal. Other agent writes, such as comments, edits and closing,
stay direct.

A proposed PR still publishes its branch, and agents push with plain Git too, so
a `push` permission of `"propose"` holds a scoped caller's pushes to the workspace
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
whatever the `merge` permission says, since it may push the default branch, and a
forge's `pr revert`, which pushes at once, is refused for scoped callers. A
pending push request keeps the workspace from unattended removal.
Deleting branches on the forge is `shoal branch delete`, which the daemon runs
for a chat as for a workspace, since a chat has no `git push --delete`; `push`
governs it as it governs a pushed deletion, with one `"propose"` request per
branch bound to the commit the branch points to. The daemon deletes each with
a Git push over HTTPS as the caller's account, leased at that commit, since
the forges' APIs delete a branch by name: the forge keeps a branch that a push
moved after the checks. Its token reaches Git on standard input, held by Git's
credential cache for that host alone, never in a file, the environment or argv.
It checks every named branch before deleting any, and refuses the default and protected branches, the head or base of an
open PR, a branch whose changes the default branch lacks unless `--force`, and
an active space's branch outside the caller's scope whatever `--force` says; a
space's branch is active while the space exists without a recorded completion
and the default branch lacks its changes, and a finished space's branch goes
as any other. The space keeps its local branch. Approving a merge
request whose branch moved since it was requested is refused as stale and marks it outdated.
Most requests the user approves only check routine things, so a permission
of `"gate"`, between `"direct"` and `"propose"`, has an agent decide them
first, opt-in since no built-in mode sets it. The gate is not a reviewer: it
checks conventions and mistakes, and the user's attention is what it saves, so
it decides even when unsure, denying with a reason the agent can answer by
asking again, and passes a request on only for a case the user wrote down in
`[gate] escalate`, which the daemon checks it quoted word for word. It decides
before a coordinator, since a coordinator's attention costs as the user's
does; an escalation then takes the `"propose"` route. Allowing approves
through the path a decision takes, so the gate adds no way to act. It reads
its rules from the registered checkout, never the requesting worktree, so an
agent cannot write the rules that judge it, and runs as a one-shot run that
holds no heavy slot, since agents wait on it. It may look further than its
prompt with the `shoal` commands that only read, under a scope of the
requesting space that the daemon refuses every change and that ends with the
run. That scope is cooperative like any other: its sandbox opens Unix sockets,
since neither tool can open only the daemon's on Linux, so a run that sets its
scope aside reaches what the agent it judges already reaches. A session per space and subject
keeps its understanding and the prompt cache across retries without mixing
unrelated requests, and every session opens with the same text so a new one
reuses that cache. A retry is a new request in that session, and the third
denial of one action stands until the action changes, so an agent cannot wear
the gate down. A failure denies, since nothing is allowed without a verdict,
and notifies no one: the agent can retry or ask. A gate that cannot be set up,
as without an agent to run, notifies the user too, since only the config fixes
it.
Agents also find problems that belong to another repository, such as a tool the
project uses, so `issue open --repo` files an issue there, on any forge, as the
agent account. Decision: every command that only talks to the forge names any
repository, registered or not, by `--repo` or an item's or run's URL, which
needs no space or checkout; a registered checkout is one way to name a
repository, never a requirement. Every caller reads another repository as it
reads its own, since `forge api` already reads it, while changing it stays the
user's, with the user's own logins, and a scoped caller's change there is
refused. Commands that
need a space or checkout, such as `link`, `watch` and `pr open`, keep their
rules. Item commands that need only a repository also run from a registered
checkout outside any space: they act as the repository's agent account under its
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
People outside the project see such an issue, so the `issue_other_repositories`
permission governs scoped callers separately from `issue_open`: `"propose"` by default,
`"direct"`, or `"deny"`. The workspace's required labels and templates describe
its own repository and do not apply there, and the issue cannot be linked, since
an assignment's cleanup follows the workspace's repository. A proposal records
the repository it opens in.
Configured resources may require human approval for scoped acquisition. The daemon
persists a request with a caller-supplied reason and the effective allocation settings;
only an unscoped caller decides it, or the nearest space above the requesting
workspace with an agent: a workspace approves only what it holds an approved
grant for with the same target and settings and otherwise denies or passes the
request up, as a chat does; the user decides at the top. A resource grant is
the only measure of what a parent may hand on, since resources are not
permissions. The decision is sent to the requesting
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
`release [kind [item]]` and `leases [kind]`, with `--space` instead of a
positional space. Like `unlink`, omitting the item or kind widens release
to every lease of that kind or in the workspace. Holds use the same verbs but
keep a space, a workspace or a chat, rather than share capacity; releasing leases without a kind
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
workspace: one that needs the user asks, and a thread it opened that waits, like a hold, keeps the
workspace from every unattended removal, completion and PR cleanup included. So
does an agent launch waiting in the agent queue to start in the workspace.

Decision: `shoal absorb` is how a coordinator takes a finished space's work
back into its own instead of merging it with Git and removing the space. It
needs no permission of its own, since it ends in removing a space below the
caller, which scoped callers may. Only done work is absorbed unless forced,
because absorbing ends the space. Code goes only where a branch of its
repository can take it, through the local merge `shoal merge` uses, generalized
to a target branch in its worktree, which need not be clean: every method makes
its commit without touching a checkout and the target fast-forwards, Git
keeping that worktree's other changes. Uncommitted files arrive as uncommitted
changes for the taking agent to review, never as a commit. Everything that is
checked is checked before anything moves; what binds to the absorbed branch,
PR proposals and push requests, is withdrawn rather than rebound, and the
removal path takes the space last, so its checkpoint and branch keep what was
left behind. Its children move up one level through one operation, which a
general reparenting command can reuse.

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
the workspace's results among its attached files, which, like every attached
file, stay for `cleanup.restore.retention_days` after cleanup removes it, so
the user or the parent's agent can read them after the worktree is gone. Files
an agent sends the user with an alert or a post stay the same way, since
the user may read the alert after the workspace is gone, and outlive the alert or post as a message's files outlive the
message: clearing or withdrawing an alert and resolving a thread leave them
among the space's files, which the user already sees and Quay may show, until
`shoal files rm` or the retention removes them. The alert or post names them by their records rather than by path,
so a file `shoal files rm` deleted drops out and a moved repository directory
moves them, and a coordinating agent that receives the alert or post
instead receives their paths as the message's attached files. A chat's `done` records its completion on the chat as a
workspace's does, with cleanup by the same setting and override, but no branch
another could integrate and no parent to tell, so it notifies the user and its
agent keeps running until completion cleanup removes the chat after the grace
period, with the done state shown until `--undo` withdraws it. A workspace or a chat is a space: the user reads both by their names and
selects both by number or ID, so what applies to both is named for
the space, and the flag that selects one is `--space`. Each space also has a
number, the smallest no other space holds when it is recorded, from one
sequence across repositories and a hub's machines, so it stays short to
type and read; it lasts as
long as the space, survives renames and stops, and is freed when the space goes.
Every argument that takes a space selects it by number or ID, through one
resolver in the daemon and one in the client; a name is a label people read
and never selects one, so names may repeat. The client refuses text that is
neither a number nor an ID before it asks the daemon. A number selects only a
space that exists, never a removed workspace or a queued launch, which are
selected by ID. `--add-blocker` is the exception: it names blockers, where
a bare number stays an issue, as `shoal start` takes one. A chat owns attached
files as a workspace does, since its
agent hands the user reports and pages too. A removed space keeps every
attached file by one rule, whatever its kind: they stay for the same retention
as its checkpoints, since an agent's restored session may refer to any of them
and a chat has no worktree to clean up, so its files are what remain of it.
Files are keyed by their owner's ID, which a workspace and a
chat never share, so no kind tells them apart: an owner is gone when neither
has the ID, and a record of its own says how long its files stay, apart from
a workspace's removal record, which a disposable workspace lacks. Each attached file records who attached it: the user, or the
agent whose process tree sent the request, by both its execution, which pins
the run, and the session it reported, which outlives a restart or resume and
names the conversation a client shows the file in, a chat's agent as a
workspace's, since it runs as an execution of its chat; like scope, this is
cooperative.
The journal records each file's record as it is added and deleted, by store
triggers so no route of attaching or deleting is missed, and carries the
owner's ID from the record because a removed space has no row
left to supply it; clients showing files follow them without polling. A file's optional title
rides on its argument as `PATH#TITLE`, the form forge attachments use for alt
text, because attach flags repeat and a separate title flag could not say which
file it names. Own-workspace withdrawal removes a
recorded completion and preserves associations and merge requirements, but never
defers cleanup: only holds, threads it opened that wait, a pending PR proposal or push request and queued
agent launches keep a workspace.
The daemon runs `hooks.post_done`
after persisting completion and before cleanup, using normal
config precedence and the space's hook identity plus the keep/cleanup choice,
where its agent works: a workspace's worktree, the worktree or checkout a chat
runs in, or a chat without a repository's `root_dir` with the global config's
hook. Only a workspace's completion reads a branch, verifies a worktree or takes
`--integrated`.
It is bounded, excludes lifecycle and permit changes, and reports failure through
an alert without undoing completion or blocking cleanup. Explicit signals
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
Agents may also start a workflow run on the branches whose runs they act on, on
every forge including Bitbucket. A started run is not a rerun of a PR's checks: on
Forgejo it has no head commit or event, so it cannot replace the rerun Forgejo
lacks before 17, while elsewhere only a run on the PR's head commit reports checks there.
`ci` gathers both: it reruns and cancels one run by its URL, or the checks on a
PR's head without one, and `view ci` reads a run as `view` reads an issue or PR.
Run numbers overlap issue and PR numbers, so only a number after `ci` names a
run. Scoped processes act only on runs of the default branch and of the
branches of the workspaces their scope reaches, in the repository they act in,
which the daemon reads from the run before acting, and read any run of their
repository.
A run or workflow URL, or `--repo`, of another repository is read by anyone and
acted on by the user: the daemon starts, links, reruns and cancels runs there
with the user's own logins, as it merges, also outside a workspace, and
refuses workspace processes those actions, which stay within their own
repository. A workflow file's URL also
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
removal hooks, take down owned containers and remove owned simulators, take a checkpoint unless the workspace is disposable, remove the worktree, and release leases with the record. Failures retain what is
needed to retry. Git forgets a worktree once it starts deleting its files, so
removal deletes what Git could not, and a later removal deletes what such a
failure, or a checkout that failed, left of a disposable workspace without Git,
proven its own by the `.git` link naming a deleted admin directory of its
repository, or, once a removal stopped after Git deleted that link and a
`git worktree prune` deleted the admin directory, by the directory's identity,
which a removal records before Git begins deleting it. Git no longer has its
HEAD, index or ignore rules, so the files left of a workspace that is not
disposable are read against its branch's commit, with the commit's missing
files taken as unchanged; those that differ are unsaved work, kept laid over
that commit in the checkpoint. Workspaces are cheap to remove because removal is reversible,
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
by removal, and when a copy starts; all but requested ones extend the newest
checkpoint instead when they would keep the same branch, HEAD and files. A
checkpoint keeps HEAD only when the unsaved files include an untracked nested
repository or a changed submodule, whose files it cannot hold.
`[cleanup.restore] archive_max_mb` caps only the checkpoint of an agent's exit
or stop, whose files stay in the worktree, so leaving them out loses nothing:
above it, that checkpoint keeps HEAD only, unless the files are a copy's
starting ones, which bounds the time each agent stop spends hashing large
untracked files and the versions a large changing file would pile up. Any other
checkpoint is asked for or may hold their last copy, so it keeps them whatever
their size. A snapshot
fails when HEAD or the branch moves while it is taken, so its record's HEAD
always matches its files. Checkpoints
expire after `retention_days`, a copy's starting checkpoint only once the copy is
gone, and so do those no workspace or removal record owns. An expiry pass that
deletes a checkpoint ref then runs `git prune` with the repository's
`gc.pruneExpire` (Git's two weeks when unset, nothing when `never`), never a
shorter window: large loose blobs alone never start Git's automatic gc, and the
user's window keeps their recovery time and the objects a `git add` or a
snapshot under way just wrote. Shoal does not repack, which would rewrite the
user's packs, so objects already packed stay until the user's gc. Checkpoint
objects stay in the repository's object store, since checkpoint refs also keep
a removed workspace's HEAD reachable after its branch is deleted.
Restoring a checkpoint reserves the `restoring` lifecycle state, which excludes
other lifecycle operations and holds new executions until it ends, refuses while another execution is
recorded or the checkpoint's HEAD or files collide with ignored ones, keeps the current state in
a checkpoint and stops when that one keeps HEAD only, then cleans untracked files under the current ignore rules, resets
the branch and applies the snapshot's files without staging them. Scoped
callers may do this in their own workspace, since the replaced state stays a
checkpoint.
Every removed space keeps one removal record, of either kind, in one table
whose `chat` column tells the kinds apart once the space's own row is gone, and
one restore path reopens both. The record of a workspace that is not disposable
(base, parent, links and stopped agents' recovery records, with its checkpoint)
persists before any file is deleted, so an interrupted or failed
removal stays restorable; records of spaces that still exist are not offered,
and a retried removal replaces the record with a new checkpoint while the earlier
one stays. When the removal's checkpoint keeps HEAD only, merged cleanup retains
the workspace. A workspace's record expires with its checkpoint; a chat's, taken
before its state is deleted, with its attached files. A restored space keeps its
ID and takes a new number, so its checkpoints, events, files and proposals are
its own again without rewriting. A workspace converted into a chat leaves the
chat its ID, so the conversion records it, with its checkpoints, under an ID of
its own, which its restore opens it under beside the chat. It goes below its recorded parent
while that takes it, else at the top level with a warning, and alone: the spaces
removed with it keep their records. `shoal restore`
reopens the branch through the existing-branch path, creating it at the removed
HEAD when deleted and refusing one that no longer contains it, then applies the
snapshot's files without staging them, links what the record names and saves its
agents' recovery records, which the same `shoal resume`
then restores as a stopped space's, since reopening is a step on the way back to
the work; once the
files are back, one transaction deletes the record and what keeps its files from
expiring. A chat restores in the place it ran, refused while the workspace whose
worktree it ran in is removed, naming the restore that opens both. A restore
first claims the record, keeping it and its checkpoints and files from
expiring for an hour while it runs. A scoped caller restores a removed space
only below its recorded parent, which must exist within its scope, as it
creates children only there; the daemon decides, and lists a scoped caller
only what it may restore.
Moving a space to another machine ends it there and continues it here, through
one path for both kinds: the package holds the space's removal record, whose
kind it names, with the agents' sessions and their files, and the import
records it as removed here and restores it as `shoal restore` does, under its
ID and below its parent where that is here, as it does each space the package
carries; only recording it differs by kind. A package carries every space
below its space, whatever its kind, since removing the space takes them: one
walk down `space_parents`, with the chats in each carried worktree, gives them
parents first, so each opens below its parent again and a copied tree keeps
its shape, each copy below the copy of its parent. `--no-recursive` instead
leaves the spaces below here, moved up one level through the tree's parent
link, and keeps the branch a staying workspace builds on, so they keep running
as they were. An import resumes the agents of every space it opens, a
chat's as a workspace's, unless `--no-resume` leaves them stopped, over SSH
as from a file, so a move behaves the same whatever carries it. Import is a
form of `shoal restore`, `--from` a package, since it opens removed spaces as
a restore does. `shoal move` is one command whose destination is a flag,
`--ssh`, `--machine` or `-o`, so the space selection, `--keep`, `--checkpoint`
and `--recursive` mean the same for every destination and a new destination is
one more flag; each destination sets whether the spaces below go by default,
and both flags are accepted with every one. `--machine` leaves them by
default, below the moved space, which keeps its ID on the other machine, so the
hub routes their events there and they need no new parent; they are detached
while its removal here runs, which would take them along, and recorded with
it until they are linked again, which a restart during the removal finishes.
Without a destination a terminal asks for one, a joined machine first while
another one is online, since the hub needs nothing set up between the
machines, and
anything else is refused, so an archive reaches standard output only through
`-o -`. A move always names its machine: a placement policy, which chooses
where a new space starts, chooses no move's destination.
Each package uses the lowest format version that reads it, so an older importer
refuses as newer what it would misread; a chat package from before chats moved
as their removal records, and a workspace package from before the record held
the workspace's branch or starting commit, are refused.
A move runs in the daemon as an operation the command follows, as creation,
`pr open` and merge do: its worker checks the spaces as a batch does, and for
each one has the daemon stop the agents, saving their sessions, and write the
package, delivers it, and has the daemon end the spaces here once their
destination has it, so a move finishes even after the terminal that started
it closes and is the one move path every destination takes. Only delivery
differs by destination: `-o` writes the package where the daemon writes it,
`-o -` hands it to the command, which holds the move's lock and ends the
spaces once it copied the package, and `--ssh` pipes it into
`ssh -o BatchMode=yes <host> shoal --json restore --from -`, so the other side
stays a plain command and the key must work without a prompt, as Git's
fetches need. `--machine` streams it to the other machine's daemon over a
duplex stream of the hub's links, which pipes it into `shoal internal
import-moved`, the same restore path, so that side too is a plain command.
That machine's claim of the space at the hub commits such a move: it records
the space as removed, then claims it, so a refused claim forgets the record,
and once the claim stands the record stays there for `shoal restore` whatever
fails after it. Each recorded move holds a
ticket its destination's claims carry; a source that cannot learn from the
destination whether it claimed the space asks the hub to settle the move,
which answers from its claims and owners and, when the destination has not
claimed it, refuses that ticket's later claims, so an import still running
there cannot take the space after its source restored it. While the hub
cannot be asked, the source keeps the agents stopped and asks again instead
of guessing, so the agents never run on both machines. The worker checks the spaces before any ends,
and where ending them needs the user it ends nothing and asks, so the command,
or an app, starts the move again with consent to what the question listed. A
move request for another joined machine's space goes through the hub to that
machine, which runs it as it runs its own; only the user moves a space over
SSH or to a file from another machine, since `ssh` reaches outside the hub's
trust, and a file stays where its machine writes it, so `-o` takes only this
machine's spaces. A move to a joined machine stays within the hub, so it
takes no permission key: a space's agent moves every space its scope
reaches there, from any machine, as it renames or converts them, except a
copy of its own space, which opens beside it, where a scoped caller creates
nothing, and so takes the `admin` permission. A move that
would stop the agent asking for it waits for its turn to end, as a
conversion does, recorded so a restart keeps it, since stopping the agent
mid-turn would cut off the command that asked.
The daemon records each space's move, with its destination and the stopped
work its spaces saved, before any agent stops, and the package's checkpoints
and phase once written, and the worker holds a lock on the space while it
moves it. A recorded move whose lock is free lost its worker, so the daemon
finishes it from the phase its record reached instead of guessing, in a
worker of its own: a package that left ends the spaces here, nothing that
left moves again, and an import that may have happened leaves the agents
stopped on both machines, as a dropped connection does, except on a joined
machine, where it settles at the hub. A worker that
outlives a daemon restart finishes its move itself, losing only the reports
its followers would have read.
Moving a workspace is a removal there and a restore here:
export writes the removal record and a Git bundle of its checkpoint from the
daemon, and the destination is checked before anything ends. Import records both as a removal of this
repository, the ref before the records, then runs the normal restore. Moving rather than copying keeps one workspace per
branch, so two agents never push it or watch its PRs, and the source's removal
stays restorable when the transfer fails. A space already removed, a
chat as a workspace, exports from its removal record, which stays, so it
remains restorable here. A copy, which leaves the space
running, keeps that rule by becoming new work under an ID of its own: a workspace's copy is one of its
checkpoints, which the import opens as a new workspace on a branch of its own
with its issue links but not its PRs or CI runs, and every copied agent, a
chat's included, starts from a fork of its session under a new ID, so the
original conversation never changes, even on the same machine. A copy of an
earlier checkpoint carries sessions newer than its files, which export, import
and the forked agents are told. The bundle leaves out the default
remote's default branch, which the importer fetches when it lacks the commit the
bundle builds on, since that branch only advances. Repositories match by origin,
and an import registers an origin no registered repository has as a clone, as
`start --machine` does, so a package needs nothing set up before it arrives.
The daemon reads a package itself, from a file or from the bytes a client
streams on its connection, and checks it whole before it records anything, so
every import takes one path whether a terminal, another daemon or the hub
delivers it; it records the spaces the package carries with its own and opens
its own, and the caller opens the rest in order, setting each workspace up
before the chats in its worktree.
Exporting every workspace of a repository checks each one before removing any, so
one that cannot leave refuses the set while all stay; a failure once removals
begin leaves the exported ones moved and the rest in place, each reported.
Copying several spaces needs no such check, since a copy removes nothing: one
that cannot be copied is reported and the rest are copied.
Agent conversations are the agent tools' private files, which Shoal never
reads or changes. Shoal tells clients where they are without reading them: the
daemon reports the sessions a space's agents reported, and `shoal_client`
finds each one's files by ID where its tool keeps them, under the client's
environment or one the repository's agents ran with, as an export does. An
export copies each reported session's files unread, and an import puts them at the
same path under the importing agent's session directory, replacing a file there
only with one that continues it, as a session that went away and came back does,
and leaving that to the installed Shoal in a development build; a session whose
files cannot be read is left out with a warning and starts over. A workspace's
files go in place once the restore that opens it reads the workspace's
configuration, which may define the agent, so the daemon keeps them with the
removal record until then and deletes them once the client has placed them;
ones it could not place stay, listed for the workspace's next removal record. The export records the agents' sessions, with the
exporting machine's ID when it has one, beside the moved records, which older
importers read. An import on that machine, one that received the session's
files, or one told to keep sessions, keeps the session IDs, which the agents resume from any directory, dropping another
machine's terminal places and waiting wrappers, and every reopen trusts the
workspace's path for those agents, since resuming skips the launch's trust
setup. Otherwise moved recovery records drop their session, place and wrapper,
and `shoal resume` starts them through the fresh-agent launch, with its trust
setup and guidance, told the space moved. Moving a chat removes it here,
stopping its agent, since a session that kept growing here would fork from the one that
continues there. Export asks first, whatever the space's kind, exactly when it
ends spaces besides the one named or releases holds. A space whose agent reported no
session moves with a warning, whatever its kind, its agent starting as a new one
there, told the space moved. Export, copy, move and restore each run one path for
a space of either kind, and only a workspace's worktree step, its checkpoint and
branch, differs. The spaces below a moving space and the chats in each moving
worktree move with it in its package as their removal records, which the import
records and restores, so they keep their IDs, their parents and their worktrees
wherever those are there. Import starts the chat through the chat launch, in its mode, in the worktree
of the workspace it ran in where that workspace is there and ready, since
workspaces keep their IDs across moves, else in the repository's checkout or,
for a chat without one, the importing machine's `root_dir`, with the agent's resume arguments, under a free name since its own may be taken there, in the background
when no terminal attends the import, as queued agents start. Export and import
move work through any byte stream, where the user runs both halves and chooses
their order. `move --ssh` runs both over SSH after checking Shoal answers there,
and ends a space here only once its import there succeeded, since a chat keeps
no record once it ends: it stops the agents of the space and of the spaces that
go with it, saving their sessions, exports it while they stay, and queues their restores here, keeping no file,
once the import there failed before recording anything; an import that may have
recorded the space there leaves them stopped, so its agents never run on both
machines. A space that cannot end here after its import, such as a workspace
that changed since its export, checked again once its removal hooks ran,
stays, existing on both machines, rather than
losing the newer work. Shoal keeps no
registry of machines, and
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
(`[cleanup.merged]`, off by default, 240 idle minutes) decides from the workspace's state
instead of its assignment: a merged branch, unsaved files only as leftovers the
removal's checkpoint keeps (the branch has commits of its own), no Git operation under
way, no waiting thread, nothing running, no leases, permits, directory users,
stacked workspaces, holds or spaces below it, and no recorded activity, file change or per-worktree Git
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
not its own: `ci ls` defaults to the runs on its linked PR's head, its other
CI commands act on that PR's checks, and `ci run` starts on the default branch
unless `--branch` names another. A space links each item under one relation, so every list, watch and
cleanup rule reads one answer for it: assigning an item replaces its opened or
related link without dropping its replay cursor, an assigned item refuses
another relation, and opened and related replace each other. Unlinking
selects a relation's items, one kind of them, or one item. PR links accumulate
without duplicates and require
every watched PR to merge, with HEAD present in at least one or merged into the freshly
fetched default branch, as a rebase after the merge leaves it. A PR's head need
not be the workspace branch, since an agent may push stacked PRs from its
worktree on other branches. PRs are looked up only once the assignment is done, and
confirmation proceeds to cleanup. The confirmed
HEAD persists so restart cannot query the same watch set again. Persisted manual
acknowledgement binds to exactly the recorded HEAD. Registrations distinguish watches from
acknowledgements.
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
on its cursor as its own comments and state changes are, and on the cursors of
the spaces below it whose activity its agent receives, so a coordinator does not
hear its change to a child's PR back while a child's own agent does: each field the edit
changed, and of labels and assignees only the names it added or removed, so
another edit read in the same poll is still reported. Only edits count, since
what the reads around any other action show changed is someone else's. A space
that comes below the acting space during or after a change has no record of it:
its cursor holds the change as someone else's once its own poll reads it, so its
new coordinator can hear the change back once. Shoal accepts that echo. Holding
the gates of the spaces above for every move into a subtree would make each move
wait for their forge requests and still miss what the joining space polled
earlier, and a cursor records what changed, not who changed it.
An assignee Shoal adds or removes on its own, as an agent starts or stops or a
link comes or goes, is taken as seen on every cursor of the issue, since it is
nobody's activity. It is changed under the issue's assignee gate, not the PR
gates, which callers that link, unlink and remove spaces already hold for one
space and could not take for the others without deadlock; a poll that records
an issue while that gate is held reports none of its details or state as
changed, so what it read before Shoal recorded the change stays unreported,
and another edit it read arrives with the next poll.
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
decides, since merging could only repeat them; `conflicts` still reports them. Once
the assignment is done, neither the watch nor the background poll checks the branch:
the work is finished, and a failed check would only wake its agent. A
linked PR leaves ordinary conflicts to the forge, avoiding duplicate reports, and
the watch and the background poll check the branch beside it only for files
changed on both sides, which no forge reports, and only where the repository
lists sensitive files. Each check against the default target is kept per
workspace with its head and target commits. Forgejo reads
come from its REST API and work anonymously for public repositories. The watch shares a
persistent cursor per workspace and item and reports changes between its reads.
Linking an item starts its cursor from a read made then, and an agent that begins a
new session, rather than continuing or forking one, registers as such, so before its
command runs the daemon starts each followed item's cursor again from a new read:
what the item's discussion and details held by then is history, which `shoal view`
shows, while its condition, such as failed checks, a conflict or its closing, is still
reported. A poll that began before the read, by the order in which the daemon numbers
its reads, is dropped whenever it finishes, since it may show the item before the
read, and the agent's first message counts the issue's comments. A watch of an item the space does not follow, or whose first read failed,
reports existing activity on its first read. Cancelling the watch discards that cursor.
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
A step's failure in a workspace, including one that keeps the step from
reading the workspace's state, shows in that workspace's status, and other
failures or a stalled loop in Shoal's `status` and the inbox; each notifies once per
run of failed passes, about its workspace where it has one, and the log has a
pass's failures only when they differ from the pass before.

There is no diagnostic command: each problem shows where the user looks and names
the command that resolves it. A failing command's error says what to run, `inbox`
lists failed workspaces and untracked worktrees (skipping a repository whose Git
gate is busy), and the daemon checks its PATH with the shared executable catalog,
`root_dir`, checkouts, the global config and recorded forge hosts when it starts
or reloads its config, recording an alert for each problem. Repair is a request
the commands that need it send: `adopt` reclaims, `setup --skip`
accepts a failed setup, and both restore verified worktrees and clear executions
proven stopped while preserving work and leases; `stop` stops survivors and asks
before forgetting an execution it cannot prove ended. Every other command reports
an unreachable or mismatched daemon itself, and `shoal cd` says when shell
integration is not loaded. Opening
persistence migrates the store, rewriting what it holds into the shapes this
release reads: Shoal promises its earlier versions no compatibility, so code
reads one shape of each record and file, and refuses a package or request of
another version, except from a process an upgrade left running, such as an
agent's wrapper. State that carries one upgrade across it,
for that release's daemon to consume once, goes in a later migration, which
refuses a database that still holds some and names the release to start
first; earlier migrations stay, so a database that skips releases runs them
in order. Under the daemon lock, startup then atomically
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
own environment. Inventories, and the command lines, run times, terminals and
foreground process groups Shoal shows or checks, read the kernel's process
records, `proc_pidinfo` and `sysctl` on macOS and `/proc` on Linux, rather than
run `ps`: stops, waits and status checks take them often, each `ps` cost a
process start plus more reads than Shoal needs, which skip a zombie after its
first, and Shoal needs no `ps` installed. Such a process takes what is remembered for it, failing that for
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
repairs normally. It is never automatic, and scoped callers request it only
as their `admin` permission allows. A deleted directory means
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
