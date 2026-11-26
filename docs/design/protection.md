# Overload protection

This file covers how the daemon protects the machine's memory, CPU and disk:
the signals it reads, the warnings it sends and the order it stops work in.
[Overload protection and stops](../reference/load.md) says what each threshold
and stop does, and the
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#overload)
gives each option.

## Signals

Protection is configured for the whole machine. Memory protection is on by
default. CPU protection is opt-in.

One `load` setting, `light`, `balanced` or `full`, sets the default CPU and
memory thresholds. A user thus chooses how hard to load the machine without
tuning each percentage. An explicit threshold overrides it.

### CPU

CPU protection acts only on busy time that lasts. More work on a busy CPU only
runs slower, so a short peak is no reason to stop anything.

CPU busy time leaves out niced time. Work at low priority, such as CI jobs or
the user's own niced work, takes only the cores nothing else wants.

### Memory

On Linux, memory protection acts on stall time, not only on available memory.
With swap, the kernel keeps `MemAvailable` just short of any threshold while
swap fills and the machine thrashes. A threshold on it thus triggers with the
OOM kill, if at all.

Pressure stall information is the main signal. Memory in use, RAM and swap on
disk together, is the backstop. It catches an allocation too fast to stall
first, and it serves kernels without stall information.

Zram swap is no capacity, since RAM holds its compressed pages. Counting it as
memory would put the backstop's level past the point where the machine stops
responding.

Either signal counts. Memory is critical when either signal is, and safe only
when both are.

### Windows and late samples

Stall stops work only once it has held for its window, so one burst stops
nothing. Only a reading below the stop level ends that window, never a late
sample. The kernel's ten-second average stays valid however late Shoal reads
it, and a thrashing machine is where samples come late.

A late sample does end the windows of CPU use and of health. CPU use is a
difference between two samples, and a daemon starved of time is not healthy.

## The monitor

The monitor samples the machine and decides on an OS thread of its own, which
reads no store. On a thrashing machine the async runtime is starved too, and a
decision that waited on store reads came too late. The monitor hands each stop
and warning to the runtime without waiting on it.

The monitor hands over one stop at a time. Once a stop has stopped something,
the monitor waits out the cooldown and starts its windows again.

A stop that waited for the runtime acts on the pressure the latest sample
found. It stops nothing once a sample found no pressure, or once a reload
changed protection since that sample.

While memory is under pressure, the monitor logs the readings it decides on,
at a limited rate. The daemon log then traces how an incident built up.

## Stop order

Protection stops work in one stop order, by what each stop loses.
`shoal-schedule` computes it beside the queue order from the same snapshot, so
one policy defines both.

The order holds three kinds of work:

1. CI jobs, newest first. A rerun loses nothing, and the newest job has done
   the least.
2. Demos, which Shoal restarts.
3. Connected tracked agents. A stop goes through the agent's execution wrapper
   and keeps its work and leases.

The daemon, places and workers are never in the order. A one-shot run, such as
a review, stops before anything in it, as [Reviews](reviews.md) says.

### Agents

An agent comes only after every running agent below it in the space tree,
light ones included. A coordinator thus never stops while its children work.

Among the agents free to go, each rule decides only where the ones before it
tie:

- A heavy agent goes before a light one.
- The queue's rank decides in reverse, so the work the queue would start last
  gives way first:
  - The agent the user interacted with longest ago, or never, goes first. The
    user is likely working with the agent they touched last.
  - An idle agent goes before a working one, since an idle agent loses no
    turn.
  - The agent of the lowest priority and rank goes first.
- Within a space, the agent idle longest goes first, or else the newest
  working one.

The order reads the user's [interaction](agent-lifecycle.md#interaction), never
whether a terminal shows the agent. A host such as Herdr may count every agent
as shown, and a user who attaches may walk away.

### Idle and working

