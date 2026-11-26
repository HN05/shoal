# Agent queue

This file says how the agent queue admits and orders agents, and what `shoal
queue` and `shoal priority` do.

```sh
shoal config set machine.max_heavy_agents dynamic   # Start agents while memory and disk allow
shoal config set machine.max_heavy_agents 3         # Fixed machine-wide limit
shoal config set machine.max_heavy_agents 0         # Only light agents start here
shoal config set agent.max_heavy_running 2 --repo shoal  # And at most two of them in this repository
shoal config set agent.max_heavy_running none --repo docs # No repository limit
shoal start <issue-url> --agent claude          # Starts now, or queues when no slot is free
shoal start <issue-url> --agent claude --now    # Starts now regardless of the limits
shoal start <issue-url> --agent claude --add-blocker <id> --add-blocker 12  # Starts once that workspace and #12 clear
shoal resume -A                               # Queues the restore of every stopped agent
shoal queue                                   # Waiting and starting entries; on a terminal, pick one to act on
shoal queue start <entry>                     # Starts a waiting entry, or a space's queued restore, now regardless of the limits
shoal queue start --attach <entry>            # Starts it and attaches this terminal to its agent's session
shoal queue cancel <entry>                    # Removes a waiting entry or a failed launch; a restore's agent stays stopped
shoal start <issue-url> --priority urgent      # Its queued launch and new space rank ahead of the rest
shoal priority urgent [space]                 # Ranks the space's queued work ahead of the rest
shoal priority 30 [space]                     # Any number from 0 to 100; low is 25, normal 50
```

