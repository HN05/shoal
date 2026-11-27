# Agent queue

This file says how the agent queue admits and orders agents, and what `shoal
queue` and `shoal priority` do. [Agent queue](../design/queue.md) gives the
reasons.

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

## Heavy slots

An agent of a
[heavy](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
mode takes a heavy slot while it runs.
[`machine.max_heavy_agents`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
limits the heavy slots of the machine. It takes `dynamic`, the default, or a
number from 0. A fixed number counts every heavy agent on the machine. 0 keeps
every heavy agent queued until `shoal queue start` starts it.

[`agent.max_heavy_running`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#agent)
limits one repository's heavy agents, within the machine's limit. A chat
without a repository counts toward the machine's limit alone.

### Who holds a slot

Each tracked agent of a heavy mode, a workspace's or a chat's, holds a heavy
slot while it:

- runs,
- awaits reattachment after a daemon restart, or
- waits for `shoal resume`.

An agent holds none once its space is done, or while its restore waits for an
update. `shoal done`, the agent's exit, `shoal restore --discard-agent` and the
space's removal free its slot.

### Light agents

An agent of a
[light](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
mode holds no slot. It starts at once, whatever the agent limits and the
memory in use. Critical or missing readings and low disk still hold it back,
as they hold every start.

## Dynamic admission

Under `dynamic`, agents start one at a time, at least 10 seconds apart. Each
start needs room below the memory and disk thresholds of
[`[machine.overload.warning]`](load.md#warnings), which
[`load`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine) sets.

CPU holds back no agent. Only a [CI job](ci.md#job-admission) beside running ones
waits for it.

### Memory of new agents

For its first three minutes, a new agent counts at the memory a settled agent
uses. That is the average resident memory of the agents that have run longer,
outside the commands their spaces run, at the latest usage sample. It is at least 1 GiB, and
2 GiB before any agent has run that long.

A start must fit the memory left below the warning threshold, after:

- every such new agent uses what it is expected to, and
- every running [CI job](ci.md#job-admission), command and seed build uses
  what it has yet to use.

No agent starts while memory stall is past its warning level. With no agent
running, one starts however little room memory and disk leave, unless a
reading below stops it.

### Readings and disk

These readings start no agent:

- critical memory, including memory stall past its stop level before it has
  lasted long enough to stop work,
- free disk below `machine.overload.disk.stop_free_gib`,
- a failed memory or disk reading.

With free disk below `[machine.overload.warning] disk_free_gib`, no agent
starts beside running work. Turning memory or disk protection off ignores its
readings here.

A fixed limit keeps the disk rules. Below `[machine.overload.warning]
disk_free_gib`, an agent starts only when no agent holds a heavy slot. Below
`machine.overload.disk.stop_free_gib`, or without a reading, none starts.

## Reviews

A [local review](reviews.md#local-reviews)'s reviewer holds no slot, so no
limit counts reviews. Its memory counts while it runs. A queued review waits
for memory and disk as an agent's start does, and ranks with the workspace it
reviews.

Disk protection stops every running reviewer. Shoal queues each stopped
review again, and it starts over once readings are healthy.

## Commands and seed builds

A [`shoal run` command](run.md#waiting-for-memory) and a repository's
[seed build](setup.md#seed-builds) hold no slot either. Each waits until the
memory its earlier runs used fits: the 95th percentile of the peaks of its
latest 30 whole runs, as for a [CI job](ci.md#job-admission). A command that
has never run counts at nothing. Until it ends, it holds what it has yet to
use of that.

- It does not wait the 10 seconds between starts, since it counts at its whole
  estimate from its start. An agent or job admitted beside it waits for the
  next pass.
- With no CI job, command or seed build running, one starts whatever it
  needs, since the agents beside it free no memory by its waiting.
- Under a fixed `machine.max_heavy_agents`, they start at once.
- A waiting command ranks with its workspace. A seed build ranks at priority
  0, so it starts after the work of every other priority, however long it
  waits.

Protection stops them after CI jobs and before demos and agents. Each then
waits again, for healthy readings too, before it runs again. Agents count at
their own memory, without their space's commands, so no agent reserves memory
for builds it may run later.

## Launch a new workspace

`shoal start` of a new workspace sends the daemon one launch, whoever makes
it. On a terminal, the start first asks what it leaves to a picker. Then the
daemon:

1. records the workspace at once as `preparing`, below the caller or the space
   `--parent` names,
2. queues its agent there, and
3. prepares the workspace in the background: it creates the worktree and runs
   setup and the post-setup hook.

The agent waits in the queue for that preparation, holding no slot, and then
for a heavy slot. The start returns and reports both. `--json` emits the
workspace record, then the queue entry.

### Agents that run in the caller

Some starts run the agent in the caller:

- `--here` runs it in this terminal.
- A focused start from a terminal opens its place beside the terminal at once.
- The user's agent that Happy runs runs in the caller's process.

`shoal create` opens the workspace's shell where a focused start would open
its agent.

Such a start holds the agent's entry and shows the preparation. It then
starts the agent:

- at once, where `--now` or a focused start asked for that,
- otherwise when a slot is free,
- otherwise not, leaving the agent to the queue.

`--json` then emits the workspace record, or the entry when the agent waits.

`--wait` returns once the preparation ends. It reports a failure as a start
from a terminal does.

### Before the worktree exists

Until its worktree exists, commands that read the workspace refuse it.
`shoal rm` then removes only its record and leases.

### Failed preparation

A preparation can fail while it creates the worktree, or in setup or the
post-setup hook. The workspace is then `failed`, and its agent keeps waiting.
Shoal records a `preparation_failed` alert. The alert reaches the agent of the
nearest space above with one as a message, or else the user. It names two
commands:

- `shoal setup <space>` retries the preparation. It creates the worktree first
  when the workspace has none.
- `shoal rm <space>` removes the workspace.

[Setup](setup.md#failed-setup) says how a failed setup is retried or accepted.

### Failed worktrees

Some commands create a workspace at once: `shoal create`, `convert`, a
restore, a copy and `adopt --copy`. A workspace whose worktree fails there
also stays `failed`. `shoal setup <space>` creates the worktree again from
what the workspace was made from:

- a restore's checkpoint, or
- the outside worktree that `adopt --copy` copies. The retry names it when it
  is gone.

The retry keeps what an earlier try left there in a checkpoint before it
writes files over it. It refuses files that a cut-short `git worktree add`
left, and names their directory.

A restore whose worktree fails still opens the space with its links and
agents. It ends by naming that retry.

### Preparations that wait

A start whose agent waits for other work, as [Blockers](#blockers) says,
leaves the workspace `preparing` without a worktree. `shoal list` and
`shoal status` show what it waits for. Once that work is complete, the daemon
creates the worktree on the done blocker's branch, or from the default branch
when no blocker leaves one. It then sets the workspace up, and the agent waits
for that in turn.

With disk protection on, a preparation also waits before it creates the
worktree while free disk is below `[machine.overload.warning] disk_free_gib`,
or unread. It starts once disk space allows. `--wait` returns at once for a
preparation that waits.

`shoal stop` of a preparing workspace cancels its agent's entry and keeps the
workspace. Its preparation then waits until `shoal setup` runs it. While the
preparation waits for work or disk space, `shoal setup` refuses the workspace
and `shoal rm` removes it.

## Admission

The daemon starts waiting entries in [rank order](#queue-order) as slots free
up. It passes over an entry whose repository is full or that does not fit in
memory. Each start records a `queue_started` alert.

Before an agent starts in an existing workspace, its branch may move, as
[when a refresh runs](branches.md#when-a-refresh-runs) says.

### Memory kept for higher-ranked work

While an entry waits for memory, nothing of another priority or tier that it
outranks starts if it needs measured memory. For example, a new agent waits
for the CI job of an open PR, and an `urgent` launch goes ahead of finishing
work of a lower priority.

Work of the same priority and tier that fits starts ahead of the waiting entry
until that entry has waited 15 minutes. Each entry it holds back waits with
`memory_kept`, which names it.

### Upstream limits

The [proxy](agents.md#retrying-proxy) of a running agent may find its tool's upstream
rate-limited or out of quota. Queued agents of that tool then wait with
`upstream_limit`, and others start ahead of them. They wait until:

- the reset the upstream names, or, without one, 15 minutes after the proxy
  last met the limit, or
- a request through any proxy of that tool succeeds.

The daemon log names each limit a proxy reports, with its execution, kind and
end, and when it lifts.

### Where an admitted agent runs

An entry queued from another host's pane, block or tab opens a background tab
or window where it was queued, as [Queued starts](terminals.md#queued-starts)
says. Otherwise it runs where
`terminal.default_host` puts it: by default, a detached zmx session that
`shoal attach` reaches.

### Failed starts

An admitted agent keeps its slot until it registers. A start that fails, or
whose agent does not register within two minutes, frees the slot. Its launch
stays on its space, a workspace or a chat, as a `failed` entry with its error.

- `shoal queue` and `shoal inbox` list it.
- Nothing admits it, and it keeps no space from cleanup.
- Its failure reaches the agent of the nearest space above with one, or else
  your alerts. It names `shoal resume <space>`, which queues the launch again
  exactly as it was, and `shoal rm <space>`.

A new launch in the space replaces the failed entry. The space's removal drops
it.

### Start now

`--now` starts the agent at once, regardless of the limits, once its
workspace is prepared. `--focus` does the same for a scoped caller, as its
space's [`limits` permission](modes.md#permitted-actions) allows.

A start you wait for also starts its agent at once:

- one that `--focus` focuses, or
- an agent started from a terminal without an issue or arguments, which the
  `focus` setting does not send to the background.

Each of these starts still takes a slot. With `--add-blocker`, the agent
instead waits for its blockers.

## One agent per workspace

A workspace has one agent of its own. The daemon refuses a launch in an
existing workspace, from any caller, while that workspace's agent runs or
waits in the queue, also while a blocker holds it back. The refusal names
`shoal attach`, `shoal restart`, `shoal stop` or `shoal queue`, as fits.

A stopped agent awaiting `shoal resume` also refuses a launch. So does an
asleep agent whose restore Shoal queued on its own. The refusal names
`shoal resume` and `--replace`. Without `--replace`, a start on a terminal
asks whether to resume the stopped agent, replace it or cancel.

A start with `--replace`:

1. stops the wrapper that waits to restore an asleep agent,
2. discards the agent, with its last sessions and stopped commands, as
   `shoal restore --discard-agent` does, and
3. queues its own agent.

`--replace` applies only to a start of a workspace's own agent: with `--in`,
or on a branch or issue the workspace has. A chat in the workspace's worktree
refuses it. A scoped caller replaces only the agent of a space below its own,
as it discards one.

A last session, whose agent exited on its own, does not count: the new agent
replaces it. Chats in the workspace's worktree never count.

## Restores

An agent's automatic restore waits in the same queue as launches, ranked with
them. It waits from when the agent's processes stop, whether its wrapper or
the daemon restores it. [Automatic restore](load.md#automatic-restore) and [sleep](sleep.md)
say when an agent stops and what brings it back.

### What a restore waits for

A restore waits for what the agent's stop calls for:

| Stop | The restore waits for |
|---|---|
| The space was blocked | nothing blocking the space |
| Idle | an update for the agent |
| Load or disk space, mid-turn | the recovery interval's healthy readings |
| Any other stop mid-turn, such as a signal | a free slot, or room for its load |

The restored agent takes the slot over once it runs.

### Restores that wait for an update

A restore that waits for an update holds back nothing behind it. Once the
agent's processes are proven stopped, it holds no slot until an update
arrives. It then needs a free slot, or room for its load, as a launch does.
With disk protection on, it also needs `disk.cleanup_free_gib` available, but
does not wait for healthy readings.

### Restores that wait for healthy readings

A restore that waits for healthy readings keeps its place without holding back
the entries behind it. While its wrapper waits, it keeps the agent's slot:

- Under a fixed limit, it takes that slot back on a full machine.
- Under `dynamic`, it still needs room for its load.
- A lowered limit holds it until other agents leave.

A stop, a reload or an unhealthy reading before the restored agent starts
returns the restore to its place.

Stopping the workspace removes the restore's entry and keeps the recovery
record. So does a restore that Shoal refuses.

### Waiting wrappers

A waiting wrapper restores its agent in place. A wrapper can exit after its
restore is admitted and before the agent starts, as one whose binary an
upgrade removed can. The restore then stays in the queue, as a signal stop's
restore does, and opens where the agent ran.

A daemon restart or a lost connection keeps the order. Once a waiting
wrapper's execution has ended without reattaching, the daemon starts the agent
as it starts a signal stop's restore.

`shoal resume` of an agent whose wrapper waits uses that entry. The restore
keeps its place and waits only for a heavy slot. `--now` or `--focus` starts
it at once in its wrapper.

### Restores without a wrapper

A restore that `shoal resume` or a signal stop queues joins the back of the
queue and waits as above. `shoal resume` makes it wait only for a heavy slot,
and it keeps its place.

The daemon starts such a restore as it starts a queued launch. It runs:

1. in the place the agent stopped in, once that place is back at its prompt,
   where the place's host keeps it after its agent;
2. otherwise in a background place of the host it stopped in, where that
   host's config lets it open places, such as `terminal.herdr.new_tab`;
3. otherwise where `terminal.default_host` puts it.

A restore whose agent has not started within two minutes records a
`queue_failed` alert and leaves the agent stopped.

Stopping the workspace or marking its assignment done cancels a pending
restore. Resuming or forgetting the agent another way removes it.

## Blockers

`--add-blocker` names what blocks a launch. Repeat it for more. It takes:

- a workspace or chat of the repository, by ID,
- a queued agent, by its entry's ID, or
- an issue, by number or URL. A bare number is always an issue.

An issue launch is also blocked by the open issues its forge records as
blocking it: Forgejo and GitHub issue dependencies and GitLab "is blocked by"
links. When the forge cannot list them, such as with that feature turned off,
Shoal warns and they block nothing.

A blocked launch queues even without a limit. It starts once:

- every workspace that blocks it is done at its current HEAD,
- every chat is done,
- every queued agent has opened its workspace and that workspace is done, and
- every issue is closed, or done in a workspace of the repository that links
  it.

A removed space or queued agent no longer blocks. A commit after `shoal done`
blocks the launch again. Shoal reads issue state every 30 seconds.

A new branch then starts from the done blocker's branch and stacks on that
workspace, as with `--base`. The agent so builds on the finished work before
it merges. With several done blockers, the branch starts from the one whose
HEAD contains the others. While they are on separate branches, the launch
waits until all but one have merged and their workspaces are gone.

`--base` and an existing `--branch` keep their own starting point. `--now` and
`--focus` cannot wait for blockers.

## List the queue

`shoal queue` lists the entries of every repository, or of one with `--repo`.
Positions count waiting entries across the machine. Each line starts with the
entry's ID, which `queue start` and `queue cancel` take. Then it shows:

- the state: `queued, position N` or `starting`;
- for a waiting launch, `blocked by` and what blocks it;
- for a restore of an idle agent, `after an update`;
- for a waiting entry, `held by` and what held it back at the last admission
  pass.

What holds an entry back is one of:

- memory, with what the entry needs and what is left,
- memory kept for a higher-ranked entry, which it names,
- low disk space,
- `machine.max_heavy_agents`, `machine.max_jobs` or `agent.max_heavy_running`,
- a full `runner.permit` pool,
- a critical or missing reading,
- a busy CPU, with its percentage and the level a job waits at.

The stagger between starts is too brief to show. A restore shows its agent,
and a command its `shoal run` command.
`shoal list` shows the entries after the workspaces.

On a terminal, `shoal queue` picks a waiting entry. It then starts it, with or
without attaching to the agent it starts, or cancels it.

### The queue as JSON

`shoal queue --json` gives each entry with:

- `kind`: `launch`, `restore`, `job`, `command` or `seed_build`;
- for a restore, its stopped `execution`; for a job, its `job` with its run;
  for a command, its `command`;
- `blockers` while any remain, among them the setup of the workspace it opens;
- `waits_for_update` for a restore of an idle agent;
- `held_by`, with its `reason`:
  - `memory`, with `needed` and `left` bytes,
  - `memory_kept`, with the higher-ranked entry's name in `behind`,
  - `disk`, `max_heavy_agents`, `max_jobs`, `repository_limit`, `permits` or
    `reading`,
  - `ranked_behind`, with the number of machines ranked `ahead` for a job,
  - `cpu`, with the `busy` and `level` percentages,
  - `upstream_limit`, with the `limit`, `rate_limited` or `exhausted`, and the
    Unix-seconds `until`;
- for a waiting entry, its `rank`, with its `priority` and `tier`. For each it
  took from other work, `priority_from` or `tier_from` gives that work's space
  number or launch name in `from` and the `dependency`: `blocked`, `parent`,
  `thread`, `stacked`, `lease` or `serves`;
- `base`, once a launch starts on a blocker's branch.

`shoal list --json` stays a list of workspaces.

## Start or cancel an entry

`shoal queue start` starts a waiting entry now, regardless of the limits, as
`--now` does. It takes the entry's ID or, for a queued restore, its space's
number or ID. It also restores an idle agent that waits for an update. It
refuses:

- a launch still waiting for the work whose branch it builds on,
- a restore whose agent's processes have not stopped yet.

With `--attach` on a terminal, it then waits for the session the agent runs
in, and attaches to it as `shoal attach` does. It waits up to ten minutes,
since setup comes first. Only a launch or a restore starts an agent to attach.

`shoal queue cancel` removes a waiting entry. A cancelled restore stops the
waiting wrapper as `shoal stop` does, and leaves the agent for
`shoal resume`. A cancelled command exits, naming the cancel, and a cancelled
seed build waits for the next check. `shoal stop` of a space also cancels the
commands waiting to run there. `shoal rm` also removes a waiting launch by its
ID. A starting entry belongs to its workspace.

### Scope

Every caller sees every entry. A scoped caller cancels the entries its scope
reaches:

- those of the spaces below its own,
- a CI job's, through the spaces its run is for,
- a new space's launch, through the parent it goes below.

A scoped caller starts an entry its scope reaches as its
[`limits` permission](modes.md#permitted-actions) allows.

## Priority

`shoal priority` sets a space's priority: `low`, `normal` or `urgent`, or a
number from 0 to 100. The priority ranks the space's queued work and the work
it waits on, as [Queue order](#queue-order) says. It lasts until changed,
through `shoal done` and conversion.

`shoal start --priority` sets it on the new space, and the launch ranks with
it while queued.

A space's agent sets the priority of its own space and those below, at most
to its own space's. What it starts without `--priority` takes its own space's
priority when that is below `normal`. You and a master session set any.

## Queue order

Waiting entries start by priority, highest first. A space's priority, from 0
to 100, is:

1. the one set on it,
2. else the highest its issues' labels map to under `[priority.labels]`,
3. else 50.

A queued launch has the priority it was queued with, or else the highest its
issue's labels map to, until its space exists.

### Schedules

Within a priority,
[`machine.schedule`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
may put other work first:

- `speed`: the work with the longest chain of work waiting on it, counted
  through the [waits below](#rank-taken-from-other-work).
- `cache`: restores of agents whose prompt cache, as their hooks last reported
  it, has not expired, the latest to expire first.

Neither passes over an entry that has waited an hour. After that, and under
the default `balanced`, entries start by tier.

### Tiers

Tiers start highest first:

1. `finishing`: the agents of a space with an open PR it opened or was
   assigned, or with a ready mark, and CI jobs on PRs and on the default
   branch.
2. `started`: restores of agents that have worked in their space.
3. `new`: other launches and CI jobs.

An entry rises one tier for each full hour it waits, but never to a higher
priority.

### Within a tier

Within a tier, entries start in this order:

1. A launch of a new workspace whose issue shares fewer areas with the
   workspaces of its repository.
2. The space with the larger share of its plan steps done.
3. The space created earliest.
4. A CI job or review ahead of an agent of the same space.
5. The entry that arrived first.

An area is a label with the
[`[issue] area_prefix`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#issues).
A workspace holds the areas of its assigned issues and PRs until it is
removed. A chat holds none.

### Rank taken from other work

Work that other work waits on takes that work's priority and tier where they
are higher. It takes each on its own, through any chain of waits. Work waits
on:

- what blocks a queued launch or a space's progress: for an issue, the spaces
  working on it; for a PR, the spaces that opened or were assigned it; for a CI
  run, its queued jobs and those of its workflow on the default branch;
- a workspace's children;
- the holder of a thread another space opened that waits;
- the workspace a stacked one builds on;
- the spaces holding a lease another space queues for.

A chat passes its children only a priority of its own: one set on it, from its
issues' labels, or passed down to it. Its children otherwise compete with
every other worker.

A CI job ranks with the highest-ranked space its run links. It takes that
space's priority even when it is below 50. A review ranks with the workspace it
reviews.

`shoal queue` shows each waiting entry's priority and tier. For each it took
from waiting work, it shows that work and why, as in `priority urgent (from 7,
its parent), finishing (from 12, blocked on this)`. It then shows each area a
launch shares with the spaces holding it, as in `sharing area/daemon: 12, 15;
area/cli: 3`.
