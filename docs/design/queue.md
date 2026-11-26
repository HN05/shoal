# Agent queue

This file covers the agent queue: admission under the machine's limits,
restores, blockers and the order `shoal-schedule` gives its entries.

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
admits one agent; critical readings admit none. Admission follows the rank:
while an entry waits for memory, nothing of another priority or tier that
it outranks and that would take memory starts, so work close to finished,
such as the CI job of an open PR, is not passed over by new work, which
would then hold the memory for hours. This holds machine-wide, since memory
is the machine's, and for every entry, launches a coordinator queues
included, since it decides who takes the machine's resources, not what work
would conflict. Slots and permits keep the rank without help: a later start
that would take one finds none free either. Within one
priority and tier, work that fits starts ahead of work that does not, so the
machine stays busy; once that work has waited 15 minutes, a fixed value,
nothing it outranks starts until it fits, so a large start is not passed
over for good. `shoal queue` names the entry an entry waits behind. An AI tool whose upstream a proxied agent finds
rate-limited or out of quota holds that tool's queued agents, which would only
fail, while other tools' start; a limit without a named end lifts after 15
minutes, a fixed value, so a new agent finds out whether it still stands, and
any limit lifts once a request through any of the tool's proxies succeeds,
since the agent that met it may never send another. A limit lasts as long as
the refusal names: Anthropic's usage window reset counts only when the
response says the usage limit refused it, since every response names it. Dynamic is machine-wide, so a
repository's `dynamic` adds no limit. A positive number fixes a repository's
limit and `none` disables it. The machine's two limits take a number from 0
instead, and no `none`: a machine that should run no heavy agents, such as a
hub box, or no CI jobs sets 0, and one that wants no limit a large number,
which still keeps the disk rule below. Fixed limits keep dynamic admission's disk rule, since an agent
started on a nearly full disk is soon stopped by disk protection. Per-agent
memory profiles remain a proposal. CI jobs go through the same admission and
order: `machine.max_jobs` limits them apart from agents, and under `dynamic` a
job is expected to use the 95th percentile of the peaks of its latest 30 whole
runs, whatever their age, interpolated between the two nearest, or an agent's
before it has run, and takes the one staggered start an agent would. A job
repeats far more closely than an agent, but not always: a run that builds
nothing peaks far below one that builds everything, and one run can peak far
above the rest. A short window, such as the latest 5 runs, can fill with runs
that built nothing, and the next full run then finds no memory reserved; the
highest peak over a period lets one outlier set the cost for all of it. With
fewer than about 20 runs the percentile comes out close to the highest peak, so
a new job starts cautious. A run cut short, as by load protection or the
user, used less than a whole one, so the estimate leaves it out, and reads the
highest peak of such runs only while the job has no whole run, as a lower
bound: each lower estimate would admit the job where it was stopped again. A
run whose process the kernel's OOM killer killed needed more than it got, so it
counts as a whole run at 1.2 times its peak, the bump Kubernetes' vertical pod
autoscaler uses. A peak counts what the job swapped out at full size, since
without the pressure that swapped it out it would have stayed in memory. A runner that ended without taking its job, however it ended, keeps
no run: it measured nothing of the job. One table keeps the runs of every kind
Shoal measures, by repository, kind and key, a CI job's key being its workflow
and job name, and forgets a key once it has not run for 30 days.
A job reaches its peak late,
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
arrives; until then it also keeps its workspace from no cleanup, unless a
`shoal sleep` timer or a turn retry will wake it with work. Cleanup's removal
cancels the restore and keeps the agent stopped for `shoal restore`. Woken, it
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