An idle agent counts as working while its work may start its next turn. That
holds while a subagent runs, as its hooks report, or while a process of the
agent runs outside its process group.
[Subagents](../reference/agents.md#subagents) says how long a subagent whose
stop is lost counts. The agent's hook that waits for the next update runs
outside its process group too, marked so that it does not count.

An agent without turn reports counts as working.

### What each stop must free

Each consumer of the order takes the first entry that frees what it needs:

- Load protection takes the first entry that holds memory worth freeing. It
  never stops a light agent for its own sake, nor a space whose processes use
  negligible memory at the latest usage sample.
- Disk protection takes every agent.
- [Making room](agent-lifecycle.md#making-room) takes an idle agent with a
  slot and no running agent below it. Putting an idle agent to sleep must
  never stop working ones.

Stopping an agent stops every running agent below it first.

## Stopping an agent

The wrapper ends the agent's process group. Once the wrapper reports the stop,
the daemon stops the agent's processes outside that group, such as background
shell commands, that it verifies by their identity. Automatic and manual
restores both need the execution proven stopped.

When that proof fails, the waiting wrapper stays connected. It restores the
agent once the proof holds, instead of leaving an execution nobody knows.

## Stopped CI jobs

A CI job that load protection stops reruns on its forge once its run has
finished, whatever this machine's readings. The forge offers the job to every
runner, and each runner's queue admits it as any other job. Load thus never
costs a job, only time. [Runner and triggers](runner.md) says how Shoal ends
the job without cancelling its run.

## OOM scores

On Linux, the kernel's OOM killer follows the stop order. The kernel picks its
victim by `oom_score_adj`. Left alone, that put the daemon's service at 200,
above agents' builds at 100, so the kernel killed a small `shoal` process
before a large `rustc`.

The daemon therefore maps the entries of the order onto scores from 900 down
to 200. It lowers its own score to the floor its service manager allows
without privilege. That floor differs between machines, so the service unit
cannot state it.

The kernel kills a CI job's scope whole. A job that loses a process fails
anyway, so killing all of it frees all its memory.

## Memory limit

`memory_limit` caps Shoal's work, not the machine. On Linux the kernel enforces
it on `shoal-work.slice`, where [Execution](execution.md#work-commands) puts the
work:

- `MemoryHigh` sits at the limit. Work above it is throttled, reclaimed and
  swapped out before anything dies.
- `MemoryMax` sits a twentieth above it, as the last resort. Its kills stay
  inside the slice, so the desktop and the daemon keep their memory whatever
  the work does.

The daemon sets both at runtime rather than in a unit file. The cap thus
follows `shoal config reload`, and nothing of it outlives the boot.

### The default limit

On Linux a machine is protected only under a limit. `load` therefore sets one
where the config leaves it out. The limit is a share of RAM that leaves room
for zram, whose compressed pages RAM holds outside the slice. `"none"` is the
only way to run without a limit.

macOS has no default limit. Its kernel enforces none, and protection already
reads its own memory pressure, so a default would only make admission count
less memory. A development build has none either, since it leaves the slice to
the installed Shoal.

### Protection under a limit

Protection and admission count the limit as the machine's memory, and read the
slice's own stall. Shoal thus warns and stops work before the kernel throttles
it.

The slice's memory leaves out its page cache. Throttling reclaims that cache
first, and counting it would make the slice read as full.

macOS has no such control. There the limit holds only through protection and
admission, on the usage Shoal samples.

## Work usage

The daemon samples what each workspace's processes use, found by the workspace
identity in their environment. The user can then see which workspace loads the
machine. Admission learns a settled agent's memory from these samples, and
load protection reads them to pass over a space that frees too little.

A workspace's containers count in its usage. The daemon reads them from the
container runtime's stats, by the project name Shoal gave them. A settled
agent's memory thus includes the services its work started.

Work that leaves the workspace's environment, such as work handed to a shared
server, counts only for the machine.

## Kills and peaks

The wrapper, not the daemon, reads its command's scope, since the wrapper learns
of the command's end first.

The wrapper polls the scope rather than watching it. A scope the kernel kills
whole is gone in moments, however it is watched. A SIGKILL that comes with a
kill the slice counted since the last read therefore names the kill.

A scope's peak is what it holds beside its page cache. `memory.peak` counts the
cache a build reads, and would make every job look as large as the files it
touched.

## Warnings

Warnings come before stops. Below each enabled protection's stop threshold,
the daemon queues an alert for every space with a running agent. Agents thus
know of the load before anything stops.

A memory or CPU warning is a heads-up that asks for no action, so agents keep
to their work. It never wakes an idle agent: it gives the agent nothing to do,
and a woken turn costs tokens. It waits for the agent's next update or turn.

A disk warning wakes a workspace's agent, since deleting files frees space. It
asks the agent to delete only in its own worktree, and only whole build
outputs:

- Other workspaces' files, and the preset files beside them, are in use by
  agents it cannot see.
- Part of a build's output leaves records that the build tool takes as fresh
  for files that are gone.

A chat's agent has no worktree and nothing to delete, so its disk warning waits
for its next turn.

A warning repeats per signal and space only after an interval. Warnings never
stop, clean up or notify.

Shoal does not check the warning thresholds of memory in use, CPU and disk
against stop thresholds. Lowering a stop threshold thus never invalidates
their configuration.

## Disk protection

Disk protection reads the space available on each filesystem that holds
workspaces or daemon state. A failed reading authorizes nothing on its
filesystem and blocks recovery. The readings of other filesystems still
protect them.

### Disk cleanup

Below its cleanup threshold, disk protection frees space in the order
[Disk cleanup](../reference/load.md#disk-cleanup) gives. It removes idle
cleanup candidates without waiting out their idle delay, through the same
removal path and repository settings as idle cleanup.

### Disk stops

When cleanup cannot keep space above the stop threshold, disk protection stops
every tracked execution. It stops agents as load protection does, and every
other execution as `shoal stop` does, since any of them may be writing.

Agents restore once space reaches the cleanup threshold, so restores cannot
fill the disk again at once. Other commands stay stopped until the user runs
them again.
