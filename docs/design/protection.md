# Overload protection

This file covers how the daemon protects the machine's memory, CPU and disk, by
warnings and by stopping work in one order.

Overload protection is configured machine-wide: memory is enabled by default,
CPU is opt-in and requires sustained aggregate busy time, which leaves out niced
time: more work on a busy CPU only runs slower, and work at low priority, such
as CI jobs or the user's own niced work, takes only cores nothing else wants.
One `load` setting (`light`, `balanced`, `full`) sets the default CPU and memory
thresholds, so a user chooses how hard to load the machine without tuning each
percentage; explicit thresholds override it. On Linux, memory protection acts on
stall time, not only on available memory: with swap, the kernel keeps
`MemAvailable` just short of any threshold while swap fills and the machine
thrashes, so the threshold came with the OOM, if at all. Pressure stall
information is the main signal; a measure of RAM and swap on disk in use
together is the backstop, for an allocation too fast to stall first and for
kernels without it. Zram swap is not capacity: RAM holds its compressed pages,
so counting it as memory would put the backstop's level past the point where the
machine stops responding.
Either signal counts: critical when either is, safe only when both are. Stall
stops work only once it has held, so one burst stops nothing. Its window ends
only at a reading below the stop level, never because a sample came late: the
kernel's ten-second average stays valid however late Shoal reads it, and a
thrashing machine is where samples come late. A late sample does end the
windows of CPU use and of health, since CPU use is a delta between samples and
a starved daemon is no healthy one. Protection stops
work in one stop order, by what a stop loses, which `shoal-schedule` computes
beside the queue order from the same snapshot, so one policy defines both: CI
jobs first, newest first, since a rerun loses nothing and the newest has done
the least; then demos, which Shoal restarts; then connected tracked agents,
stopped through their execution wrappers, preserving work and leases. An agent
comes only after every running agent below it in the space tree, light ones
included, so a coordinator never stops while its children work. Among the agents
free to go, heavy ones go before light ones, then the queue's rank reversed: the
agent the user interacted with longest ago, or never, first, since the user is
likely working with the one they touched last, then idle ones, since an idle
agent loses no turn, and working ones only when no idle one is left, each the
lowest priority and rank first, so the work the queue would start last gives way
first; within a space the agent idle longest, or the newest working one. The
daemon, places and workers are never in it. Each consumer takes the first entry
that frees what it needs: load the first holding memory worth freeing, passing
over a light agent or a space with negligible memory for its own sake; disk
every agent; making room an idle agent with a slot and no running agent below
it, since putting an idle agent to sleep must never stop working ones. Stopping
an agent stops every running agent below it first. A CI job load stops ends as
one whose runner outlived its run does and reruns on its forge once its run has
finished, whatever this machine's readings: the forge offers it to every runner,
each admitting it through its queue's normal admission, so load never costs a
job, only time. On Linux the kernel's OOM killer follows the same order: it
picks its victim by `oom_score_adj`, which left alone put the daemon's service
at 200 above agents' builds at 100, so it killed a small shoal process before a
large rustc. The daemon maps places in the order onto scores from 900 down to
200 and lowers its own to the floor its service manager allows without
privilege, which differs between machines, so the unit cannot state one; a CI
job's scope has the kernel kill the whole job, which fails anyway, and so frees
all of it. `memory_limit` caps Shoal's work, not the machine: on Linux
the kernel enforces it on `shoal-work.slice`, with `MemoryHigh` at the limit, so
work above it is throttled, reclaimed and swapped out before anything dies, and
`MemoryMax` a twentieth above it as the last resort, whose kills stay inside the
slice, so the desktop and the daemon keep their memory whatever work does. The
daemon sets both at runtime rather than in a unit file, so the cap follows
`shoal config reload` and nothing outlives the boot. On Linux, where a machine
is protected only under a limit, `load` sets one where the config leaves it
out, a share of RAM that leaves room for zram, whose compressed pages RAM holds
outside the slice; `"none"` is the only way to run without one. macOS has no
default, since its kernel enforces nothing and protection already reads its
own memory pressure, so a default there would only make admission count less
memory; nor has a development build, which leaves the slice to the installed
Shoal. Protection and admission
count the limit as the machine's memory, and read the slice's own stall, so
Shoal warns and stops work before the kernel throttles it; the slice's memory
leaves out its page cache, which throttling reclaims first and which would
otherwise read as full. macOS has no such control, so there the limit holds only
through protection and admission, on the usage Shoal samples. The wrapper, not
the daemon, reads its command's scope, since it learns of the command's end
first, and polls it rather than watching it: a scope the kernel kills whole is
gone in moments however it is watched, so a SIGKILL that comes with a kill
counted on the slice since the last read names the kill instead. A scope's peak
is read as what it holds beside its page cache, not from `memory.peak`, which
counts the cache a build reads and would make every job look as large as the
files it touched. Whether a terminal shows an agent plays no part, since a host
such as Herdr may count every agent as shown and a user who attaches may walk
away. An idle agent with a running subagent, which its hooks report, or
processes outside its process group counts as working, since their completion
may start its next turn, and so does one without turn reports. A subagent whose
stop is lost counts until the agent's session starts again or it exits, or,
where the agent's proxy has passed its subagents' requests in the current
session, until none has for an hour, which leaves room for a long tool call. Its
hook waiting for the next update runs there too, marked so that it does not
count. Once the wrapper reports the stop, the daemon also stops the agent's
identity-verified processes outside its process group, such as background shell
commands, since automatic and manual restores both require the execution proven
stopped. When the proof fails, the waiting wrapper stays connected and restores
once it holds, instead of leaving an unknown execution.

The daemon also samples what each workspace's processes use, attributed by the
workspace identity in their environment, so the user can see which workspace
loads the machine; admission learns a settled agent's memory from it, and
protection and warnings do not use it yet. A workspace's containers count in its
usage, read from the runtime's stats by the project name Shoal gave them, so a
settled agent's memory includes the services its work started; other processes
that leave the workspace's environment, such as work handed to a shared server,
count for the machine only. Warnings come first: below each enabled protection's
stop threshold, the daemon queues an alert for every space with a running agent,
so agents know of the load before anything stops. A memory or CPU warning is a
heads-up that asks for no action, so agents keep to their work. Warnings repeat
per signal and space only after an interval and never stop, clean up or notify.
A memory or CPU warning never wakes an idle agent, since it gives the agent
nothing to do and a woken turn costs tokens; it waits for the agent's next
update or turn. A disk warning wakes a workspace's agent, since deleting files
frees space, but only in its own worktree and only whole build outputs: other
workspaces' files and the preset files beside them are in use by agents it
cannot see, and part of a build's output leaves records the build tool takes as
fresh for files that are gone. A chat's agent, with no worktree, has nothing to
delete, so its disk warning waits for its next turn too. Their thresholds are
not validated against stop thresholds, so lowering a stop threshold never
invalidates an existing configuration. Disk protection reads available space on
filesystems holding workspaces or daemon state. Below its cleanup threshold it
removes idle cleanup candidates there without their idle delay, through the same
removal path and repository settings. When that cannot keep space above its stop
threshold, it stops every agent as overload protection does and every other
tracked execution as manual stop does, since any of them may be writing. Agents
restore once space reaches the cleanup threshold, so restores cannot refill the
disk at once; commands stay explicit. A failed reading authorizes nothing on its
filesystem and blocks recovery; readings of other filesystems still protect
them.