[`machine.max_heavy_agents`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine) limits the heavy slots: the agents of
[heavy](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules) modes that run at once, `dynamic` or a number
from 0, where 0 keeps every heavy agent queued until `shoal queue start`. It defaults to `dynamic`,
which starts one agent at a time, at least 10 seconds apart, while measured
memory and disk leave room below the
[`[machine.overload.warning]`](load.md) memory and disk
thresholds, which [`load`](load.md) sets. CPU holds back no
agent, since more work on a busy CPU only runs slower; only a
[CI job](ci.md#job-admission) beside running ones waits for it. A new agent mostly
reads files at first and compiles later, so for its first three minutes it
counts at the memory a settled agent uses: the average resident memory of the
workspaces of the agents that have run longer, at the latest usage sample, at
least 1 GiB, or 2 GiB before any has. A start must fit the memory left below
the warning threshold once every such warming agent uses what it is expected
to and every running [CI job](ci.md#job-admission) what it has yet to use, and none starts
while memory stall is past its warning level.
With no agent running, one starts whatever memory and disk read. Critical
memory, including stall past its stop level before it has held, disk below `machine.overload.disk.stop_free_gib`, or a failed memory
or disk reading starts none, and disk below `[machine.overload.warning]
disk_free_gib` starts none beside running work. Disabled memory or disk
protection ignores that signal. While a fixed
limit applies, so does the disk rule: below `[machine.overload.warning] disk_free_gib`
an agent starts only when none uses a heavy slot, and below
`machine.overload.disk.stop_free_gib` or without a reading none starts. That
value counts every heavy agent on the machine; a fixed [`agent.max_heavy_running`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent) counts
only one repository's heavy agents, within the machine limit; a chat without a
repository counts toward the machine's alone. Each tracked agent, a
workspace's or a chat's, uses a heavy slot while it runs, awaits reattachment
after a daemon restart, or waits for `shoal resume`, unless its space is
done, its restore waits in the queue or its mode is
[light](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules), which starts it at once beside the agent
limits and whatever memory is in use, though critical or missing readings and
low disk hold it as they hold every start; `shoal done`, the agent's exit,
`restore --discard-agent` and removal free it.
A [local review](reviews.md#local-reviews)'s reviewer uses no slot, so no count limits
reviews, but its memory counts while it runs and a queued review waits for
room and disk as an agent's start does, ranked with the workspace it reviews.
Overload protection stops a running reviewer before any agent, and disk
protection stops every reviewer; the review is queued again and starts over
once readings are healthy.

`shoal start` of a new workspace sends the daemon one launch, whoever makes
it: once a terminal has picked what the start leaves to a picker, the daemon
records the workspace at once, `preparing`, below the caller or the space
`--parent` names, queues its agent there and prepares the workspace in the
background, creating its worktree and running setup and the post-setup hook,
while the agent waits in the queue for that, holding no slot, then for a heavy
slot. The start returns reporting both (`--json` emits the workspace record,
then the entry), unless its agent runs in the caller: `--here` runs it in this
terminal, a focused start from a terminal opens its place beside the terminal
at once, and the user's agent Happy runs runs in the caller's process, while a
`shoal create` opens the workspace's shell where a focused start would open
its agent. Such a start holds the agent's entry and shows the
preparation, then starts the agent at once where `--now` or a focused start
asked for that, otherwise when a slot is free, and otherwise leaves it to the
queue; `--json` then emits the workspace record, or the entry when the agent
waits. Until its worktree exists, commands that read it refuse
the workspace, and `shoal rm` removes only its record and leases. A
workspace created at once, by `shoal create`, `convert`, a restore, a copy or
`adopt --copy`, whose worktree fails stays `failed` the same way, and
`shoal setup <space>` creates it again from what it was made from: a
restore's checkpoint, or the outside worktree `adopt --copy` copies, which
the retry names when it is gone, keeping what an earlier try left there in a
checkpoint before it writes files over it, and refusing, with the directory's
name, files a cut-short `git worktree add` left. A restore whose worktree fails
still opens the space with its links and agents and ends naming that retry. A
preparation that fails, creating the worktree or in setup or the post-setup
hook, leaves the workspace failed and its agent waiting, and records a
`preparation_failed` alert, which reaches the agent of the nearest
space above with one as a message, or else the user, naming
`shoal setup <space>`, which retries it, creating the worktree first when it
has none, and `shoal rm <space>`. `--wait` returns
once the preparation ends, reporting its failure as a start from a terminal
does. `--now` starts that agent
regardless of the limits, as does `--focus` for a scoped caller, as its
space's [`limits` permission](modes.md#permitted-actions) allows. A start whose agent
waits for other work leaves the workspace `preparing` without a worktree;
`shoal list` and `shoal status` show what it waits for. Once that
work is complete the daemon creates the worktree on the done blocker's branch,
or from the default branch when no blocker leaves one, and sets it up, which
the agent waits for in turn. A preparation also waits before
it creates the worktree while free disk space is below
`[machine.overload.warning] disk_free_gib`, or unread, with disk protection on,
and starts once disk space allows. `--wait` returns at once for a preparation
that waits. `shoal stop` of such a workspace cancels its agent's entry and keeps
the workspace, whose preparation then waits until `shoal setup` runs it; while
it waits for work or disk space, `shoal setup` refuses it, and `shoal rm`
removes it. When the queue admits the agent of an existing workspace whose
branch holds nothing of its own, the daemon fast-forwards the branch to what it
builds on, as `shoal refresh` does with what the checkout has of the default
branch, before the agent starts. The daemon starts waiting entries
in [rank order](#queue-order) as slots free, passing over an entry whose
repository is full or that does not fit in memory, and records a
`queue_started` alert. While an entry waits for its memory, nothing of
another priority or tier that it outranks and that needs measured memory
starts: so a new agent waits for the CI job of an open PR, and an `urgent`
launch goes ahead of finishing work of a lower priority. Work of the same
priority and tier that fits starts ahead of it until it has waited 15
minutes. Each entry it holds back waits with `memory_kept`, naming it. While the [proxy](agents.md#retrying-proxy) of a running
agent finds its tool's upstream rate-limited or out of quota, queued agents of
that tool wait with `upstream_limit` and others start ahead: until the reset
the upstream names, or, without one, 15 minutes after the proxy last met the
limit, unless a request through any proxy of that tool succeeds first. The
daemon log names each limit a proxy reports, with its execution, kind and end,
and when it lifts. An entry queued from another host's pane, block
or tab opens a background tab or window where it was queued; otherwise it runs where
`terminal.default_host` puts it, by default a detached zmx session that `shoal attach`
reaches. An admitted agent keeps its slot until it
registers. A start that fails, or whose agent does not register for two
minutes, frees the slot, and its launch stays on its space, a workspace or a
chat, as a `failed` entry with its error, which `shoal queue` and `shoal inbox`
list: nothing admits it, it keeps no space from cleanup, and its failure
reaches the agent of the nearest space above with one, else your
alerts, naming `shoal resume <space>`, which queues it again exactly
as it was, and `shoal rm <space>`. A new launch in the space replaces it, and
the space's removal drops it. `--now` starts the agent at once once its workspace is prepared, and
still takes a slot, and so does a start the user waits for: one `--focus`
focuses, or an agent started from a terminal without an issue or arguments
that the `focus` setting does not send to the background, unless
`--add-blocker` makes it wait for work.

A workspace has one agent of its own. The daemon refuses a launch in an
existing workspace, from any caller, while that workspace's agent runs or waits
in the queue, including while a blocker holds it back, naming `shoal attach`,
`shoal restart`, `shoal stop` or `shoal queue` as fits. A stopped agent awaiting
`shoal resume`, or an asleep one whose restore Shoal queued on its own, refuses
it too, naming `shoal resume` and `--replace`, unless the start passes
`--replace`: the launch then stops the wrapper that waits to restore an asleep
agent, discards the agent, with its last sessions and stopped commands, as
`shoal restore --discard-agent` does, and queues its own.
Without `--replace`, a start on a terminal asks whether to resume the stopped
agent, replace it or cancel. `--replace` applies only to a start of a
workspace's own agent, with `--in` or on a branch or issue the workspace has,
so a chat in its worktree refuses it, and a scoped caller replaces only the
agent of a space below its own, as it discards one. A last session, whose agent
exited on its own, does not count: the new agent replaces it. Chats in the
workspace's worktree never count.

An agent's automatic restore waits in the same queue, ranked with launches,
from when its processes stop, and waits for what the agent's stop calls for,
whether its wrapper or the daemon restores it: nothing blocking the space for
an agent stopped because its space was blocked, an update for one stopped
while idle, the recovery interval's healthy readings for one load or disk
space stopped mid-turn, and a free slot, or room for its load, for one stopped
mid-turn otherwise, such as by a signal. A restore that waits for an update
holds back nothing behind it, and holds no slot once its processes are proven
stopped, until an update arrives for the agent; then it needs a free slot, or
room for its load, as a launch does, and with disk protection on
`disk.cleanup_free_gib` available, without waiting for healthy readings. A
restore that waits for healthy readings starts one per interval and keeps its
place meanwhile without holding back the entries behind it; while its wrapper
waits, it keeps the agent's slot, so a fixed limit lets it take that slot back
on a full machine, while `dynamic` still needs room for its load, and a lowered
limit holds it until other agents leave. A stop, reload or unhealthy reading
before the restored agent starts returns it to its place, and disabling
`[machine.overload.recovery]` leaves it stopped for `shoal resume`. The
restored agent takes the slot over once it runs. Stopping the workspace or a
restore Shoal refuses removes the entry and keeps the recovery record.

A waiting wrapper restores its agent in place. A wrapper that exits after its
restore is admitted and before the agent starts, as one whose binary an
upgrade removed can, leaves the restore in the queue as a signal stop's
restore waits there, opening where the agent ran. A daemon restart or a lost
connection keeps the order, and once a waiting wrapper's execution has ended
without it reattaching, the daemon starts the agent as it starts a signal
stop's restore. `shoal resume` of an agent whose wrapper waits uses this
entry: the restore keeps its place and waits only for a heavy slot, and
`--now` or `--focus` starts it at once in its wrapper.

A restore that `shoal resume` or a signal stop queues joins the back of the
queue and waits as above; `shoal resume` makes it wait only for a heavy slot,
keeping its place.
The daemon
starts it as it starts a queued launch: in the place the agent stopped in, where
its host keeps that place after its agent, once that is back at its prompt,
otherwise in a background place of the host it stopped in where that host's
config lets it open places, such as `terminal.herdr.new_tab`, otherwise where
`terminal.default_host` puts it. A restore whose
agent has not started within two minutes records a `queue_failed` alert
and leaves the agent stopped. Stopping the workspace or marking its assignment
done cancels a pending one, and resuming or forgetting the agent another way
removes it.

`--add-blocker` names what blocks the launch: a workspace or chat of the
repository by ID, a queued agent by its entry's ID, or an issue number or URL,
so a bare number is an issue; repeat it for more. An issue launch is also
blocked by the open issues its forge records as blocking it: Forgejo and GitHub
issue dependencies and GitLab "is blocked by" links; when the forge cannot list
them, such as with that feature turned off, Shoal warns and they do not block
the agent. Such a launch queues even without a limit and starts once every
workspace blocking it is done at its current HEAD, every chat is done, every
queued agent has opened its workspace and that workspace is, and every issue is
closed or done in a workspace of the repository that links it; a removed space
or queued agent no longer blocks.
A commit after `shoal done` blocks the launch again; issue state is read every
30 seconds. A new branch then starts from the done blocker's branch and is
stacked on that workspace, as with `--base`, so the agent builds on the
finished work before it merges. With several done blockers it takes the one
whose HEAD contains the others, and while they are on separate branches it
waits until all but one have merged and their workspaces are gone. `--base` and
an existing `--branch` keep their own starting point, and `--now` and `--focus` cannot
wait for blockers.

`shoal queue` lists the entries of every repository, since positions apply
machine-wide, or of one with `--repo`, as `queued, position N` or `starting`,
followed by `blocked by` and what blocks a waiting launch, or `after an update` for a
restore of an idle agent, then what held a waiting entry back at the last
admission pass, as `held by` memory with what the entry needs and what is
left, memory kept for a higher-ranked entry it names, low disk space, `machine.max_heavy_agents`, `machine.max_jobs`,
`agent.max_heavy_running`, a full `runner.permit` pool, a critical or missing
reading, or a busy CPU with its percentage and the level a job waits at; the
stagger between starts is too brief to show. A restore shows its
agent; `shoal list` shows the entries after the workspaces. `queue --json` gives the entries with their `kind`,
`launch`, `restore` or `job`, a restore's stopped `execution`, a job's `job`
with its run, `blockers` while any remain, the setup of the workspace it opens
among them,
`waits_for_update` for a restore of an idle agent, `held_by` with its `reason`
(`memory` with `needed` and `left` bytes, `memory_kept` with the
higher-ranked entry's name `behind`, `disk`, `max_heavy_agents`, `max_jobs`,
`repository_limit`, `permits`, `reading`, `cpu` with the `busy` and `level`
percentages, or `upstream_limit` with the `limit`,
`rate_limited` or `exhausted`, and the Unix-seconds `until`), a waiting entry's `rank` with its
`priority` and `tier` and, for each it took from other work, `priority_from`
or `tier_from` with that work's space number or launch name `from` and the
`dependency` (`blocked`, `parent`, `thread`, `stacked`, `lease` or
`serves`), and
`base` once a launch starts on a blocker's branch, while `list --json` stays
a list of workspaces. Each line starts with the entry's ID, which `queue
start` and `cancel` take. On a terminal, `shoal queue` picks a waiting entry and
then starts it, with or without attaching, or cancels it.

`queue start` starts a waiting entry, by its ID or, for a queued restore, by
its space's number or ID, now regardless of the limits, as `--now` does, and
for a scoped caller one its scope reaches, as its [`limits` permission](modes.md#permitted-actions) allows, or restores an idle agent waiting for an update; it refuses a launch
still waiting for work, whose branch it builds on, and a restore whose agent's
processes have not stopped yet. With `--attach` on a terminal it then waits, up
to ten minutes since setup comes first, for the session the agent runs in and
attaches to it as `shoal attach` does. `queue cancel` removes a waiting entry: a
cancelled restore stops the waiting wrapper as `shoal stop` does and leaves the
agent for `shoal resume`. `shoal rm` also removes a waiting launch by its ID;
a starting entry belongs to its workspace. Every caller sees every entry.
Scoped callers may cancel the entries their scope reaches: those of its
spaces but their own, a CI job's through the spaces its run is for, and a new
space's launch through the parent it goes below; starting an entry is for the
user.

`shoal priority` sets a space's priority, which ranks its queued work and the
work it waits on (see [Queue order](#queue-order)): `low`, `normal` or
`urgent`, or a number from 0 to 100; `shoal start --priority` sets it on the
new space, and its launch ranks with it while queued. It lasts until changed,
through `done` and conversion. A space's agent sets it on its own space and those below, at
most to its own space's priority, and what it starts without `--priority` takes
its own space's when that is below `normal`; you and a master session set any.

## Queue order

Waiting entries start by priority, highest first: a space's priority, from 0
to 100, is the one set on it, else the highest its issues' labels map to under
`[priority.labels]`, else 50, and a queued launch has the one it was queued
with, or else the highest its issue's labels map to, until its space exists.
Within a priority, [`machine.schedule`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine) may put other work
first: under `speed`, the work with the longest chain of work waiting on it,
counted through the waits below; under `cache`, restores of agents whose prompt
cache, as their hooks last reported it, has not expired, the latest to expire
first. Neither passes over an entry that has waited an hour. Then, and under
the default `balanced`, they start by tier, highest first:
`finishing`, the agents of a
space with an open PR it opened or was assigned or with a ready mark, and CI
jobs on PRs and on the default branch; `started`, restores of agents that have
worked in their space; `new`, other launches and CI jobs. An entry rises one
tier for each full hour it waits, but never to a higher priority. Within a
tier, a launch of a new workspace whose issue shares fewer
[areas](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#issues) with the workspaces of its repository goes first:
an area is a label with the `[issue] area_prefix`, a workspace holds those of
its assigned issues and PRs until it is removed, and a chat holds none. Then
the space with the larger share of its plan steps done goes first, then the
space created earliest, then a CI job or review ahead of an agent of the same
space, then the entry that arrived first.

Work that other work waits on takes that work's priority and tier where they
are higher, each on its own, through any chain of waits: what blocks a queued launch or a space's progress, which
for an issue is the spaces working on it, for a PR the spaces that opened or
were assigned it, and for a CI run its queued jobs and those of its workflow
on the default branch; a workspace's children; the holder of a thread another
space opened that waits; the workspace a stacked one builds on; and the spaces holding a
lease another space queues for. A chat passes its children only a priority
of its own, set or from its issues' labels, or passed down to it, so they compete with every other worker. A CI job ranks with the
highest of the spaces its run links, taking its priority even when lower, and
a review with the workspace it reviews. `shoal queue` shows each waiting
entry's priority and tier, and for each it took from waiting work, that work
and why, as in `priority urgent (from 7, its parent), finishing (from 12,
blocked on this)`, then each area a launch shares with the spaces holding it,
as in `sharing area/daemon: 12, 15; area/cli: 3`.
