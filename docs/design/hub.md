# Hub

This file covers `shoal-hub`: the hub role and its membership, what crosses
machines, shutdowns, sleeps and pauses, and where new spaces go.

A space starts on the caller's machine, or on the joined machine `shoal start
--machine` names, whose daemon starts it through the same launch; a start that
names none and runs its agent detached goes where the hub's placement policy
puts it, decided once, where its worktree goes, while each machine's own queue
and admission decide when its agent starts.

The hub is a role of the ordinary daemon, `[machine.hub] enabled`, which the
user's other machines join; the hub stays the machine of its own spaces. One hub
per user, without failover and never chained, though machines are addressed by
the ID the hub gives each at pairing, so nothing assumes one hop. Every machine
connects out and keeps one link, opening it again with backoff; the hub never
dials one. The listener serves HTTP and the link over a WebSocket on loopback by
default and handles no TLS, which a tunnel or proxy in front of it provides; the
hub and each member hold their own Ed25519 keys, exchanged at pairing and proven
by both sides signing a transcript of the other's nonce and an X25519 exchange
on every link, whose keys then encrypt and authenticate each numbered frame, and
a joiner proves its pairing code by an HMAC of that transcript instead of
sending it, so whoever ends TLS can neither pose as a side, use a code, nor
read, change or replay a frame. Only a refusal the hub sends before it can seal
arrives unauthenticated, so a member never takes one as final. A revocation
counts only when it comes sealed, and the hub admits a member and revokes one
under one lock. The same link runs
over any command's standard input and output through `shoal daemon connect`,
which relays them to the daemon's socket; running commands there as the user
proves more than a pairing code, so a join that way needs none. Link frames are
lines or WebSocket messages of at most 16 MiB. A credential records its kind, a
machine or a device, the hub refuses a revoked one or one another installation's
link holds, and versions match exactly: the hub refuses a member of another
release, reading only the protocol and version of its first request, with its
own version signed over the member's nonce, and records an alert naming the
update, while the member keeps working on its own. A member follows the hub's
version once that signature checks with the key it paired with, so nothing
between them chooses its release: a downloaded installation installs exactly
that release, older or newer, through `shoal update`'s checks, unless
`[machine.hub] auto_update` is off or the release is too old to read the state,
whose schema the signature also covers, and the daemon's restart on a replaced
binary carries its agents through; a Homebrew one is only told to upgrade, since
one package manager owns each installation. Pairing codes live in the hub's
memory and admit one join within ten minutes. Each machine owns its spaces'
records and reports a summary of each to the hub as it changes, all of them
whenever its link opens; the hub keeps a directory of those summaries, not
copies of the records, and serves one view of every machine's spaces from it,
keeping an unreachable machine's with when it last heard from it. Repositories
match across machines by their origin, never by each machine's ID. The directory
lists a space ID under the first machine that reported it: another machine's
copy is left out, routed nothing, and alerts the user, and takes the ID once the
first's space ends, while an import claims the ID, so a move hands it over
without a conflict, and the copy it hands over, which its machine retires, never
takes it back; a restore asks the hub first and refuses a space another machine
runs, going on while the hub cannot be asked, unless that machine is offline and
the hub keeps a package of the space: the restore then takes the space over,
importing the package under the space's ID, whose claim makes the space its
machine's, and the machine it was taken from retires its copy through the
removal path once its link opens, alerting the user when its copy changed after
the package. Silence alone never starts a second copy: a machine that lost only
the hub keeps working, so the hub only alerts the user once an intermittent
machine, or one that announced its sleep, has been unreachable for 30 minutes
while its spaces had something to do. A shutdown is no silence: every machine
joined to a hub, whatever its availability, hands its spaces over as it shuts
down, which is not a setting. Once the save-first shutdown stop below ends, it
sends the final package of every space at once, one stream each, with the start
its agent would make, and only the sending has to fit in the shutdown's window:
logind's delay limit, or for a signal a budget of its own that the service's
stop timeout adds to the drain's, so a slow hub never cuts the daemon's drain
short. The hub keeps each package that arrives whole as it keeps any, places its
space among the other connected machines, its own as the caller's, so
`always_on` machines with room come first and the hub takes the rest, and writes
the placement back; only once the machine, having read it, says it leaves the
space there does that machine import the package under the space's ID and queue
its agents' restores, so a machine that gave up waiting keeps its space, and one
whose word went unanswered keeps no restore, as the space may have gone; nothing
waits for an import. A chat in a workspace's worktree follows it to the same
machine, released and continued after it, since it opens only in that worktree.
That machine's takeover claim stands though the machine it came from may still
be connected, which forgets the space's restores so it does not come back at
boot, and retires its copy once its link opens, as after any takeover; work a
reboot moved away stays where it went. A space whose package did not arrive, or
that no machine took, keeps its place and restores at boot as before, and the
stop's notification names where each space went or why it stayed. The daemon
cannot tell a shutdown's signal from a logout's or a service stop's, and on
macOS has no announcement of a shutdown at all, so on every platform every
signal Shoal did not send itself hands work over, as its agents stop anyway;
Shoal's own restart, stop and upgrades go through its control path and hand
nothing over. A cancelled shutdown tells the hub the machine stays, so it
retires the copies of what went at once rather than at its next link. Nor is an
announced sleep: every machine joined to a hub, whatever its availability,
announces its sleep from IOKit's will-sleep notification on macOS, whose
acknowledgement it delays, and logind's `PrepareForSleep` under a sleep delay
lock on Linux. One small notice goes first, naming each space whose package lags
and since when, so the hub learns of the sleep however little else fits; then
the changed packages, one stream per space, all at once, until the platform's
allowance runs out, an `always_on` machine, which keeps no packages, sending
every space with work. The machine then keeps its link closed and admits no
queued work until it fully wakes, so a dark wake neither cancels the wait nor
races a takeover. A link that drops while the last report, which carries whether
the machine runs on battery and its level, said on battery below 5% counts as a
sleep announced at that report. Once a sleeping machine has stayed away 10
minutes, the hub moves each of its spaces that has something to do, an agent
working at the notice, a queued restore, or an update held for it while it
sleeps, which takes it over as it arrives, and whose package is current or
lagged less than 15 minutes before the notice, as a shutdown's hand-over does,
holding what it held for the space for the machine it went to; a chat in a
workspace's worktree goes only with that workspace. A space that lags more stays
for the alert and a manual restore; the machine retires the copies taken over
once it wakes, and one that wakes within the 10 minutes keeps everything. The
hub keeps a sleep in its store, and these numbers are fixed, not settings. What
a machine does once its user walks away, by closing its lid or, without one or
docked with it closed, leaving it idle as long as the platform reads as idle, is
`[machine] keep_awake`: `off` sleeps as usual, leaving its work to the sleep's
announcement; `finish` admits nothing new, keeps the machine awake only while a
turn works and then lets it sleep; `handover` admits nothing new and hands its
spaces with something to do over as a shutdown does, the walk-away or the
sleep's announcement, whichever comes first, starting it, holding the machine
awake until the hub confirms their packages, at most 10 minutes, and retiring
its copies once the user is back or the machine wakes, while a shutdown
meanwhile still hands over every space that did not go; `on` carries on. While
the user is there, `finish` and `on` keep the machine awake while agents work,
and `off` and `handover` hold nothing. A machine that admits nothing new says so
in its capacity, which placement skips. The daemon reads the lid and idle state
every few seconds only under `finish` and `handover`. `shoal pause` frees a
machine on demand, whatever its `keep_awake`, and on the hub's own machine too:
the store keeps the pause until `shoal unpause`, so it holds through restarts,
the queue admits nothing new and the capacity says so, every agent stops as for
a shutdown, the spaces with something to do hand over as a walk-away's do, and
the CI jobs end to rerun, the reviews requeue as for a sleep and the demos stop.
The hub's own machine, whose daemon keeps running, builds each package then and
keeps it as the hub, placing the space among the other machines; unpausing
retires the copies that went there too. Only the user pauses, since a pause acts
on every space of a machine; `--machine` reaches another through the hub. Space
numbers form one sequence across the hub's machines, which the hub holds: it
answers each report with a few spares it keeps for that machine, which numbers a
new space from them without waiting for the hub, and once none is left, as while
the hub cannot be reached, above its own highest. A space whose number another
machine's space or spares held first, as after that or after joining with spaces
of its own, takes the smallest number none holds, with its threads and approval
requests, and its machine tells the user and the space's agent; the space that
held the number first keeps it. A machine reports the numbers it holds without a
space, those of a removed space's issue proposals, with its spaces, and the hub
gives them no other space. An import's claim of a space's ID also keeps its
number for the claiming machine while the hub still lists the space, so `shoal
move --machine` and `--ssh` keep it; an exported package imported after its
source is gone takes a new one. Alert numbers, to the user and to spaces alike,
form another sequence the hub holds, which never gives a number twice, so a
stale number never names another alert: each machine reports the spares it has
left and the numbers its alerts took on their own, above the highest it knew
once its spares ran out or before it joined, and the answer tops its spares up,
confirms each such number no machine was given, and gives one another machine
was given a new number, whose sending space's agent hears of an unread one. Each
machine reports the user's alerts it keeps with its alert numbers, all of them
once its link opens, which replaces what the hub had for it, then each change;
the hub keeps a copy of every machine's, drops a revoked or departed machine's,
and writes the other machines' on a stream each member keeps open on its link,
all at once and then each change, which the member keeps in memory, never in its
store, and keeps while its link is down; the hub's own machine reads the hub's
copies. The daemon alone decides where a request runs: each request kind
declares, with no default, whether it goes to the machine of the space it acts
on, and every kind that only reads or changes Shoal's state does, while one that
needs the caller's machine (its execution, terminal or files, its agent's hook
and wrapper reports, a lease taken for it, a space created there) stays. A
request names the space, or the thread or approval request of a space, it acts
on, which the caller's daemon finds here or else in the hub's directory before
it handles the request; a thread a space's process opens is numbered in its own
space, so only the user's goes to the space named. A request for a space another
machine owns goes from the caller's daemon through the hub to that machine,
which decides it as its own caller's and names itself in the answer where the
caller shows it, as a space's status does; while that machine is offline, a
space's status is what it last reported to the hub. No command is special on the
hub. Each machine vouches for its own callers, so its unscoped callers' requests
and a device's go as the user's and a space's process's with that space's ID,
the reach of its scope and its effective permissions, which the owning machine
checks through the operation every scope check takes, under single-user trust as
scope is cooperative; the owning machine answers it on its own socket under a
token standing for that caller, whose vouch decides that request alone, never
another of the same space's, and never forwards a forwarded request again. A
request that can cross machines names a space, or an item of a space, never a
handle only one machine knows, such as an execution ID, and a long wait is state
on the owning daemon, never a connection held open or a client polling across
the hub. The daemon, never the CLI, reads the hub's directory: it says where a
named space runs, expands a selection across machines, named spaces wherever
they run and with the spaces below them on every machine, each after the space
above it, and lists every machine's spaces the caller sees for lists, the inbox
and pickers. A command's selection is this machine's spaces, and a picker hands
the daemon the request its command sends for the space it picks, which offers
the other machines' spaces that request reaches after them, unless the command
acts on the space itself, through this machine's worktree, terminal or files;
`--machine NAME|ID`, which repeats, makes it the named machines' spaces, a
selector that, as `--all` does, takes no named spaces, and narrows a picker to
them, while a named space is selected wherever it runs. Each machine reports
with each space whether a stop or a resume would take it there, so `--machine`
selects there what `--all` selects here, by that machine's last report; a stop
of an offline machine's space waits at the hub, and a resume never does. A
request whose answer goes on, as following an agent's session does, comes back
on a byte stream as that machine's daemon writes it, failing at once while the
machine is offline. Streams run on each link beside its calls, many at once,
opened by the side that reads them, and each holds its writer to the credit its
reader granted as it took what came, so a reader that stalls holds up neither
the calls nor the other streams; the hub relays a stream from one link to the
other the same way. Following a session sends its files' bytes where the agent's
tool keeps them, everything so far and then what the tool appends, unread: apps
parse each tool's format, so a tool changing it changes only the app. A diff of
another machine's workspace streams what that machine's Git writes under its own
config for the repository, run as a plain read that is no use of the workspace,
while the caller's Git decides its colour, width and pager, as it would for a
diff run there. An attached file's copy stays on the machine that keeps it: a
record read on any other machine names that machine instead of a path, which
means nothing there, while one that comes back to it gives the path again; a
client reads the copy by its space and record ID, streamed from that machine as
a diff is. Each machine reports the removed spaces whose records or files it
keeps with its spaces, since the hub's directory drops a space once it is
removed, and a request for a space's files that no machine lists goes to the one
that keeps them. A move of a removed space's record, and the read of its
package, go to the one that keeps that record. Either machine checks the
caller's scope as for its other removed spaces. What a space passes up to
another machine carries its files' records, so the user or coordinator there
reads them the same way, and a terminal there opens one only after fetching it
into a directory marked as Shoal's, its repository's where that machine has it,
else `root_dir`'s. Attaching to another machine's session runs Shoal's own
attach there, as the user, on a terminal that machine makes at the viewer's
size, so waking, restoring, choosing a session and the host's attach stay one
code path; a host callback decides how a place shows to another machine, a
client of the host on that terminal by default, and for Rex, which has none, the
viewer's own Rex app, found by the label it reports. The viewer's keys go there
unread, the host's detach key included, so Shoal has no detach key of its own; a
dropped link hangs that terminal up, leaving the session running, and the viewer
attaches again to the session at the same place, without waking or restoring
twice.

The hub forwards requests under a `[machine.hub] forward` path prefix to a local
address, such as an app's own server, a device-paired client that is never part
of Shoal, once a paired member signed the request with a fresh time and an
unused nonce. A request that needs the caller's machine is refused for another
machine's space, naming it. `shoal start --machine` forwards its launch through
the hub to the machine it names, whose daemon starts it as its own caller's, so
the space's queue, admission, cleanup and leases are that machine's, as are its
mode and the kind of space it implies, which the caller's machine reads of that
machine's modes through the hub, naming the repository by its origin and sending
the config it tracks, which that machine reads them under where it has none of
the repository; the launch carries its parent by ID, its repository by its
origin's URL, which that machine registers as a URL clone where it has none, and
its base as a branch or commit of that origin, or, for a base only the caller's
machine has, as a commit it carries first in a Git bundle of the history the
origin's default branch lacks, over a duplex stream of the hub's links, which
that machine fetches under a ref of Shoal's own and deletes once nothing needs
the commit: its launch failed, or no workspace there waits to start from it or
records it as a base that no branch holds. That machine answers the start once
the hub lists the new space, so the caller follows its preparation there, naming
the operation with its space as every request across machines names one, at
once, and a start that shows its agent in the caller's terminal, with `--here`
or `--focus`, starts it there at once and attaches to it as `shoal attach` does,
since the agent runs on the space's machine. A start that forks a session
carries that session's files there first over a duplex stream, which that
machine's CLI puts where its agent tool looks under the settings of the
repository the start goes in, as an imported move's are, so the fork finds them.
A copy goes as the copied HEAD, its base there, and a commit on it that keeps
the copied files, as a checkpoint keeps them, carried as a base's commits are;
its new branch there starts at that HEAD and writes those files in unsaved, as a
restore writes a checkpoint's, and its pending plan keeps that commit until the
worktree exists. A start below another machine's space, on whichever machine, in
a repository that machine has not registered, clones the URL of its origin,
which the parent's machine reports to the hub beside the key repositories match
by, and reads its modes under the config that repository tracks, which the
parent's machine reads for it. A device's start, which is on no machine, and its
read of the modes it may start in go to the machine its launch names, else to
the hub's `[machine.hub] default_machine`, else to the machine the device sent
it to. The space tree may span machines, as when a space moves without its
parent or starts elsewhere: what a space passes up reaches the nearest space
above with an agent wherever it runs, through the first space above that runs
elsewhere, whose machine goes on from there; the item activity of a space
without an agent goes as messages its machine's background poll sends while the
hub lists an agent above, each fitting a share of the coordinator's unread
messages, and stays on the space's cursor until the hub takes it, and approval
requests take deciders on the machines above after the local ones. What reaches
a space on another machine, an alert, a thread's post or a request's message,
carries the text its agent would read here, which the sending machine builds,
and a thread names such a space by the number and name the hub listed when the
thread named it. The hub holds what can wait for an offline machine, messages,
answers, decisions, stops, events and the removal someone asked for of a space
below one they remove, which keeps that space until it goes, in order until it
connects, for seven days, after which the sender hears it expired; anything else
fails at once, naming the machine and when the hub last saw it, and a machine
whose link is down keeps what its spaces pass up until it opens. A thread or
approval request that waits `[machine.hub] escalate_after_minutes` for a
coordinator whose machine is offline passes to the next space above with an
agent on a connected machine, or to the user. Each machine is `always_on` or
`intermittent`, which `[machine] availability` sets or else the hub guesses from
its link history alone, never from what powers the machine: `intermittent` below
95% of the last 7 days connected, gaps under 5 minutes not counting, and for its
first day by whether it reports a battery, so a new portable machine takes no
lasting work before the hub has seen it behave. The hub keeps that history in
its own state, counts its own machine from when its daemon ran with the machine
awake, and tells each machine its availability when its link opens and when it
changes, since a machine acts on it. The numbers are fixed, not settings. An
intermittent machine keeps the latest package of each of its spaces on the hub,
the one `shoal move --keep` builds but under the space's own ID, built without
stopping its agents: it looks at a space as its agent's turn ends and at every
space every 10 minutes, sends one whose HEAD, files, agents' session files or
records changed since its last package, at most every 5 minutes and each over a
stream of its own, and reports with each space since when it has changes the
hub's package lacks. The hub keeps one package per space, with the start its
agent would make, which places a takeover, replacing it only once the new one
arrived whole, until the space ends on its machine or the machine is revoked,
and refuses new ones while its disk is below the overload warning threshold; the
hub's own machine sends none, having nowhere else to keep them, but keeps one as
it hands a space over once paused, and counts as connected, so no other machine
takes its spaces over from them. These times are fixed, not settings. A machine
also tells the hub which agent tools it has installed, when its link opens and
as its config changes them, which the hub keeps for its open link alone, since
only a connected machine starts work. Placement is a policy in `shoal-schedule`,
beside the queue's scheduler and under the same rule: it reads only a snapshot
of the start and of each connected machine, never the hub's or a daemon's state,
and ranks the machines with a reason each, so a start and an app can say why a
space went where it did. Each machine reports its capacity with its spaces: its
repositories by origin, its `max_heavy_agents`, the heavy agents its admission
would start now, the memory new work may use, its waiting heavy agents and
whether its user walked away from it while its `keep_awake` admits nothing new
then or paused it, which every policy's machine skips, measured at most every 15
seconds, and at once as a pause starts or ends, and sent when it changes enough
to place by; the hub places from the latest report, not a live query, and keeps
it in memory alone. `balanced` sends lasting work to `always_on` machines with
room and otherwise keeps it on the caller's machine, beside its parent and
unpushed work; it never clones a repository onto a machine, which stays with
`--machine`. An `always_on` caller with room keeps the start, beside its
parent. Among the other `always_on` machines, a heavy start goes where a heavy
agent is admitted now, with the most spare memory. A light start goes where it
takes the least from heavy agents, since dynamic admission gives them the same
memory: first a machine that runs no heavy agents, then one where no heavy work
waits, then the one where the least waits. `external` lets an app on the hub's machine take the decision over,
registered as the placer, while a start waits at most two seconds for it and
otherwise places by `balanced`. Placement never moves a space after it is
created; rebalancing running work is a move. Browser sessions are a later step.
