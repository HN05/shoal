# Agent queue

This file covers the agent queue: admission under the machine's limits,
restores, blockers and the order `shoal-schedule` gives its entries. [Agent
queue](../reference/queue.md) says what the queue does and what `shoal queue`
and `shoal priority` show; this file gives the reasons.

## The daemon admits

Each machine's daemon admits the agents, reviews and CI jobs that start there.
Concurrent starts and the dispatcher thus share one count.

Slots derive from what the daemon tracks: connected agents, executions that
await reattachment, resume records and starting entries. Done workspaces hold
none. An agent therefore counts however it started.

The queue holds agents, not workspaces. A new workspace is prepared once its
launch is recorded, before its agent is admitted. A waiting agent thus waits in
a workspace that exists.

## Weight

A mode's `weight` alone decides how its agent takes capacity, whatever the
kind of space it runs in. A heavy chat uses the machine as a heavy workspace
agent does, so it waits for a heavy slot and holds one in the same way.

A light agent counts toward no limit. It starts at once, holds no slot and is
never stopped for load or for being idle. A mode whose agents use little of the
machine, such as a plan run in a workspace, is thus not held up while heavy
work fills it. The built-in modes that start chats are light, and they stay
light in a workspace.

Some conditions hold every start, light ones too: a critical or missing
reading, low disk, and an upstream limit of the agent's tool.

A chat without a repository resolves its mode, and so its weight, in the global
config. Its slot counts toward `machine.max_heavy_agents` alone.

A heavy chat that finds no free slot starts as a new workspace does. The daemon
records the chat at once, below where it goes, so the caller can address it.
Its launch waits on it.

## One-shot runs

A one-shot run, such as a review, is short and holds no slot. No count limits
reviews. Its memory still counts toward dynamic admission, and it waits for
memory and disk as a start does.

Its admission is a request the daemon keeps in memory. The request ends with
the run, or with the daemon when the run's processes could not be proven
stopped. None outlives a daemon restart.

## Limits

`machine.max_heavy_agents` limits the machine, and the repository option
`agent.max_heavy_running` limits each repository. The repository option takes
a global default like any other option, so one name never means both limits.
`dynamic` is machine-wide, so a repository's `dynamic` adds no limit.

The machine's limits take no `none`. A machine that should run no heavy agents
or no CI jobs, such as a hub box, sets 0. One that wants no limit sets a large
number, which still keeps the disk rule.

A fixed limit keeps dynamic admission's disk rule. An agent started on a nearly
full disk is soon stopped by disk protection.

## Dynamic admission

`dynamic`, the default, admits from measured memory and disk instead of a slot
count. The machine thus runs as many agents as it can without running out of
either.

CPU reserves nothing. Builds and tests use every free core, so a run's peak CPU
is not its cost. More work on a busy CPU only runs slower. Memory and disk do
not stretch, and they are what break a machine.

### New agents

Agents start one at a time, with a stagger between starts. An agent reads files
before it edits and compiles, so its memory grows after it starts. A recent
start therefore reserves what a settled agent's workspace uses until its own
memory is measured. Admission learns that as the settled agents' average, with
a floor.

### The target

Admission aims at the warning thresholds that `load` sets. The user thus picks
how hard to push the machine, rather than the windows and intervals of the
mechanism. Memory protection stays the safety net when settled agents grow
later.

An idle machine always admits one agent. Critical readings admit none.

Per-agent memory profiles remain a proposal.

## Memory kept for higher-ranked work

Admission follows the rank. While an entry waits for memory, nothing of
another priority or tier that it outranks starts if it would take memory. Work
close to finished, such as the CI job of an open PR, is thus not passed over by
new work, which would then hold the memory for hours.

This holds machine-wide, since memory is the machine's. It holds for every
entry, launches a coordinator queues included, since it decides who takes the
machine's resources, not what work would conflict.

Slots and permits keep the rank without help. A later start that would take
one finds none free either.

Within one priority and tier, work that fits starts ahead of work that does
not, so the machine stays busy. Once the waiting entry has waited 15 minutes,
nothing it outranks starts until it fits. A large start is thus not passed over
for good. The wait is a fixed value.

## Upstream limits

A proxied agent may find its tool's upstream rate-limited or out of quota. The
queue then holds that tool's queued agents, which would only fail, and starts
other tools' agents.

A limit lasts as long as the refusal names. Anthropic's responses all name a
usage window's reset, so that reset counts only when the response says the
usage limit refused the request.

A limit without a named end lifts after 15 minutes, a fixed value. A new agent
then finds out whether the limit still stands. Any limit lifts once a request
through any proxy of the tool succeeds, since the agent that met the limit may
never send another.

