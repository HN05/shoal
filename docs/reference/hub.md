# Hub

This file says what the `hub` commands do and how machines joined to a hub work
together.

```sh
shoal hub pair                     # On the hub or a joined machine: a code and its QR code
shoal hub join <code> [--name N]   # Join this machine with that code
shoal hub join --connect "ssh desk shoal daemon connect"   # Or through a command, without a code
shoal hub ls                       # The listener, and every machine and device
shoal hub revoke <machine or device>
shoal hub update                   # The latest release on the hub, from any machine
shoal hub leave
```

One daemon of yours takes the hub role, with
[`[machine.hub] enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub),
and your other machines join it; the hub stays the machine of its own spaces,
and one without repositories is a hub alone. Each machine that joined keeps one
link to the hub and opens it again with backoff after sleep or a network
change; the hub never connects to a machine. `shoal status` shows the role the
daemon takes: the hub's listener, or the machine's membership with whether its
link is open and why not.

The hub's listener serves HTTP, and the link over a WebSocket at `/hub`, on
`[machine.hub] listen`, loopback by default. Shoal handles no TLS, so
`shoal status` and `shoal hub ls` show the listener as unencrypted: put a
tunnel or reverse proxy in front of it, such as Tailscale Serve, cloudflared or
Caddy, and set `[machine.hub] url` to the address machines reach it at there.
The hub and every machine hold a key pair of their own, which pairing
exchanges. Each link opens with both sides signing what the other sent,
including a key exchange whose keys then encrypt and authenticate every frame,
and a joiner proves its pairing code without sending it, so nothing between
them, a proxy included, can pose as either, use a code it saw, or read,
change, add or replay a frame.

`shoal hub pair` runs on the hub, or on a machine that joined it, and prints a
code with its QR code: the hub's URL, its key's fingerprint and a secret that
admits one join within ten minutes. It warns when the URL is `http://` to
another machine. `shoal hub join <code>` joins this machine under the host's
name, or `--name`; an app joins as a device with the same code.

On a computer without Shoal, run the other line `hub pair` prints, `curl -fsSL
<hub url>/install | sh -s -- '<code>'`: the hub's listener serves that script,
which downloads exactly the hub's version for the platform from its GitHub
release, checks it against the release's `SHA256SUMS` and its reported
version, installs it as a downloaded binary at `~/.local/bin/shoal`, or on
macOS as `~/Applications/Shoal.app` with that path linked to it, then runs
`shoal daemon install` and `shoal hub join` with the code, which pins the
hub's key. It installs nothing where `shoal` is already installed at either
path: run `shoal hub join` there instead. It installs neither the [runtime
dependencies](../../README.md#runtime-dependencies) nor agent tools and their
logins; repositories are cloned once a start there needs them.

The hub gives
each machine or device an ID at pairing, and `hub ls` lists them with kind,
name, Shoal version, when each was last seen, each machine's availability
and, for a connected machine, the agent tools it has installed (`agents` in
`--json`) and what it last reported for placement (`capacity`), by which `hub revoke` names one. `hub leave` revokes this machine's own credential, or names what to revoke
on the hub when it cannot reach it. Only your own terminal pairs, joins, leaves
and revokes; anyone lists.

A setting whose `{ cmd }` names a machine with
[`on`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#files-and-layers)
takes its value from that machine, so a token or key set up once, such as in
its keychain, serves every machine. The daemon that needs the value sends the
command over its link, the hub passes it on, and the named machine runs it as
the user for at most a minute and sends the output back; `on` naming this
machine runs it here. Only a machine's daemon asks: a device cannot, and a
process of a space asks its own daemon for the setting's value, which runs only
the command its config gives. The value is fetched once per operation, so a
rotated one applies at once, and the machine that asked keeps the last one in
memory, never on disk, using it while the named machine cannot be reached;
without one, reading the setting fails, naming the machine and the setting. The
hub records which settings each machine fetched from which, and when, never the
values: `shoal hub revoke` stops a machine from asking again and lists the
settings it fetched, to rotate, since a value it received cannot be taken back. Agent tools' own logins, such as Claude Code's and Codex's, are files
those tools own and are not shared: their rotating refresh tokens would log out
the other copy.

Each machine is `always_on` or `intermittent`, which `hub ls` shows with the
reason and `shoal status` shows for this machine.
[`[machine] availability`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
sets it (`set`); under `auto`, the default, the hub guesses from how reachable
the machine was, never from what powers it. A machine whose link was open less
than 95% of the last 7 days is `intermittent`, and any other `always_on`
(`connected N% of 7 days`); an absence shorter than 5 minutes, such as a
network blip or a daemon restart, counts as connected, and the hub's own
machine counts as connected while its daemon runs with the machine awake. For
the first day the hub knows a machine, it goes by the battery instead: a
machine with one is `intermittent` (`new, battery`) and one without
`always_on` (`new, no battery`). The hub tells each machine its availability
when its link opens and whenever it changes.

An `intermittent` machine keeps the latest package of each of its workspaces
and chats on the hub, so their work outlives the machine going away: what
`shoal move --keep -o` writes, a new checkpoint of a workspace with a bundle
of its commits, its agents' session files and its records, but under the
space's own ID, built without stopping its agents. The machine looks at a
space when its agent's turn ends, and at every space every 10 minutes, and
sends each whose HEAD, branch, files, agents' session files or records, such
as its links, threads and queued work, changed since its last package, at
most once every 5 minutes; a space that did not change sends nothing. The
hub keeps one package per space in its state directory, replacing it only
once the new one arrived whole, and deletes it once the space ends on its
machine, as by removal or a move, or the machine is revoked. While the disk
of its state directory has less free space than
[`[machine.overload.warning] disk_free_gib`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#overload),
it refuses new packages and records a `hub_package_refused` alert. The hub's
own machine sends none.

Every machine of a hub, the hub's own included, shares the
[shared config](../../crates/shoal-cli/src/cli/commands/configuration/reference.md):
`shared.toml` and the mode and rule files in `shared/` beside it. A machine
sends the hub each of them it edited within ten seconds, or at once on
`shoal config reload` and `shoal config set --shared`, once they load; an
invalid file stays on the machine, and an alert says why. The hub keeps
whichever edit reached it last, a removal included, and every other machine
writes it and applies it. An edit made while a machine is offline goes once
its link opens; a joining machine takes the hub's copy of each file it has
and sends those the hub lacks. When a copy replaces an edit the hub never
got, the machine keeps that edit as `<file>.backup`, such as
`shared.toml.backup`, and alerts you naming it.

`shoal repo ls` lists the repositories every other joined machine registers
after this machine's, each naming its machine, which `--json` gives as
`machine` and `machine_id`. They also share the repositories you mark shared: `shoal repo add <url>
--shared`, or `shoal repo share [repo]` for a registered one, which needs an
origin, and `shoal repo ls` shows them. Every other machine registers each
one as `shoal repo add <url>` would, matched by origin, cloning it at once in
the background under its own `root_dir` and under the same name where no
repository there has it; a repository it already has, whatever its name,
keeps that registration and becomes shared; a `shoal contribute` clone that
becomes shared is no longer removed on its own. A clone that fails records a
`shared_repository_failed` alert naming `shoal repo add <url> --shared`,
which registers it once you fix the cause; otherwise it is tried again when
the machine next connects to the hub, or for the hub's own machine, when its
daemon restarts. `shoal repo unshare [repo]`, or removing a shared repository, stops
sharing it on every machine, which keeps its registration. Machines clone
from the origin, never through the hub. A repository that is not shared stays
on its machine, and `shoal start --machine` clones it there on demand.

A shared repository's local config, `.shoal/config.toml` in its repository
directory, is shared by the shared config's rules: an edit reaches every other
machine within ten seconds, or at once on `shoal config set --repo`, once it
parses; an invalid file stays on its machine, and an alert says why. A
repository that becomes shared on a machine, cloned or registered there
before, takes the hub's copy, and a replaced edit stays as
`config.toml.backup`, with an alert naming it. Once a repository is no longer
shared, each machine keeps its local config as it is.

A machine can also reach the hub through a command, whose standard input and
output its daemon uses as the link, restarting it with backoff and logging its
standard error: `shoal daemon connect` on the hub relays them to its daemon's
socket, byte for byte, and fails when no daemon runs there, never starting
one. Running commands on the hub as you proves more than a code, so `shoal hub
join --connect` joins without one: it checks that the command answers, names
an absolute path to `shoal` when the other side does not find it, and keeps
the command as the machine's route to the hub. `[machine.hub] connect`
overrides the route a machine joined by. Each join makes the machine a key
pair of its own, so one that left or was revoked joins again as a new
member.

The hub refuses a link that presents a revoked credential, which stops the
machine from trying again until it joins again; one that presents a
credential another machine's link holds, as a copy of a state directory does,
which is told to join as a machine of its own; and one that runs another
Shoal version, recording an alert that names the machine, its version and the
update to run. A refused machine keeps working on its own.

The hub updates as it was installed: with `brew upgrade hn05/tap/shoal`
there, or for a downloaded binary with `shoal update` there or `shoal hub
update` on any of its machines, which the `daemon` permission governs. A managed
daemon restarts on the new release once idle; `shoal hub update` says when a
foreground one needs a restart.

A machine follows the hub's version. Once the hub refuses it for running
another one, a Shoal downloaded from a release installs exactly the hub's
release, as `shoal update --version` does, older or newer, after the hub's key
signed which one it is; a managed daemon restarts on it once idle, with its
agents running through the restart, and connects again, while a foreground one
tells you to restart it. It never installs a release too old to read the state
its newer one migrated, and tells you to update the hub instead, there or with
`shoal hub update` from a machine connected to it; nor does `shoal hub join`
suggest one.
[`[machine.hub] auto_update = false`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
turns that off. A Homebrew installation never updates on its own: its machine
tells you to run `brew upgrade hn05/tap/shoal` there, or to update the hub
when it runs an older release, which Homebrew never installs. A machine that does not
follow, or fails to, stays off the hub and tells you what to run, and so does
the hub's alert about it, which for a machine that follows comes only once
the hub has refused it for 30 minutes.

Each machine keeps its spaces and reports a summary of each to the hub as it
changes, all of them again whenever its link opens: its number and name, its
kind, its repository by origin, the space above it, which may run on another
machine, its mode, what lists show it doing, whether it has an agent and
the name of the one that runs there, its progress, its open threads and
approval requests, and on an `intermittent` machine since when it has changes
the hub's package of it lacks (`unsent_since`), none while the package is
current. `shoal list`, `shoal inbox` and
`shoal status <space>` on any machine of the hub show the other machines'
spaces as their machines last reported them, and once they do, every row names
its machine; a space a scoped caller's scope reaches, through the spaces above
it on any machine, is one it sees. A machine the hub has not heard from since
keeps its spaces listed, with when the hub last saw it. `shoal status` of
another machine's space, by its ID or a number no space here has, shows the
status its machine answers, naming the machine, which keeps the space's
sessions and transcripts; while that machine is offline, it shows what the
machine last reported. The `--repo` filter
matches another machine's spaces by their repository's origin. A hub that
cannot be reached leaves the other machines out with a warning.

The hub's machines number their spaces from one sequence, so a number names
the same space on every one of them. The hub keeps a few spare numbers for
each machine, so creating a space never waits for the hub; a machine that
runs out of spares while it cannot reach the hub numbers above its own
highest. A space whose number another machine's space or spares held
first, as after that or when a machine joins with spaces of its own, takes
the smallest number free on the hub, its threads and approval requests
follow it as `S.N`, and its machine tells you and the space's agent the old
and new numbers in a `space_renumbered` alert; the space that held the
number first keeps it. A space `shoal move --machine` or `--ssh` moves
between the hub's machines keeps its number; one `shoal move -o` wrote, imported after its
source is gone, takes a new one.

The hub lists a space ID under one machine, the first that reported it. A
second machine reporting it, as after copying a state directory, is left out
and raises an alert naming the command that resolves it there: `shoal rm` of
its copy, or `shoal start --copy` to keep it as a new space first; once the
first machine's space ends, the copy takes its place. An import, as of
`shoal move`, claims the ID first, so the machine it moves to takes the ID over
and the one it moves from keeps a copy without a conflict until it ends the
space there; that copy never takes the ID back, so removing and restoring the
space on the machine it moved to finds no other machine running it.
`shoal restore` of a space another machine runs is refused, naming
`shoal move` and a copy, while that machine is connected or the hub keeps no
package of the space; while the hub cannot be asked, the restore goes on,
and a duplicate shows as a conflict once both machines report it.

`shoal restore <space>` of a space whose `intermittent` machine is offline,
by its number or ID, takes the space over from the package the hub keeps of
it, for an agent as its `admin` permission allows, as `restore --from` is: it fetches the package and imports it under the space's ID as
`shoal restore --from` does, so its claim makes the space this machine's,
and queues its agents' restores, which continue their sessions. It warns
when the machine had changes the package lacks, naming how old the oldest
is. When that machine's link opens again, the hub names the spaces taken
over meanwhile, and the machine stops their agents and removes each through
the removal path, keeping a checkpoint, its agents' sessions in its removal
record and its branch, and leaving the spaces below it, whose parent now
runs on the other machine. A copy that changed after the hub's package also
raises a `space_diverged` alert naming what opens that work: for a
workspace, `shoal start --copy <id> --checkpoint <number>` with the
checkpoint its removal kept, and for a chat its agents' sessions. A copy the
machine cannot remove raises a `space_conflict` alert naming the error and
`shoal rm <id>`, and the hub asks it again the next time it connects.

A machine joined to a hub hands its spaces over as it shuts down, whether
it is `always_on` or `intermittent`, and under
[`keep_awake = "handover"`](machine.md#handing-work-over) those with something
to do once its user walks away or [pauses it](machine.md#pausing-a-machine); a machine
in no hub keeps its spaces, and so does the hub's own until it is paused. Once the stop before the shutdown saved its agents, it
sends the hub the final package of every workspace whose worktree is ready,
but one a CI job runs in, and every chat, all at once, until a second
before logind's `InhibitDelayMaxSec` ends, or for a signal for 15 seconds,
which the service definition's stop timeout adds to the daemon's own. For
each package that arrives whole, the hub chooses a machine by its
[`[machine.hub] placement`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
policy among the other
connected machines, its own taking the place of the caller's, from the
space's repository, its agent's tool and its mode. Once the shutting-down
machine has read that choice and tells the hub it leaves the space there,
that machine imports the package under the space's ID, as `shoal restore`
of a kept package does, and queues its agents' restores, which continue
their sessions. A chat in a workspace's worktree goes where that workspace
went, once it has continued there, and stays when the workspace stays. The `system_shutdown`, `daemon_stopped` or `handed_over` alert names, for
each space, the machine it went to, or that it stays and why: its package
did not reach the hub in time, no machine can take it, or the hub cannot
be reached. A space whose leaving the hub did not acknowledge in time may
have gone, which the alert says; it keeps no restore here either. A space
that stays restores here at boot as before. One that went keeps no restore
here, and once this machine's link opens again it retires its copy as after
any takeover; work a reboot moved does not come back on its own. A space
the chosen machine fails to continue stays here too, and the hub raises a
`handover_failed` alert naming `shoal resume <id>` here and
`shoal restore <id>`, which takes it over from the hub's package. Every
SIGTERM or SIGINT the daemon receives counts as a shutdown, on any
platform, so a logout that ends the daemon, a service manager's stop and
Ctrl-C of a foreground daemon hand work over too; `shoal daemon restart`,
`shoal daemon stop` and upgrades hand nothing over. When a shutdown is
cancelled after the hand-over, the machine has the hub retire its copies
of the spaces that went, as it does once its link opens again.

A machine joined to a hub announces its sleep, whether it is `always_on` or
`intermittent`: on macOS from IOKit's will-sleep notification, on Linux from
logind's `PrepareForSleep` under a sleep delay lock. It first sends the hub
one notice naming each space whose package lacks changes and since when,
then a new package of each space that changed, or on an `always_on` machine,
which keeps no packages current, of each space with an agent working or
queued, all at once, until a second before the platform stops waiting:
logind's `InhibitDelayMaxSec`, or 30 seconds on macOS. Then it closes its
link and admits no queued agent until the machine fully wakes, so a dark
wake, such as Power Nap's, changes nothing. A machine whose link drops while
its last report said it ran on battery below 5% counts as one that announced
its sleep at that report.

Once a machine that announced its sleep has been away for 10 minutes, the
hub takes over each of its spaces that has something to do (an agent
working at its last report, a queued agent, or an event or request the hub
holds for it since the notice, which takes it over as it arrives) and whose package is
current or lacks only changes from less than 15 minutes before the notice.
It places each as a shutdown's hand-over does, and the machine it chose
continues it under its ID and queues its agents' restores; a chat in a
workspace's worktree goes with that workspace, and only together. A
`spaces_taken_over` alert names where they went, and the events the hub
held for them follow them there. A space whose package lags more stays, and
the `machine_unreachable` alert below names it. The machine retires its
copies of the spaces taken over once it wakes, as after any takeover, while
waking within the 10 minutes leaves everything where it was. These times
and the battery level are fixed, not settings.

The hub never takes a space over on its own otherwise: a machine that is
awake but lost only the hub keeps working. Once an `intermittent` machine, or
any that announced its sleep, has been unreachable for 30 minutes, the hub
records one `machine_unreachable` alert
for that drop, naming its spaces that had something to do (an agent working
when its link dropped, a queued agent, or an event the hub holds for one
since), how far behind the hub's package of each is, and the
`shoal restore` that takes it over. A revoked
machine's spaces leave the hub's lists.

A command for a space another machine of the hub owns works from any of
them: the daemon that takes it forwards it through the hub to the space's
machine, which decides it as it decides its own callers'. Your terminal's
commands go as yours; a space's processes' go with that space's scope and
permissions, which its machine vouches for, so an agent acts on the spaces
below its own on every machine and is refused the rest, and a device's go as
yours. An action a space's permission sets to `propose` is refused from another
machine, since its approval request would belong to that space's machine. This covers every command
that reads or changes a space's state, and following an agent's session and
reading an attached file, whose bytes come back as its machine reads them. An
attached file stays on the machine that keeps its copy: a record of it on
any other machine, in `shoal files`, an alert or a thread, names that machine
where it would give the copy's path. The link to the hub carries such answers as byte streams beside
its other traffic, each held to what its reader has taken, so a reader that
falls behind holds up nothing else. A machine that received a request but
did not answer it in time fails the command, saying that the request may go
on there, which `shoal status <space>` shows. Commands that need the
machine they run on, such as acquiring and releasing leases,
starting a demo and an agent's hooks, are refused for a space elsewhere,
naming its machine, as is a command that attaches files, which that
machine cannot read, and `resume --focus` and `--execution`. `shoal stop`,
`shoal resume`, `shoal restart` and `shoal restore --discard-agent` take
`--machine <name or ID>`, repeatable, which selects the spaces of that
machine that `--all` would select there, by what it last reported: those
with something to stop, those whose saved work an unfinished assignment
resumes, those with an agent running and those with something to discard.
The reports name the machine of each space elsewhere, as `--json` does in
`machine`. The picker of `shoal resume` offers the other machines' spaces
with something to resume. Without a target, a
command's picker offers the other machines' spaces that the request it
sends for the picked space reaches, after this machine's, as each
machine's daemon judges its own for that request, every space of one that
is offline, each naming its machine, and an offline one with when the hub
last saw it; choosing one acts as naming it does. A command that works in the
space's worktree, terminal or files here, as `cd`, `exec` and
`stop --interrupt` do, offers only this machine's.

While a space's machine is offline, a message, an answer, an approval
decision or a stop for it waits at the hub, and the command says so, naming
the machine, when the hub last saw it and when the request expires; the hub
delivers what waits, in order, once the machine connects. A request still
waiting after seven days is dropped, and the space whose process sent it, or
else you, hears of it in a `hub_expired` alert, as of one the machine received
but whose link closed before it confirmed it ran, which the hub never sends
twice. A `stop --machine` reports one line per offline machine, with how
many stops wait for it. Any other command for an
offline machine's space, following a session included, fails at once, naming
the machine and when the hub last saw it, and `shoal restore <id>`, which
takes the space over here, where the hub keeps a package of it;
under `--machine`, `resume` and `restore --discard-agent` skip an offline
machine's spaces, naming it, and go on with the others. `shoal restore` of a
removed space whose record another machine keeps names that machine, and
`shoal move <id> -o FILE` with `shoal restore --from FILE`, which open it
here.

`shoal start --machine <name or ID>` starts a space on another joined machine
of the hub, a workspace or a chat, below the space `--parent` names, else
below the caller's own, else at the top level. Your machine's daemon
forwards the start through the hub, and that machine's daemon starts it as
its own `shoal start` does, so the space's queue, limits, cleanup, ports and
leases are that machine's, and its queue starts the agent in the background
there with that machine's agent tools and modes: `--mode` names one of that
machine's, which decides whether it starts a workspace or a chat, and a
terminal's picker lists them. That machine finds the repository by
its origin, cloning it as `shoal repo add <url>` does where it has none; a
start on any machine below another machine's space, in a repository the
starting machine lacks, clones it from the origin URL that space's machine
reports to the hub. A
`--base` goes as the origin's branch of that name, or as a commit one of the
origin's branches holds; a base with commits only your machine has, such as
a stacked branch you have not pushed, goes along as a Git bundle of what the
origin's default branch lacks, so nothing needs a push first.
`--wait` follows the workspace's preparation there, and `--here` and
`--focus` start the agent there at once and attach your terminal to it once
the workspace is prepared, as `shoal attach` does, or leave it running there
without a terminal. `--fork` carries the forked session's files there first,
where that machine's agent tool looks for them under the repository's
settings, so its agent starts from them. `--copy` and `--checkpoint` carry the
copied files there, ignored ones excepted, as a bundle of the copied HEAD and
of a commit that keeps them, and the copy's new branch there starts at that
HEAD with them unsaved. `--in`, which names a workspace of your machine, does
not combine with `--machine`. A
start from a device, such as an app, goes to the machine its launch names,
else to the hub's
[`[machine.hub] default_machine`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub),
else to the machine the device sent it to.

A start without `--machine` whose agent runs detached, such as a background
chat, a child an agent starts or a trigger's launch, goes where the hub's
[`[machine.hub] placement`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
policy puts it, and continues there as a start with `--machine` does; one
that runs in your terminal, or needs your machine, as `--in`, `--copy`,
`--fork`, `--path`, `--add-blocker`, a workspace of yours it reopens on its
branch or issue, and a repository your machine lacks or that has no origin
do, stays on your machine. The hub places it from what each connected
machine last reported with its spaces: the origins of its repositories, its
`max_heavy_agents`, the heavy agents its admission would start now, the
memory new work may use, the heavy agents waiting in its queue and whether
its user walked away while its [`keep_awake`](machine.md#walking-away)
admits nothing new then, or [paused it](machine.md#pausing-a-machine), which
`hub ls --json` shows as `capacity`. A start that finds the machine full
meanwhile waits in its queue there; placement never moves a space once it
is created. `shoal start` names the machine with the policy's reason, and
each connected machine it skipped for lacking the repository or the agent
tool or for a user who walked away or paused it, which `--json` gives as `placement` in a chat's start and as a last
document after a workspace's; a hub that cannot be asked leaves the start on
your machine, as does a machine the start never reached, such as one that went
offline since its last report, with a warning. Under `placement = "external"`, an
unscoped client on the hub's machine, such as an app's server, registers as
the placer with `shoal-client`'s `hub::Placing` and answers each snapshot the
hub sends it, the start and every connected machine, with the machines in
order of preference; the latest registration takes over from the one before.
A start the placer does not answer within two seconds, or answers with a
machine that is not connected or lacks the start's repository or agent tool,
since placement never clones a repository, and one without a placer, is
placed by `balanced`, which the start names.

The tree of spaces may span machines: a space that moves to another machine,
or that `shoal start --machine` starts there, without the space above it stays below it there, once what the hub lists of
that space takes it as a parent here would, and its permissions stay no wider
than that space's, as its machine last reported them. Removing a space
removes the spaces below it on other machines first, through their own
machines; one that cannot go, or a removal that confirmed only the spaces it
saw here, keeps it. A space on an offline machine cannot go: for a removal
you or an agent asked for, the hub holds that space's removal until its
machine connects, and the space above stays until it is removed again. What a space passes up the
tree, its agent's alerts, threads, approval requests, completion, ready
marks and exits, and the issue and PR activity of a space without an agent,
reaches the nearest space above with an agent wherever it runs, and that
space decides the space's approval requests, answers the threads it holds and
sends it an alert from its own machine. A machine whose link to the hub is down
keeps what its spaces pass up and sends it once the link opens. A thread that
waits for a holder, or an approval request for a coordinator, that waited
[`[machine.hub] escalate_after_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
for a coordinator whose machine is offline passes to the next space above
with an agent on a connected machine, or to you.

## Path forwarding

[`[machine.hub] forward`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
lets one address and tunnel serve the hub and an app's own server, such as
Quay's, which runs as its own process, a paired device using `shoal-client`.
The hub's listener forwards a request under one of its path prefixes, the
longest that covers it, to that prefix's local address with its path
unchanged, upgrades such as a WebSocket's included, once a paired machine or
device signed it: the request names the member in `shoal-member`, the time
it was signed in `shoal-time`, within five minutes of the hub's clock, a
nonce no earlier request used in `shoal-nonce`, and in `shoal-signature`
the member's signature of what `shoal_protocol::hub::app_servers::signed`
makes of them, the method and the path with its query. The server receives
`shoal-member`, which the hub checked, without the other three. The
signature covers no body. Any other request under a prefix is refused with
`401` and never reaches the server, and a request under no prefix is `404`.
The hub keeps each nonce it took in its store until requests signed when it
was are no longer taken, so a restart lets none through twice.
