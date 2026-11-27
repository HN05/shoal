# Hub

This file covers `shoal-hub`: the hub role and its membership, what crosses
machines, shutdowns, sleeps and pauses, and where new spaces go.
[Hub](../reference/hub.md) says what each command does and how joined machines
behave; this file gives the reasons.

## The hub role

The hub is a role of the ordinary daemon, which `[machine.hub] enabled` turns
on. The user's other machines join it. So the hub needs no server of its own,
and it stays the machine of its own spaces.

A user has one hub. It has no failover, and hubs never chain. The hub still
gives each machine an ID at pairing, and every request addresses a machine by
that ID. So nothing assumes that a machine is one hop away.

Every machine connects out to the hub and keeps one link open. It opens the
link again with backoff. The hub never dials a machine, so only the hub needs
an address the others reach.

## The link

The hub's listener serves HTTP, and the link over a WebSocket. It listens on
loopback by default and handles no TLS. A tunnel or proxy in front of it
provides TLS.

Shoal secures the link itself, so whoever ends TLS learns nothing and changes
nothing:

- The hub and each member hold Ed25519 keys of their own, which pairing
  exchanges.
- On every link, both sides sign a transcript of the other's nonce and an
  X25519 exchange. The exchange's keys then encrypt and authenticate each
  numbered frame.
- A joiner proves its pairing code by an HMAC of that transcript instead of
  sending it.

So a proxy can neither pose as a side nor use a code. Nor can it read, change
or replay a frame.

The hub may refuse a link before it can seal a frame. That refusal arrives
unauthenticated, so a member never takes it as final. A revocation counts only
when it arrives sealed. The hub admits and revokes members under one lock, so
a revocation never races an admission.

A frame is a line or a WebSocket message of at most 16 MiB.

### Joining through a command

The link also runs over any command's standard input and output.
`shoal daemon connect` relays them to the daemon's socket. Running commands on
the hub as the user proves more than a pairing code does, so a join that way
needs no code.

### Credentials and pairing codes

A credential records its kind, a machine or a device. The hub refuses a
revoked credential, and one that another installation's link holds.

Pairing codes live only in the hub's memory, so a restart voids every unused
one.

## Versions

