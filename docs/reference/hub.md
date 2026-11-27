# Hub

This file says what the `hub` commands do and how machines joined to a hub work
together. [Hub](../design/hub.md) gives the reasons.

```sh
shoal hub pair                     # On the hub or a joined machine: a code to join with
shoal hub join <code> [--name N]   # Join this machine with that code
shoal hub join --connect "ssh desk shoal daemon connect"   # Or through a command, without a code
shoal hub ls                       # The listener, and every machine and device
shoal hub revoke <machine or device>
shoal hub update                   # The latest release on the hub, from any machine
shoal hub leave
```

## The hub and its members

One of your daemons takes the hub role, which
[`[machine.hub] enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
turns on. Your other machines and devices join it. The hub stays the machine
of its own spaces. A hub without repositories serves only as the hub.

Each joined machine keeps one link to the hub. It opens the link again with
backoff after a sleep or a network change. The hub never connects to a
machine.

`shoal status` shows the role the daemon takes. On the hub, it shows the
listener. On a joined machine, it shows the membership, whether the link is
open, and why not.

## Listener and encryption

The hub's listener serves HTTP on `[machine.hub] listen`, on loopback by
default. It serves the link as a WebSocket at `/hub`. Shoal handles no TLS, so
`shoal status` and `shoal hub ls` show the listener as unencrypted. Put a
tunnel or reverse proxy in front of it, as the
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
describes. Set `[machine.hub] url` to the address machines reach the hub at
there.

The hub and every member hold a key pair of their own, which pairing
exchanges. Each link opens with both sides signing what the other sent. That
includes a key exchange, whose keys then encrypt and authenticate every frame.
A joiner proves its pairing code without sending it. So nothing between the
two sides, a proxy included, can pose as either side or use a code it saw. Nor
can it read, change, add or replay a frame.

## Pair and join

`shoal hub pair` runs on the hub or on a joined machine. It prints a code,
as the command to run on the machine that joins:

```sh
shoal hub join '<hub url>/pair#code=<secret>&key=<fingerprint>'
```

The code is that whole URL, quoted for the shell. It holds the hub's URL, its
key's fingerprint and a secret.
The secret admits one join within ten minutes. `hub pair` warns when the URL
is `http://` to another machine.

`shoal hub join <code>` joins this machine under its host's name, or the name
`--name` gives. An app joins as a device with the same code.

Each join gives the machine a key pair of its own. A machine that left or was
revoked joins again as a new member.

Only your own terminal pairs, joins, leaves and revokes.

## Install Shoal from the hub

`hub pair` also prints a line to run on a computer without Shoal:

```sh
curl -fsSL <hub url>/install | sh -s -- '<code>'
```

The hub's listener serves that script. The script downloads exactly the hub's
version for the platform from its GitHub release. It checks the download as
[`shoal update`](install.md#update-a-downloaded-binary) does. It installs it as
a downloaded binary at `~/.local/bin/shoal`. On macOS, it installs
`~/Applications/Shoal.app` instead and links that path to it. It then runs
`shoal daemon install`, and `shoal hub join` with the code, which pins the
hub's key.

The script installs nothing where `shoal` is already installed at either path.
Run `shoal hub join` there instead. It installs neither the
[runtime dependencies](../../README.md#runtime-dependencies) nor agent tools
and their logins. A machine clones a repository once a start there needs it.

## Join through a command

A machine can reach the hub through a command instead of a URL. Its daemon
uses the command's standard input and output as the link. It restarts the
command with backoff and logs its standard error.

The command ends in `shoal daemon connect` on the hub, such as
`ssh desk shoal daemon connect`. `shoal daemon connect` relays its standard
input and output to the daemon's socket there, byte for byte. It fails when no
daemon runs there, and never starts one.

`shoal hub join --connect "<command>"` joins without a code, since running
commands on the hub as you proves more than a code does. It checks that the
command answers. When the command fails, it suggests naming `shoal` by its
absolute path, for a remote side whose `PATH` lacks it. The machine keeps the
command as its route to the hub.
[`[machine.hub] connect`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
overrides the route a machine joined by.

## List, revoke and leave

The hub gives each machine and device an ID at pairing. `shoal hub ls` lists
them with their kind, name, Shoal version and when each was last seen. For
each machine, it shows its [availability](#availability). For a connected
machine, it also shows what it last reported for [placement](#placement),
with the agents it can launch, `capacity` in `--json`. Anyone lists the
members.

`shoal hub revoke` revokes a member by the ID or name `hub ls` shows.
`shoal hub leave` revokes this machine's own credential. When it cannot reach
the hub, it names what to revoke on the hub instead.

## Refused links

The hub refuses a link that presents:

- a revoked credential. The machine stops trying until it joins again.
- a credential another machine's link holds, as a copy of a state directory
  does. The machine is told to join as a machine of its own.
- another Shoal version. The hub records an alert that names the machine, its
  version and the update to run.

A refused machine keeps working on its own.

## Update the hub

The hub updates the way it was installed. A Homebrew installation updates with
`brew upgrade hn05/tap/shoal` on the hub. A downloaded binary updates with
`shoal update` on the hub, or with `shoal hub update` on any of its machines.
The [`daemon` permission](modes.md#permitted-actions) governs
`shoal hub update`.

A managed daemon restarts on the new release once idle. `shoal hub update`
says when a foreground daemon needs a restart.

## Follow the hub's version

A machine follows the hub's version. Once the hub refuses it for running
another version, a downloaded Shoal installs exactly the hub's release, older
or newer, as `shoal update --version` does. It first checks that the hub's key
signed which release that is. A managed daemon then restarts on the release
once idle, with its agents running through the restart, and connects again. A
foreground daemon tells you to restart it.

A machine never installs a release too old to read the state its newer release
migrated. It tells you to update the hub instead, on the hub or with
`shoal hub update` from a machine connected to it. `shoal hub join` never
suggests such a release either.

[`[machine.hub] auto_update = false`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
turns following off.

A Homebrew installation never updates on its own. Its machine tells you to run
`brew upgrade hn05/tap/shoal` there. When the hub runs an older release, which
Homebrew never installs, the machine tells you to update the hub instead.

A machine that does not follow, or fails to, stays off the hub and tells you
what to run. So does the hub's alert about it. For a machine that follows, the
hub raises that alert only once it has refused the machine for 30 minutes.

## Settings from another machine

A setting whose `{ cmd }` names a machine with
[`on`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#values)
takes its value from that machine. A token or key set up once, such as in that
machine's keychain, then serves every machine.

Agent tools' own logins, such as Claude Code's and Codex's, are not shared.
Those tools own the files, and their rotating refresh tokens would log out the
other copy.

The daemon that needs the value sends the command over its link, and the hub
passes it on. The named machine runs it as the user, for at most a minute, and
sends the output back. When `on` names this machine, the command runs here.

Only a machine's daemon asks for a value, never a device. A space's process
asks its own daemon for the setting's value, and that daemon runs only the
command its config gives.

The daemon fetches the value once per operation, so a rotated value applies
at once. The machine that asked keeps the last value in memory, never on disk.
It uses that value while the named machine cannot be reached. Without one,
reading the setting fails, naming the machine and the setting.

The hub records which settings each machine fetched from which machine, and
when, never the values. `shoal hub revoke` stops a machine from asking again.
It lists the settings that machine fetched, for you to rotate, since a value
it received cannot be taken back.

## Shared config

Every machine of a hub, the hub's own included, shares the
[shared config](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#files-and-layers):
`shared.toml` and the mode and rule files in `shared/` beside it.

A machine sends the hub each of those files it edited, once the file loads. It
sends an edit within ten seconds, or at once on `shoal config reload` and
`shoal config set --shared`. An invalid file stays on its machine, and an
alert says why.

The hub keeps whichever edit reached it last, a removal included. Every other
machine writes it and applies it. An edit made while a machine is offline goes
once its link opens.

A joining machine takes the hub's copy of each file it has, and sends those
the hub lacks. When a copy replaces an edit the hub never got, the machine
keeps that edit as `<file>.backup`, such as `shared.toml.backup`, and alerts
you naming it.

## Repositories across machines

`shoal repo ls` lists this machine's repositories, then those every other
joined machine registers. Each names its machine, which `--json` gives as
`machine` and `machine_id`.

A repository that is not shared stays on its machine.
[`shoal start --machine`](#start-on-another-machine) clones it on another
machine on demand.

## Shared repositories

Machines also share the repositories you mark shared.
`shoal repo add <url> --shared` adds one shared. `shoal repo share [repo]`
shares a registered one, which needs an origin. `shoal repo ls` shows which
are shared.

Every other machine registers a shared repository as `shoal repo add <url>`
would, matched by origin:

- Where it has no repository of that origin, the machine clones it at once in
  the background, under its own `root_dir`. The clone takes the same name
  where no repository there has that name, and otherwise one from its URL.
- A repository of that origin the machine already has, whatever its name,
  keeps that registration and becomes shared.
- A [`shoal contribute`](repo.md#contributions) clone that becomes shared is
  no longer removed on its own.

Machines clone from the origin, never through the hub. A clone that fails
records a `shared_repository_failed` alert naming
`shoal repo add <url> --shared`, which registers it once you fix the cause.
Otherwise the machine tries again when it next connects to the hub. The hub's
own machine tries again when its daemon restarts.

`shoal repo unshare [repo]`, or removing a shared repository, stops sharing it
on every machine. Each machine keeps its registration.

### Local config of a shared repository

A shared repository's
[local config](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#files-and-layers),
`.shoal/config.toml` in its repository directory, is shared as the
[shared config](#shared-config) is. An edit reaches every other machine within
ten seconds, or at once on `shoal config set --repo`, once it parses. An
invalid file stays on its machine, and an alert says why.

A repository that becomes shared on a machine, cloned or registered there
before, takes the hub's copy. A replaced edit stays as `config.toml.backup`,
and an alert names it. Once a repository is no longer shared, each machine
keeps its local config as it is.

## Availability

Each machine is `always_on` or `intermittent`. `hub ls` shows each machine's
availability with its reason, and `shoal status` shows this machine's.

[`[machine] availability`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
sets it, with the reason `set`. Under `auto`, the default, the hub guesses it
from how reachable the machine was, never from what powers it:

- A machine whose link was open less than 95% of the last 7 days is
  `intermittent`, and any other is `always_on`. The reason reads
  `connected N% of 7 days`.
- An absence shorter than 5 minutes counts as connected, such as a network
  blip or a daemon restart.
- The hub's own machine counts as connected while its daemon runs with the
  machine awake.
- For the first day the hub knows a machine, it goes by the battery instead. A
  machine with one is `intermittent` (`new, battery`), and one without is
  `always_on` (`new, no battery`).

The hub tells each machine its availability when its link opens and whenever
it changes.

## Spaces of other machines

Each machine keeps its own spaces. It reports a summary of each to the hub as
it changes, and all of them again whenever its link opens. A summary holds:

- the space's number, name and kind;
- its repository, by origin;
- the space above it, which may run on another machine;
- its mode, what lists show it doing, and its progress;
- whether it has an agent, and the name of the one that runs there;
- its open threads and approval requests;
- on an `intermittent` machine, since when it has changes the hub's
  [package](#packages-on-the-hub) of it lacks (`unsent_since`), none while the
  package is current.

`shoal list`, `shoal inbox` and `shoal status <space>` on any machine of the
hub show the other machines' spaces as their machines last reported them.
Once they do, every row names its machine. A scoped caller sees a space its
scope reaches through the spaces above it, on any machine. The `--repo`
filter matches another machine's spaces by their repository's origin.

A machine the hub has not heard from since keeps its spaces listed, with when
the hub last saw it. A revoked machine's spaces leave the hub's lists. A hub
that cannot be reached leaves the other machines out, with a warning.

`shoal status` of another machine's space, by its ID or by a number no space
here has, shows the status its machine answers and names that machine. That
machine keeps the space's sessions and transcripts. While it is offline,
`status` shows what it last reported.

## Space numbers across machines

The hub's machines number their spaces from one sequence, so a
[space number](workspaces.md#space-numbers) names the same space on every
machine. The hub keeps a few spare numbers for each machine, so creating a
space never waits for the hub. A machine that runs out of spares while it
cannot reach the hub numbers above its own highest number.

Another machine's space or spares may have held a space's number first, as
after running out of spares or when a machine joins with spaces of its own.
The space then takes the smallest number free on the hub, and its threads and
approval requests follow it as `S.N`. Its machine tells you and the space's
agent the old and new numbers in a `space_renumbered` alert. The space that
held the number first keeps it.

A space that `shoal move --machine` or `--ssh` moves between the hub's
machines keeps its number. A space that `shoal move -o` wrote takes a new
number when it is imported after its source is gone.

## One machine per space

The hub lists a space ID under one machine, the first that reported it. A
second machine that reports it, as after copying a state directory, is left
out. It raises an alert naming the command that resolves the conflict there:
`shoal rm` of its copy, or first `shoal start --copy` to keep it as a new
space. Once the first machine's space ends, the copy takes its place.

An import, as by [`shoal move`](move.md), claims the ID first. The machine a
space moves to takes the ID over. The machine it moves from keeps a copy
without a conflict until it ends the space there. That copy never takes the ID
back, so removing and restoring the space on the machine it moved to finds no
other machine running it.

`shoal restore` of a space another machine runs is refused, naming
`shoal move` and a copy, while that machine is connected or the hub keeps no
package of the space. While the hub cannot be asked, the restore goes on. A
duplicate then shows as a conflict once both machines report it.

## Commands for another machine's space

A command for a space another machine of the hub owns works from any of them.
The daemon that receives it forwards it through the hub to the space's
machine. That machine decides it as it decides its own callers' requests.
This covers every command that reads or changes a space's state. It also
covers following an agent's session and reading an attached file, whose bytes
come back as that machine reads them.

Your terminal's commands and a device's go as yours. A space's processes'
commands go with that space's scope and permissions, which its machine vouches
for. So an agent acts on the spaces below its own on every machine, and is
refused the rest. An action a space's permission sets to `propose` is refused
from another machine, since its approval request would belong to the space's
machine.

An attached file stays on the machine that keeps its copy. On any other
machine, a record of it in `shoal files`, an alert or a thread names that
machine where it would give the copy's path.

The link carries such answers as byte streams beside its other traffic. Each
stream is held to what its reader has taken, so a reader that falls behind
holds up nothing else.

A machine that received a request but did not answer it in time fails the
command. The error says the request may go on there, which
`shoal status <space>` shows.

A command that needs the machine it runs on is refused for a space elsewhere,
naming its machine. Such commands acquire and release leases, start a demo,
attach files, which the other machine cannot read, or run an agent's hooks.
`resume` with `--focus` or `--execution` is refused the same way.

## Select another machine's spaces

`shoal stop`, `shoal resume`, `shoal restart` and
`shoal restore --discard-agent` take `--machine <name or ID>`, which repeats.
It selects the spaces of that machine that `--all` would select there, by what
the machine last reported:

- `stop` selects those with something to stop;
- `resume` selects those whose saved work an unfinished assignment resumes;
- `restart` selects those with an agent running;
- `restore --discard-agent` selects those with something to discard.

The reports name the machine of each space elsewhere, as `--json` does in
`machine`.

## Pickers

Without a target, a command's picker offers this machine's spaces, then the
other machines' spaces that the request it sends for the picked space
reaches. Each machine's daemon judges its own spaces for that request. The
picker offers every space of an offline machine. Each space names its machine,
and an offline machine's says when the hub last saw it. Choosing a space acts
as naming it does.

The picker of `shoal resume` offers the other machines' spaces with something
to resume.

A command that works in the space's worktree, terminal or files here offers
only this machine's spaces, as `cd`, `exec` and `stop --interrupt` do.

## Requests for an offline machine

While a space's machine is offline, a message, an answer, an approval decision
or a stop for the space waits at the hub. The command says so, naming the
machine, when the hub last saw it and when the request expires. The hub
delivers what waits, in order, once the machine connects.

A request still waiting after seven days is dropped. The space whose process
sent it, or else you, hears of it in a `hub_expired` alert. A request the
machine received, but whose link closed before it confirmed that it ran, is
reported the same way. The hub never sends such a request twice.

`stop --machine` reports one line per offline machine, with how many stops
wait for it. Under `--machine`, `resume` and `restore --discard-agent` skip an
offline machine's spaces, naming it, and go on with the others.

Any other command for an offline machine's space fails at once, following a
session included. The error names the machine and when the hub last saw it.
Where the hub keeps a package of the space, it also names `shoal restore <id>`,
which [takes the space over](#take-over-a-space-from-an-offline-machine) here.

`shoal restore` of a removed space whose record another machine keeps names
that machine. It also names `shoal move <id> -o FILE` with
`shoal restore --from FILE`, which open the space here.

## Start on another machine

`shoal start --machine <name or ID>` starts a workspace or a chat on another
joined machine. The space goes below the space `--parent` names, else below
the caller's own, else at the top level.

Your machine's daemon forwards the start through the hub. That machine's
daemon starts it as its own `shoal start` does, so the space's queue, limits,
cleanup, ports and leases are that machine's. Its queue starts the agent in
the background there, with that machine's agent tools and modes. `--mode`
names one of that machine's modes, which decides whether the start makes a
workspace or a chat. A terminal's picker lists those modes.

Your machine refuses the start before anything goes there when the hub lists
that machine without the `--agent` it names, for the start's repository or
outside one. The refusal names the connected machines that list that agent.
A machine that has not reported yet, or has not registered the repository,
gets the start and decides there.

That machine finds the repository by its origin. Where it has none, it clones
it as `shoal repo add <url>` does. A start below another machine's space, on
any machine, in a repository the starting machine lacks, clones it from the
origin URL that the space's machine reports to the hub.

A `--base` goes as the origin's branch of that name, or as a commit that one
of the origin's branches holds. A base with commits only your machine has,
such as a stacked branch you have not pushed, goes along as a Git bundle of
what the origin's default branch lacks. So nothing needs a push first.

`--wait` follows the workspace's preparation there. `--here` and `--focus`
start the agent there at once. Once the workspace is prepared, they attach
your terminal to the agent as `shoal attach` does. Without a terminal, they
leave it running there.

`--fork` first carries the forked session's files there. That machine's agent
tool looks for them under the repository's settings, so its agent starts from
them. `--copy` and `--checkpoint` carry the copied files there, except ignored
ones, as a bundle of the copied HEAD and of a commit that keeps them. The
copy's new branch there starts at that HEAD, with the files unsaved.

`--in` names a workspace of your machine, so it does not combine with
`--machine`.

A start from a device, such as an app, goes to the machine its launch names.
Without one, it goes to the hub's
[`[machine.hub] default_machine`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub),
else to the machine the device sent it to.

## Placement

A start without `--machine` goes where the hub's
[`[machine.hub] placement`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
policy puts it. The start then continues there as a start with `--machine`
and the same options does. That includes a start whose agent would run in
your terminal, or in a place beside it: the agent starts on that machine at
once and your terminal attaches to it, as `--here` with `--machine` does.

A start stays on your machine when it needs your machine:

- it takes `--in`, `--copy`, `--checkpoint`, `--fork`, `--path` or
  `--add-blocker`, or sets up a repository;
- it reopens a workspace of yours on its branch or issue;
- your machine lacks its repository, or the repository has no origin;
- its agent runs its own way, outside Shoal's terminal hosts, which your
  terminal cannot attach to on another machine;
- it runs its agent with `--here` without a terminal, which waits for the
  agent's exit, or from a space's process.

The hub places a start from what each connected machine last reported with
its spaces, which `hub ls --json` shows as `capacity`:

- the origins of its repositories, each with the agents it can launch there,
  and the agents it can launch outside a repository;
- its `max_heavy_agents`, and the heavy agents its admission would start now;
- the memory new work may use;
- the heavy agents waiting in its queue;
- whether its user [walked away](machine.md#walking-away) while its
  `keep_awake` admits nothing new then, or
  [paused it](machine.md#pausing-a-machine).

A start that finds the machine full meanwhile waits in its queue there.
Placement never moves a space once it is created.

`balanced` places a start by its lifetime, its weight and what it requires. A
start with `--disposable` is disposable, and any other is lasting. It ranks
the machines by these rules, each ordering only the machines the rules before
it rank equal:

1. The machine has the start's repository, can launch its agent there and
   has reported since its link opened, and its user has neither walked away
   nor paused it. Lasting work goes only to an
   `always_on` machine with room, and disposable work to any machine with
   room. Without one, the start stays on your machine.
2. Disposable work goes to an `intermittent` machine first, leaving the room
   of `always_on` machines to lasting work.
3. Your own machine comes first.
4. A heavy start goes to the machine whose admission would start a heavy
   agent now, with the most spare memory. A light start goes first to a
   machine with `max_heavy_agents = 0`, then to one where no heavy work waits,
   with the most spare memory, then to the one where the least heavy work
   waits.

`shoal start` names the machine with the policy's reason before it attaches
your terminal. Under `balanced`, the reason is the first rule that put it
ahead. It also names each connected machine it skipped: for lacking the
repository, for an agent it cannot launch there, for having not reported yet,
or for a user who walked away or paused it. `--json` gives these as
`placement`, in a chat's start and as a last document after a workspace's.

A start stays on your machine, with a warning, when the hub cannot be asked.
So does a start that never reached the chosen machine, such as one that went
offline since its last report.

### External placement

Under `placement = "external"`, an unscoped client on the hub's machine, such
as an app's server, registers as the placer with `shoal-client`'s
`hub::Placing`. It answers each snapshot the hub sends it, of the start and
every connected machine, with the machines in order of preference. The latest
registration takes over from the one before.

The hub places a start by `balanced` instead, and the start names it, when:

- no placer is registered;
- the placer does not answer in the time the
  [configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
  gives;
- it answers with a machine that is not connected, lacks the start's
  repository or cannot launch its agent there, since placement never clones
  a repository.

## The space tree across machines

The tree of spaces may span machines. A space stays below the space above it
when it moves to another machine without that space, or when
`shoal start --machine` starts it there. Its machine reads that parent from
what the hub lists of it, as it reads a parent of its own. The space's
permissions stay no wider than the parent's, as the parent's machine last
reported them.

Removing a space first removes the spaces below it on other machines, through
their own machines. A space below that cannot go keeps the space above. So
does a removal that confirmed only the spaces it saw here.

A space on an offline machine cannot go. For a removal you or an agent asked
for, the hub holds that space's removal until its machine connects. The space
above stays until it is removed again.

What a space passes up the tree reaches the nearest space above with an agent,
wherever it runs. That covers its agent's alerts, threads, approval requests,
completion, ready marks and exits, and the issue and PR activity of a space
without an agent. That space decides the space's approval requests, answers
the threads it holds and sends it alerts from its own machine. A machine whose
link to the hub is down keeps what its spaces pass up, and sends it once the
link opens.

A thread or approval request that waits for a coordinator whose machine is
offline passes on after
[`[machine.hub] escalate_after_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub).

## Packages on the hub

An `intermittent` machine keeps the latest package of each of its workspaces
and chats on the hub, so their work outlives the machine going away. A package
is what [`shoal move --keep -o`](move.md) writes: a new checkpoint of a
workspace with a bundle of its commits, its agents' session files and its
records. The hub's package keeps the space's own ID, and the machine builds it
without stopping its agents.

The machine looks at a space when its agent's turn ends, and at every space
every 10 minutes. It sends a space whose HEAD, branch, files, agents' session
files or records changed since its last package. Records include its links,
threads and queued work. It sends each space at most once every 5 minutes. A
space that did not change sends nothing.

The hub keeps one package per space in its state directory. It replaces a
package only once the new one has arrived whole. It deletes the package once
the space ends on its machine, as by removal or a move, or once the machine is
revoked.

While the disk of its state directory has less free space than
[`[machine.overload.warning] disk_free_gib`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#overload),
the hub refuses new packages and records a `hub_package_refused` alert.

The hub's own machine sends no packages.

## Take over a space from an offline machine

`shoal restore <space>`, by number or ID, takes over a space whose
`intermittent` machine is offline, from the package the hub keeps of it. An
agent does so as its [`admin` permission](modes.md#permitted-actions) allows,
as for `restore --from`.

The restore fetches the package and imports it under the space's ID, as
`shoal restore --from` does. Its claim makes the space this machine's. It
queues the restores of the space's agents, which continue their sessions. It
warns when the machine had changes the package lacks, naming how old the
oldest is.

## Retire a copy

When the machine a space was taken from connects again, the hub names the
spaces taken over meanwhile. The machine retires its copy of each: it stops
the copy's agents and removes it through the removal path. The removal keeps a
checkpoint, the agents' sessions in its removal record, and the branch. It
leaves the spaces below, whose parent now runs on the other machine.

A copy that changed after the hub's package also raises a `space_diverged`
alert, naming what opens that work. For a workspace, that is
`shoal start --copy <id> --checkpoint <number>` with the checkpoint its
removal kept. For a chat, it is its agents' sessions.

A copy the machine cannot remove raises a `space_conflict` alert, naming the
error and `shoal rm <id>`. The hub asks the machine again the next time it
connects.

## Hand-over at shutdown

A joined machine hands its spaces over as it shuts down, whether it is
`always_on` or `intermittent`. Under
[`keep_awake = "handover"`](machine.md#handing-work-over), it also hands over
the spaces with something to do once its user walks away. So does a
[pause](machine.md#pausing-a-machine). A machine in no hub keeps its spaces.
So does the hub's own machine, unless it is paused.

The [stop before the shutdown](load.md) saves the agents first. The machine
then sends the hub the final package of every chat, and of every workspace
whose worktree is ready except one a CI job runs in. It sends them all at
once, until a second before logind's `InhibitDelayMaxSec` ends. For a signal,
it sends them for 15 seconds, which the service definition's stop timeout adds
to the daemon's own.

For each package that arrives whole, the hub chooses a machine among the other
connected machines, by its
[`[machine.hub] placement`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
policy. The hub's own machine stands in for the caller's. The hub chooses from
the space's repository, its agent's tool, its mode and whether the workspace
is disposable.

When no connected machine can launch the space's agent, the hub keeps the
package and the space goes to the hub. The hub places it again as each machine
reports its capacity, and continues it on the first machine it chooses. When
this machine's link opens first, the space stays here instead, and a
`handover_failed` alert names `shoal resume <id>`.

The shutting-down machine reads that choice and tells the hub it leaves the
space there. The chosen machine then imports the package under the space's
ID, as `shoal restore` of a kept package does. It queues the agents' restores,
which continue their sessions. A chat in a workspace's worktree goes where
that workspace went, once the workspace has continued there. It stays when the
workspace stays.

A `system_shutdown`, `daemon_stopped` or `handed_over` alert names the machine
each space went to, or the hub. For a space that stays, it says why: its
package did not reach the hub in time, or the hub cannot be reached. A
space whose leaving the hub did not acknowledge in time may have gone, and the
alert says so. It keeps no restore here either.

A space that stays restores here at boot as before. A space that went keeps no
restore here. Once this machine's link opens again, it
[retires its copy](#retire-a-copy). Work a reboot moved does not come back on
its own.

When the chosen machine fails to continue a space, the space stays here too.
The hub raises a `handover_failed` alert naming `shoal resume <id>` here and
`shoal restore <id>`, which takes the space over from the hub's package.

The daemon counts every SIGTERM or SIGINT it receives as a shutdown, on any
platform. So a logout that ends the daemon, a service manager's stop and
Ctrl-C of a foreground daemon hand work over too. `shoal daemon restart`,
`shoal daemon stop` and upgrades hand nothing over.

When a shutdown is cancelled after the hand-over, the machine has the hub
retire its copies of the spaces that went at once, as it does once its link
opens again.

## Sleep announcement

A joined machine announces its sleep, whether it is `always_on` or
`intermittent`. On macOS, it acts on IOKit's will-sleep notification. On
Linux, it acts on logind's `PrepareForSleep` under a sleep delay lock.

It first sends the hub one notice, naming each space whose package lacks
changes and since when. It then sends a new package of each space that
changed, all at once. An `always_on` machine keeps no packages current, so it
sends a package of each space with an agent working or queued instead. It
sends until a second before the platform stops waiting: logind's
`InhibitDelayMaxSec`, or 30 seconds on macOS.

The machine then closes its link. It admits no queued agent until it fully
wakes, so a dark wake, such as Power Nap's, changes nothing.

A machine whose link drops while its last report said it ran on battery below
5% counts as having announced its sleep at that report.

## Takeover after a sleep

Once a machine that announced its sleep has been away for 10 minutes, the hub
takes over each of its spaces that has something to do. A space has something
to do when:

- its agent was working at the machine's last report;
- it has a queued agent;
- the hub holds an event or request for it since the notice. The hub takes
  over such a space as the event arrives.

The hub takes over only a space whose package is current, or lacks only
changes from less than 15 minutes before the notice. It places each space as
a [shutdown's hand-over](#hand-over-at-shutdown) does. The machine it chose
continues the space under its ID and queues its agents' restores. A chat in a
workspace's worktree goes with that workspace, and only together.

A `spaces_taken_over` alert names where the spaces went. The events the hub
held for them follow them there. A space that no machine can take waits on the
sleeping machine, and the hub tries again as each machine reports its
capacity. A space whose package lags more stays, and the
[`machine_unreachable` alert](#unreachable-machines) names it.

The machine retires its copies of the spaces taken over once it wakes. A
machine that wakes within the 10 minutes keeps everything where it was.

These times and the battery level are fixed, not settings.

## Unreachable machines

The hub takes a space over on its own only after an announced sleep. A machine
that is awake but lost only the hub keeps working.

Once an `intermittent` machine, or one that announced its sleep, has been
unreachable for 30 minutes, the hub records one `machine_unreachable` alert
for that drop. The alert names the machine's spaces that had something to do:
an agent working when its link dropped, a queued agent, or an event the hub
holds for one since. For each space, it says how far behind the hub's package
is, and gives the `shoal restore` that takes it over.

## Path forwarding

[`[machine.hub] forward`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hub)
lets one address and tunnel serve both the hub and an app's own server, such
as Quay's. That server runs as its own process, a paired device that uses
`shoal-client`.

The hub's listener forwards a request under one of the path prefixes to that
prefix's local address, with its path unchanged. The longest prefix that
covers the request wins. Upgrades, such as a WebSocket's, are forwarded too.

The hub forwards a request only once a paired machine or device signed it. A
signed request carries these headers:

- `shoal-member` names the member;
- `shoal-time` gives the time it was signed, within five minutes of the hub's
  clock;
- `shoal-nonce` gives a nonce no earlier request used;
- `shoal-signature` gives the member's signature of what
  `shoal_protocol::hub::app_servers::signed` makes of them, the method, and the
  path with its query.

The signature covers no body. The server receives `shoal-member`, which the
hub checked, without the other three headers.

The hub refuses any other request under a prefix with `401`, and it never
reaches the server. A request under no prefix gets `404`.

The hub keeps each nonce it took in its store until requests signed when it
was taken are no longer accepted. So a restart lets no request through twice.