## CI jobs

CI jobs go through the same admission and order as agents.
`machine.max_jobs` limits them apart from agents. No count limits jobs by
default, since the memory reservation already admits a job only where its whole
expected memory fits. A job takes the one staggered start an agent would.
[Job admission](../reference/ci.md#job-admission) gives the rules.

### Expected memory

A job's expected memory is a high percentile of the peaks of its latest whole
runs, whatever their age. A job repeats far more closely than an agent, but not
always. A run that builds nothing peaks far below one that builds everything,
and one run can peak far above the rest. A short window can fill with runs
that built nothing, and the next full run then finds no memory reserved. The
highest peak over a period lets one outlier set the cost for all of it.

With fewer than about 20 runs, the percentile comes out close to the highest
peak. A new job thus starts cautious.

A run that a stop cut short used less than a whole run, so the estimate leaves
it out. Its peak counts only while the job has no whole run, as a lower bound.
Each lower estimate would admit the job where it was stopped again.

A run that the kernel's OOM killer killed needed more than it got. It counts as
a whole run at 1.2 times its peak, the bump Kubernetes' vertical pod
autoscaler uses.

A peak counts what the job swapped out at full size. Without the pressure that
swapped it out, it would have stayed in memory.

A runner that ended without taking its job keeps no run, since it measured
nothing of the job.

One table keeps the runs of every kind Shoal measures, by repository, kind and
key. A CI job's key is its workflow and job name. The table forgets a key that
has not run for 30 days.

### Reservation

A job reaches its peak late, while it builds and runs tests. Until it ends, it
therefore reserves what it has yet to use: its expected memory less what its
workspace uses now. Counting it only while it is young would let each pass
admit another job against memory the running ones will soon need.

### Busy CPU

A job beside running ones waits while the CPU has stayed at or above the CPU
warning level over its window. Further jobs on a saturated CPU only slow every
job down. The reading includes niced time, since jobs run niced.

The first job never waits for the CPU, so agents' load cannot starve jobs.
Agents never wait for it.

The reading comes from the overload monitor's samples, taken while dynamic job
admission applies. No second sampler runs.

## Restores

An automatic restore is a typed queue entry, ordered with launches. It is
queued once the stopped agent's processes are proven stopped.
[Agent stops and restores](agent-lifecycle.md) says when an agent stops.

### What a restore waits for

One daemon operation decides what a queued restore waits for, from the agent's
recovery record, whoever carries the restore out. The two thus cannot drift
apart. [What a restore waits for](../reference/queue.md#what-a-restore-waits-for)
lists each stop's wait. A restore the user asked for waits for a free slot.

The daemon marks a load or disk stop mid-turn in the recovery record before the
stop. Its restore waits for the recovery interval's healthy readings, and one
such restore starts per interval.

### Restores that wait for an update

An agent stopped between turns has nothing to do until an update arrives. Its
restore therefore holds no slot and holds back nothing until then. It keeps its
workspace from no cleanup, unless a `shoal sleep` timer or a turn retry will
wake it with work. Cleanup's removal cancels the restore and leaves the agent
stopped for `shoal restore`.

Woken, the restore is admitted as a launch is, once free disk space has
recovered. Later stops do not take back an update's admission.

### Restores that wait for healthy readings

A restore that waits in its wrapper for healthy readings keeps the agent's
slot. A fixed limit thus lets it take that slot back rather than counting it
twice, which would deadlock a full machine of stopped agents. Dynamic admission
measures its memory as it does a new start's. Disabling recovery leaves the
agent for `shoal resume`.

Admission binds the recovery epoch. A stop, a reload or a reading that turns
unsafe advances the epoch. Any of them before the agent restarts returns the
restore to its place.

### Waits that hold back nothing

An entry that waits for anything besides a slot, such as healthy readings, an
update or a blocker, keeps its place. It holds back no work queued behind it.
The stopped agent's slot still counts, and dynamic admission measures memory.
The limits, not the order, thus keep new starts from undoing the protection.

### Who carries a restore out

Admission decides first, and the carrier second. The agent's waiting wrapper
carries the restore out while it holds its link. It takes the link once the
agent's processes are proven stopped, and admission hands it the restore
through that link. Otherwise the daemon starts the agent in a new place.

A wrapper whose execution has ended can no longer reattach. The daemon then
takes its restore over rather than dropping it. A wrapper whose session ends
while it waits leaves its restore to the queue, as a running agent's wrapper
does.

The entry is keyed by its execution. A reattached wrapper thus keeps its place,
and a daemon restart cannot restore the agent twice. The slot passes to the
agent when its restored process registers.

### Restores `shoal resume` queues

`shoal resume` queues restores, since starting every stopped agent at once
would overload the machine. `--now` starts them at once, as `start --now` does.

No wrapper waits for such a restore. The daemon starts it through the queue
start worker, as it starts a launch in an existing workspace. Its stopped agent
holds no slot while it waits, so stopped agents past a limit cannot keep each
other from restoring.

The entry names the execution without referencing its row, which ended with
the agent. It leaves the queue once the recovery record or last session is
gone. A last session's admitted restore counts as a launch, since it has no
slot to take over.

## Starting entries

A starting entry keeps its slot until its agent registers. A short grace period
covers handoffs to the place a host opens, and daemon restarts. A dead start
thus cannot hold a slot.

The daemon starts admitted entries through a detached CLI worker that carries
the queuing terminal's origin, since terminal launch logic stays in the CLI.
[Where an admitted agent runs](../reference/queue.md#where-an-admitted-agent-runs)
says where it opens.

## Blockers

Admission passes over a launch until its
[blockers](../reference/queue.md#blockers) clear.

A space clears once it is done at its current HEAD, not once it merges. A new
workspace's preparation waits for its blockers too, then branches from the done
blocker and stacks on it. Blocked agents thus start before review, and the
restack on merge carries them to the default branch. A commit after `shoal
done` is new work and blocks them again.

One launch builds on one branch. Done blockers on separate branches therefore
wait until all but one merge.

A launch that blocks another becomes its workspace once it opens one. Removed
work clears, since nothing is left to wait for.

## Order

The queue starts the work that gets the most done with the fewest agents
running. Work close to finished goes before new work, and work that other work
waits on goes before the work that waits.

### One policy owns the order

One ordering policy, a `Scheduler`, orders admission and the listing alike. It
reads a snapshot of the entries, the spaces and what waits on what, and nothing
else. It owns the whole order, so a policy is replaced without touching
admission. A policy is code in `shoal-schedule`, never a script or a loaded
plugin.

Admission, with the limits and the readings that measure the machine, stays
with each machine's daemon, the only one that measures it. No policy replaces
it, so a schedule changes only the order.

`shoal queue` starts and cancels entries, but it places none. A hand-placed
order would fight the rank. Positions count waiting entries machine-wide, as
admission does. For the same reason, every caller sees the whole queue, while
acting on an entry keeps the scope rules.

### Priority

A space's [priority](../reference/queue.md#queue-order) goes first, so the user
decides what matters. Lower-priority work waits while higher-priority work
keeps coming, as the user chose.

A space's agent sets at most its own space's priority, as it gives permissions.
A label's priority comes from the repository config, so the labels an agent
edits on its issues are not capped.

### Schedules

`machine.schedule` picks the policy. The built-in schedules share one ranking
and differ only in what goes first within a priority:

- `balanced` puts the higher tier first.
- `speed` puts first the work with the longest chain of work waiting on it, the
  critical path. A machine with room thus finishes sooner.
- `cache` puts first the restores whose prompt cache is still warm. An agent
  thus continues before its cached context costs a full read again. The cache
  that expires latest goes first, since it is surest to be warm when its agent
  starts.

After their lead, `speed` and `cache` follow `balanced`. An entry that has
waited an hour ranks as if it led as well as any other, so no schedule starves
work within its priority. Its tier then orders it among them.

### Tiers

Within a priority, [tiers](../reference/queue.md#tiers) rank work by how close
it is to finished. An hour's wait raises an entry one tier, so nothing starves
within its priority. It never raises a priority.

### Rank taken from other work

Priority and tier each flow along one dependency graph, from waiting work to
the work it waits on. Work that holds other work up thus runs first, so the
waiting work can go on. [Rank taken from other
work](../reference/queue.md#rank-taken-from-other-work) lists the waits.

A chat parent passes on only a priority of its own. An orchestrator's children
thus compete with every other worker.

CI jobs and reviews have no rank of their own beyond a job's tier. Each takes
the rank of the spaces it serves and goes ahead of their agents, which wait on
it.

### Areas

Within a tier, a launch of a new workspace whose issue shares fewer areas with
the existing workspaces of its repository goes first. Queued issues thus run
beside unrelated work rather than conflicting with it.

An area only orders. It never holds a launch while a slot is free, and every
area counts the same. The hourly rise keeps a launch that shares areas from
starving. Only new workspaces move, since other work's branch already exists.

### Why an entry waits

Each waiting entry carries what held it back at the last admission pass, so a
wait never goes unexplained. The stagger is too brief to report. The work an
entry waits for shows as its blockers, apart from what held it back.