The hub and its members run exactly the same release, since the protocol
keeps [no compatibility promise](overview.md#protocol). The hub refuses a
member of another release. It reads only the protocol and version of the member's first
request. Its refusal carries its own version, signed over the member's nonce.
The hub records an alert naming the update, and the refused member keeps
working on its own.

A member follows the hub's version once that signature checks with the key it
paired with. So nothing between them chooses the member's release. A
downloaded installation installs exactly the hub's release, older or newer,
through `shoal update`'s checks. The daemon's restart on the replaced binary
carries its agents through. The signature also covers the state's schema, so
a member never installs a release too old to read its state. `[machine.hub]
auto_update` turns following off.

A Homebrew installation is only told to upgrade, since one package manager
owns each installation.

## The directory of spaces

Each machine owns its spaces' records. It reports a summary of each space to
the hub as it changes, and every summary whenever its link opens. The hub
keeps a directory of those summaries, never copies of the records. It serves
one view of every machine's spaces from the directory. An unreachable
machine's spaces stay in it, with when the hub last heard from the machine.

Repositories match across machines by their origin, since each machine's
repository IDs are its own.

### One machine per space

The directory lists a space ID under the first machine that reported it.
Another machine's copy, as from a copied state directory, is left out and
routed nothing. Its machine alerts the user. The copy takes the ID once the
first machine's space ends.

An import claims the ID, so a [move](moves.md) hands it over without a
conflict. The copy the move leaves behind, which its machine retires, never
takes the ID back.

A restore asks the hub first and refuses a space another machine runs. It goes
on while the hub cannot be asked, since a lost hub must not keep the user from
their work. When that machine is offline and the hub keeps a package of the
space, the restore [takes the space over](#taking-over-a-space) instead.

### Space numbers

Space numbers form one sequence across the hub's machines, which the hub
holds. A machine never waits for the hub to number a space. The hub answers
each report with a few spare numbers for that machine, and the machine numbers
new spaces from them. With none left, as while the hub cannot be reached, it
numbers above its own highest.

A space whose number another machine's space or spares held first takes the
smallest number none holds. That happens after numbering offline, or after
joining with spaces of one's own. Its threads and approval requests follow
it, and its machine tells the user and the space's agent. The space that held
the number first keeps it.

A machine also reports the numbers it holds without a space, those of a
removed space's issue proposals. The hub gives them to no other space.

An import's claim of a space's ID keeps its number for the claiming machine
while the hub still lists the space. So `shoal move --machine` and `--ssh` keep
the number. A package imported after its source is gone takes a new one.

### Alert numbers

Alert numbers, to the user and to spaces alike, form another sequence the hub
holds. It never gives a number twice, so a stale number never names another
alert.

Each machine reports the spares it has left, and the numbers its alerts took
on their own. Those are numbers above the highest it knew, taken once its
spares ran out or before it joined. The hub's answer tops up the spares. It
confirms each such number no machine was given. It gives a new number to one
that another machine was given, and the sending space's agent hears of it
while the alert is unread.

### The user's alerts

[Alerts](coordination.md) says how the user's alerts form one set across a
hub. The hub carries them:

- Each machine reports the user's alerts it keeps, with their numbers. It
  reports all of them once its link opens, which replaces what the hub had for
  it, then each change.
- The hub keeps a copy of every machine's, and drops a revoked or departed
  machine's.
- The hub writes the other machines' alerts on a stream each member keeps open
  on its link: all of them, then each change. The member keeps them in memory,
  never in its store, and keeps them while its link is down.
- The hub's own machine reads the hub's copies.

## Requests across machines

The daemon alone decides where a request runs. Each request kind declares
whether it goes to the machine of the space it acts on, with no default, so a
new kind cannot cross machines by accident. Every kind that only reads or
changes Shoal's state goes there. A kind that needs the caller's machine
stays: its execution, terminal or files, its agent's hook and wrapper reports,
a lease taken for it, or a space created there.

A request names the space it acts on, or a thread or approval request of a
space. The caller's daemon finds that space here, or else in the hub's
directory, before it handles the request. A thread a space's process opens is
numbered in its own space, so only the user's thread requests go to the space
they name.

A request for another machine's space goes through the hub to that machine.
That machine decides it as it decides its own callers' requests, and names
itself in the answer for the caller to show. While that machine is offline, a
space's status is what the machine last reported. No command is special on
the hub.

A request that needs the caller's machine is refused for another machine's
space, naming that machine.

### Vouching for callers

Each machine vouches for its own callers. Its unscoped callers' and devices'
requests go as the user's. A space's process's requests go with that space's
ID, the reach of its scope and its effective permissions. The owning machine
checks them through the operation every scope check takes. That is
single-user trust, as [scope](permissions.md) is cooperative.

The owning machine answers on its own socket, under a token that stands for
that caller. The token's vouch decides that one request, never another of the
same space's. The owning machine never forwards a forwarded request again.

### What a request names

A request that can cross machines names a space, or an item of a space. It
never names a handle only one machine knows, such as an execution ID. A long
wait is state on the owning daemon, never a connection held open or a client
polling across the hub.

### Selections and pickers

The daemon reads the hub's directory, never the CLI. It finds where a named
space runs. It expands a selection across machines: named spaces wherever they
run, with the spaces below them on every machine, each after the space above
it. It lists every machine's spaces the caller sees for lists, the inbox and
pickers.

A command's selection, such as `--all`, is this machine's spaces. A picker
hands the daemon the request its command sends for the picked space. The
daemon then offers the other machines' spaces that request reaches, after
this machine's. A command that acts through this machine's worktree, terminal
or files offers only this machine's spaces.

`--machine` names the machines whose spaces a command selects, as `--all` does
for this one, and narrows a picker to them. Like `--all`, it takes no named
spaces. Each machine reports with each space whether a stop or a resume would
take it, so `--machine` selects there what `--all` selects here, by that
machine's last report. A stop of an offline machine's space waits at the hub.
A resume never waits.

## Streams

A request whose answer goes on, such as following an agent's session, comes
back on a byte stream as the owning daemon writes it. It fails at once while
that machine is offline.

Streams run on each link beside its calls, many at once. The side that reads
a stream opens it. Each stream holds its writer to the credit its reader
granted as it took what came. So a reader that stalls holds up neither the
calls nor the other streams. The hub relays a stream from one link to the
other the same way.

Following a session sends the bytes of the files where the agent's tool keeps
them, unread: everything so far, then what the tool appends. Apps parse each
tool's format, so a change to that format changes only the app.

A diff of another machine's workspace streams what that machine's Git writes,
under its own config for the repository. It runs as a plain read, which is no
use of the workspace. The caller's Git decides its colour, width and pager, as
it would for a diff run here.

### Attached files

An attached file's copy stays on the machine that keeps it. A record of it
read on any other machine names that machine instead of a path, which would
mean nothing there. A record that comes back to the keeping machine gives the
path again. A client reads the copy by its space and record ID, streamed from
that machine as a diff is.

The hub's directory drops a space once it is removed. So each machine reports
the removed spaces whose records or files it keeps, with its spaces. A request
for a removed space's files goes to the machine that keeps them. A move of a
removed space's record, and the read of its package, go to the machine that
keeps that record. Either machine checks the caller's scope as for its other
removed spaces.

What a space passes up to another machine carries its files' records. So the
user or coordinator there reads them the same way. A terminal there opens a
file only after fetching it into a directory marked as Shoal's: its
repository's where that machine has the repository, else `root_dir`'s.

### Attaching to another machine's session

Attaching to another machine's session runs Shoal's own attach there, as the
user, on a terminal that machine makes at the viewer's size. So waking,
restoring, choosing a session and the host's attach stay one code path.

A [terminal host](terminals.md) callback decides how a place shows to another
machine. By default it is a client of the host on that terminal. Rex has none,
so it uses the viewer's own Rex app, found by the label it reports.

The viewer's keys go there unread, the host's detach key included, so Shoal
has no detach key of its own. A dropped link hangs that terminal up and leaves
the session running. The viewer attaches again to the session at the same
place, without waking or restoring it twice.

## Path forwarding

The hub forwards requests under a `[machine.hub] forward` path prefix to a
local address, such as an app's own server. So one address and tunnel serve
both. That server is a device-paired client and never part of Shoal.

The hub forwards a request only once a paired member signed it, with a fresh
time and an unused nonce.

## Starting on another machine

`shoal start --machine` forwards its launch through the hub to the machine it
names. That machine's daemon starts it as its own caller's. So the space's
queue, admission, cleanup and leases are that machine's, and so are its mode
and the kind of space the mode implies.

The caller's machine reads that machine's modes through the hub. It names the
repository by its origin and sends the config the repository tracks. That
machine reads the modes under that config where it lacks the repository.

The caller's machine refuses a start whose agent the hub lists that machine
without, before it carries anything there, since that machine would refuse
it. It goes by the machine's last [capacity report](#capacity-reports), so a
machine without one, or without the repository, decides on its own.

### What a launch carries

The launch carries:

- its parent, by ID;
- its repository, by its origin's URL, which that machine registers as a URL
  clone where it has none;
- its base, as a branch or commit of that origin.

A base only the caller's machine has goes first as a Git bundle, of the
history the origin's default branch lacks, over a duplex stream of the hub's
links. So nothing needs a push first. That machine fetches the bundle under a
ref of Shoal's own. It deletes the ref once nothing needs the commit: the
launch failed, or no workspace there waits to start from it or records it as
a base that no branch holds.

A start that forks a session first carries the session's files over a duplex
stream. That machine's CLI puts them where its agent tool looks under the
repository's settings, as an imported move's are, so the fork finds them.

A copy goes as the copied HEAD, which becomes its base there, and a commit on
it that keeps the copied files, as a checkpoint keeps them. Both go as a
base's commits do. The new branch there starts at that HEAD and writes those
files in unsaved, as a restore writes a checkpoint's. Its pending plan keeps
that commit until the worktree exists.

### Following the start

That machine answers the start once the hub lists the new space, so the caller
follows the space's preparation there at once. The operation names its space,
as every request across machines does. A start that shows its agent in the
caller's terminal, with `--here` or `--focus`, starts the agent there at once
and attaches to it as `shoal attach` does, since the agent runs on the space's
machine.

### Repositories and devices

A start below another machine's space may go in a repository the starting
machine has not registered. It then clones the URL of the origin, which the
parent's machine reports to the hub beside the key repositories match by. It
reads its modes under the config that repository tracks, which the parent's
machine reads for it.

A device is on no machine. Its start, and its read of the modes it may start
in, go to the machine its launch names. Without one, they go to the hub's
`[machine.hub] default_machine`, else to the machine the device sent it to.

## The space tree across machines

The space tree may span machines, as when a space moves without its parent or
starts elsewhere. What a space passes up reaches the nearest space above with
an agent, wherever it runs. It goes through the first space above that runs
elsewhere, whose machine carries it on from there. Approval requests take
deciders on the machines above after the local ones.

The item activity of a space without an agent goes as messages its machine's
background poll sends while the hub lists an agent above. Each message fits a
share of the coordinator's unread messages. The activity stays on the space's
cursor until the hub takes it.

What reaches a space on another machine, an alert, a thread's post or a
request's message, carries the text its agent would read here. The sending
machine builds that text. A thread names such a space by the number and name
the hub listed when the thread named it.

### Offline machines

The hub holds what can wait for an offline machine, in order, until it
connects:

- messages, answers and approval decisions;
- stops and events;
- the removal someone asked for of a space below one they remove. It keeps
  that space until it goes.

After seven days, the sender hears that the request expired. Anything else
fails at once, naming the machine and when the hub last saw it. A machine
whose link is down keeps what its spaces pass up until the link opens.

A thread or approval request that waits for a coordinator whose machine is
offline passes on after `[machine.hub] escalate_after_minutes`. It goes to the
next space above with an agent on a connected machine, or to the user.

## Availability

Each machine is `always_on` or `intermittent`. A machine acts on its
availability, so the hub tells each machine its own when its link opens and
when it changes.

`[machine] availability` sets it. Otherwise the hub guesses it from the
machine's link history alone, never from what powers the machine, by the
rules [availability](../reference/hub.md#availability) gives. For its first
day the hub goes by whether the machine reports a battery. So a new portable
machine takes no lasting work before the hub has seen it behave.

The hub keeps the history in its own state. It counts its own machine as
connected while its daemon runs with the machine awake. The rule's numbers are
fixed, not settings.

## Packages on the hub

An `intermittent` machine keeps the latest package of each of its spaces on
the hub, so their work outlives the machine going away. It is the package
`shoal move --keep` builds, but under the space's own ID, built without
stopping its agents. The machine sends a space's package when it changed, at
the times [packages on the hub](../reference/hub.md#packages-on-the-hub)
gives, each over a stream of its own. It reports with each space since when it
has changes the hub's package lacks. These times are fixed, not settings.

The hub keeps one package per space, with the start its agent would make,
which places a takeover. It replaces a package only once the new one arrived
whole. It keeps the package until the space ends on its machine or the machine
is revoked. It refuses new packages while its disk is below the overload
warning threshold.

The hub's own machine sends no packages, since it has nowhere else to keep
them. It counts as connected, so no other machine takes its spaces over from
them. It keeps a package only as it hands a space over once
[paused](#pauses).

## Taking over a space

A space continues on another machine from its package, as after a
`shoal restore` of an offline machine's space, a
[hand-over at shutdown](#hand-over-at-shutdown) or a
[takeover after a sleep](#takeover-after-a-sleep). That machine imports the
package under the space's ID. The import's claim makes the space that
machine's.

The machine it was taken from retires its copy through the removal path once
its link opens. It alerts the user when its copy changed after the package.

Silence alone never starts a second copy. A machine that lost only the hub
keeps working. So the hub only alerts the user once an `intermittent` machine,
or one that announced its sleep, has been unreachable for 30 minutes while its
spaces had something to do.

## Hand-over at shutdown

A shutdown is no silence. Every machine joined to a hub hands its spaces over
as it shuts down, whatever its availability. This is not a setting.

The [save-first stop](power.md#the-save-first-stop) runs first. Once it ends, the machine sends the
final package of every space at once, one stream each, with the start its
agent would make. Only the sending has to fit in the shutdown's window. Under
logind that is its delay limit. For a signal it is a budget of its own, which
the service's stop timeout adds to the drain's, so a slow hub never cuts the
daemon's drain short.

The hub keeps each package that arrives whole, as it keeps any. It places the
space by its own profile, as it places a start, among the other connected
machines, its own standing in for the caller's. So under `balanced` the hub
takes what no other machine has room for, when it can launch the space's
agent. It writes the placement back.

When no machine can launch the space's agent, the hub writes back that it
keeps the space. A space never runs another agent tool than its own, and Shoal
installs none. The hub places the space again as each machine reports its
capacity, and continues it on the first machine it chooses. When the machine
the space came from registers a link again before the hub chose one, the space
stays there, without a restore, and the user hears of it. A choice made first
goes ahead, as any hand-over does, and that machine retires its copy. The hub keeps such a space in memory alone,
so after the hub's restart `shoal restore` takes it over.

The shutting-down machine reads the placement and says it leaves the space
there. Only then does the chosen machine import the package and queue its
agents' restores. So a machine that gave up waiting keeps its space. One whose
word went unanswered keeps no restore, since the space may have gone. Nothing
waits for an import.

A chat in a workspace's worktree follows the workspace to the same machine. It
is released and continued after the workspace, since it opens only in that
worktree.

The chosen machine's claim stands though the machine the space came from may
still be connected. That machine forgets the space's restores, so the space
does not come back at boot. It retires its copy once its link opens, as after
any takeover. Work a reboot moved stays where it went.

A space whose package did not arrive keeps its place and restores at boot as
before.

### Which stops hand over

The daemon cannot tell a shutdown's signal from a logout's or a service
stop's. macOS announces no shutdown at all. So on every platform, every signal
Shoal did not send itself hands work over, as its agents stop anyway. Shoal's
own restart, stop and upgrades go through its control path and hand nothing
over.

A cancelled shutdown tells the hub that the machine stays. So the machine
retires its copies of what went at once, rather than at its next link.

## Sleep announcement

An announced sleep is no silence either. Every machine joined to a hub
announces its sleep, whatever its availability. On macOS it acts on IOKit's
will-sleep notification, whose acknowledgement it delays. On Linux it acts on
logind's `PrepareForSleep`, under a sleep delay lock.

The machine first sends one small notice, naming each space whose package lags
and since when. So the hub learns of the sleep however little else fits. It
then sends the changed packages, one stream per space, all at once, until the
platform's allowance runs out. An `always_on` machine keeps no packages, so it
sends every space with work instead.

The machine then keeps its link closed and admits no queued work until it
fully wakes. So a dark wake neither cancels the wait nor races a takeover.

A machine may lose power before it can announce. So a link that drops while
the last report said the machine ran on battery below 5% counts as a sleep
announced at that report. That report carries whether the machine runs on
battery, and its level.

### Takeover after a sleep

Once a sleeping machine has stayed away 10 minutes, the hub moves each of its
spaces that
[has something to do](../reference/hub.md#takeover-after-a-sleep). An update
the hub held for a space while the machine sleeps takes the space over as it
arrives. The hub moves only a space whose package is current, or lagged less
than 15 minutes before the notice. It moves it as a shutdown's hand-over does,
and holds what it held for the space for the machine it went to. A chat in a
workspace's worktree goes only with that workspace.

A space no machine can take waits on the sleeping machine, and the hub tries
again as each machine reports its capacity. A space that lags more stays, for
the alert and a manual restore. The machine
retires the copies taken over once it wakes. One that wakes within the 10
minutes keeps everything.

The hub keeps a sleep in its store. These numbers are fixed, not settings.

## Walking away

`[machine] keep_awake` decides what a machine does once its user walks away,
as [machine](../reference/machine.md#walking-away) says. Under `handover`, the
machine starts its hand-over at the walk-away or the sleep's announcement,
whichever comes first. It holds the machine awake until the hub confirms the
packages, for at most 10 minutes. It retires its copies once the user is back
or the machine wakes. A shutdown meanwhile still hands over every space that
did not go.

A machine that admits nothing new says so in its capacity, which placement
skips. The daemon reads the lid and idle state every few seconds, and only
under `finish` and `handover`, the values that act on it.

## Pauses

`shoal pause` frees a machine on demand, whatever its `keep_awake`, the hub's
own machine included. [Pausing a machine](../reference/machine.md#pausing-a-machine)
says what it stops. Its spaces with something to do hand over as a
walk-away's do.

The store keeps the pause until `shoal unpause`, so it holds through restarts.
Only the user pauses, since a pause acts on every space of a machine.
`--machine` reaches another machine through the hub.

The hub's own machine keeps running its daemon while paused. So it builds each
package itself and keeps it as the hub, placing the space among the other
machines. Unpausing retires the copies that went there too.

## Placement

A space starts on the caller's machine, or on the joined machine
`shoal start --machine` names. A start that names none goes where the hub's
placement policy puts it, unless it needs the caller's machine. The hub
decides once, where the worktree goes. Each machine's own [queue](queue.md)
and admission then decide when the agent starts. Placement never moves a space
after it is created. Rebalancing running work is a [move](moves.md).

A start whose agent would run in the caller's terminal is placed too, since
the terminal attaches to an agent on any machine of the hub. The machine it
goes to starts the agent detached and at once, and the caller attaches to it
as `shoal attach` does. Two such agents need the caller's machine, which the
launch records apart from whether its agent runs in the caller: one that runs
its own way, outside Shoal's terminal hosts, which nothing attaches to from
another machine, and one a caller without a terminal waits for. Those starts
stay, and so does one whose agent a space's process runs, since
`shoal attach` refuses that process.

Placement is a policy in `shoal-schedule`, beside the queue's scheduler, under
the same rule. It reads only a snapshot of the start and of each connected
machine, never the hub's or a daemon's state. It ranks the machines with a
reason each, so a start and an app can say why a space went where it did.
The hub's log records every placement, under every policy: the machine it
chose and why, and each machine it skipped and why.

### Capacity reports

Each machine reports its capacity with its spaces, as
[placement](../reference/hub.md#placement) lists it. The daemon measures it at most
every 15 seconds, and at once as a pause starts or ends. It sends a report
when the capacity changes enough to place by. The hub places from the latest
report, never a live query, and keeps it in memory alone.

The report lists the agents the machine can launch in each of its
repositories, under that repository's settings as a launch there resolves
them, and those it can launch outside a repository. The machine works them
out at each measurement, so an agent tool installed on the daemon's `PATH`
shows in the next report. The hub matches agents by name alone: the machine
that runs a start launches that name under its own definition.

A connected machine that has not reported yet is `undescribed`. Placement
skips it as such, never as lacking the repository or the agent. A placement
that meets one first waits 2 seconds for its report, as such a machine sends
one soon after its link opens, then places without it.

A machine also reports how long its runs take: per repository and run key,
the median duration of its whole runs over the last 7 days. It leaves out
runs the OOM killer hit, which ran short of memory rather than at the
machine's speed. It sends the medians with a report when they changed.

The hub derives each machine's speed per repository from these medians. It
compares two machines only on the keys both ran, since machines run
different jobs and commands. A machine's speed is the mean of its log ratios
against each machine it shares keys with, so it stays relative to the
machines that ran the same work. A machine that shares no key with another
has no speed there. The hub compares only the connected machines, and leaves
out medians under 10 seconds, whose startup outweighs their work.

### Policies

`[machine.hub] placement` picks the policy, as the
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
says.

`balanced` places by the task's profile. Its lifetime is `lasting` or
`disposable`: a `--disposable` start is disposable, and any other start is
lasting. Its weight is heavy or light, by its mode. Its requirements are its
repository, by origin, and its agent tool. A hand-over, takeover or pause
places a space by its own profile.

`balanced` ranks the machines by stages, each a function that yields a key
and a reason. A later stage only orders the machines an earlier one ranks
equal, so adding a preference adds a stage in its place. Each machine's reason
names the first stage that prefers it, so a start can say why it went there.
The stages, in order:

1. The hard rules. The machine must fit the start: not paused or away, with
   its repository and agent tool. Lasting work goes only to an `always_on`
   machine with room, since its user comes back to it. Disposable work goes to
   any connected machine with room, since nothing keeps it. Otherwise the
   start stays on the caller's machine.
2. Disposable work prefers an `intermittent` machine. Only `always_on`
   machines take lasting work, so they keep their room for it.
3. The caller's machine comes first, beside the start's parent and unpushed
   work. It follows the lifetime stage, so disposable work leaves an
   `always_on` caller for an `intermittent` machine.
4. A heavy start prefers the machine fastest at its repository, and a light
   start the slowest, since a light agent mostly waits on its model and
   keeps the fast machines free. A machine without a speed there ranks as
   an average one, not a slow one. Speeds within a factor of 1.25 of each
   other rank equal, so the noise of medians leaves the choice to room.
5. A heavy start goes where a heavy agent is admitted now, with the most
   spare memory. A light start goes where it takes the least from heavy
   agents, since dynamic admission gives them the same memory: first to a
   machine that runs no heavy agents, then to one where no heavy work waits,
   by spare memory, then to the one where the least heavy work waits.

`balanced` never clones a repository onto a machine; that stays with
`--machine`.

Two more stages are proposed. Before the lifetime stage, a machine whose
exclusive work waits now ranks after those where none waits. Before speed, a
machine scarce in recent exclusive demand ranks later.

`external` lets an app on the hub's machine take the decision over, registered
as the placer. A start waits for it only briefly, and otherwise places by
`balanced`, so a slow or absent app never holds starts up.

## The crate's boundary

The `Hub` service keeps the links, pairing codes, vouches and outbox. It reads
the spaces, permissions, messages and approvals it routes across machines
through the crate's interface. Request dispatch, which answers what the hub
forwards to this machine, stays with the daemon.
