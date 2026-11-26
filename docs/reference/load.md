# Overload protection

This file says how Shoal protects the machine's memory, CPU and disk, and where
the work it starts runs.

At critical memory pressure, or sustained CPU overload where CPU protection is
on, the daemon stops one entry of the stop order, the running work in the order
a stop of it loses least, first first:

1. CI jobs this machine runs, newest first: a rerun loses nothing.
2. [Demos](demos.md), whose commands Shoal runs again.
3. Agents, a workspace's or a chat's, each only after every running agent
   below its space, so a coordinator never stops while its children work.
   Among the agents free to go, one of a mode that is not
   [light](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
   goes before a light one, then, in the [agent queue's](queue.md#queue-order) order
   reversed, the one you [interacted](sleep.md#interaction) with longest ago, or never, then
   an idle one before a working one, then the one whose space ranks last,
   lowest priority first; within a space the one idle longest, otherwise the
   newest.

It stops the first entry whose stop frees what the pressure presses: a CI job
always, so agents stop only once no job runs; a demo or an agent of a mode that
is not light, under memory pressure only where its space's processes use at
least 64 MiB at the latest usage sample, or have none sampled. A
light agent never stops for its own sake, but stopping an agent first stops
every running agent below it, light ones included. Under memory pressure,
before it would stop an agent, and when nothing in the order is left to stop,
it stops one simulator or containers an asleep agent
[keeps](resources.md#leases-while-asleep), the asleep agent ranking lowest in the agent
queue first, and stops an agent only once no asleep agent holds one. A running
[reviewer](reviews.md#local-reviews) stops before any of them. A stopped CI job ends as
one whose runner outlived its run does: Shoal stops its workspace's processes,
then its runner, and its end in the workspace's [log](log.md) says what it was
stopped for. On Forgejo, Shoal first cancels that job alone, so it shows as
cancelled and the run's other jobs go on; on GitHub, which offers no way to,
the job shows as failed. Shoal reruns the job on its forge as soon as its run has
finished there, whatever this machine's readings, and each runner's
[queue](queue.md) admits it as any other job; one whose run allows no rerun
within a day records a `runner_failed` alert.

On Linux the kernel's OOM killer follows the same order. The daemon lowers its
own `oom_score_adj` at startup to that of the service manager that started it,
the lowest it may set without privilege, which the places and workers it starts
keep, and every 10 seconds gives each entry's processes the score of its place,
900 for the first entry down to 200 for the last: those carrying its
execution's identity, short of its wrapper, and for a CI job every process of
the scope its runner runs in, whose kernel kill of one process ends the whole
job. A command a wrapper starts begins at 200, so what it starts scores above
Shoal's own before the daemon places it. Within one score the kernel takes the
largest process first.

An agent is idle when its
hooks last reported a finished turn, it runs no [subagent](agents.md#agent-state), and
none of its processes run outside its process group, such as a background
command whose completion would start its next turn; an agent without turn reports counts as working. On Linux,
memory pressure is critical when RAM and swap on disk in use together,
counting reclaimable cache in `MemAvailable` as free, pass
`memory.used_percent`, at once, or when every task has stalled waiting for memory for at least
`memory.stall_percent` of the last ten seconds, from the kernel's pressure stall
information, for `memory.sustained_seconds`. Only a reading below
`memory.stall_percent` ends that stretch, however far apart the samples
come. Stall shows a machine thrashing
while memory in use still looks short of full; memory in use catches an
allocation that reaches the kernel's OOM killer before any stall builds up, and
alone measures a kernel without pressure stall information. Zram swap counts
as neither memory nor free space, since RAM holds what it swaps out: the
compressed pages count as RAM in use. macOS uses native
critical pressure. While memory is past a warning or stop level, the daemon
log records the readings protection acts on: memory in use, the ten-second
stall of some tasks and of every task, and Shoal's work's against its
[memory limit](#work-slice). It logs a level again after 30 seconds, and a
higher one at once. The wrapper terminates the agent's process
group through its normal grace period. The monitor waits for the cooldown before
resampling and stopping another entry if pressure persists. It retains workspaces, execution recovery records and resource
leases and records an `agent_stopped` alert.
Ordinary commands, desktop handoffs and disconnected executions are not selected.
Monitoring does not guarantee that the OS will never reach its OOM limit.

## Work slice

On Linux every command an execution wrapper starts for work, an agent, a CI
job's runner, a demo, a setup or a `shoal run` command, and every one-shot
agent the daemon runs, a [reviewer](reviews.md#local-reviews) or the gate, runs in a
transient systemd scope of its own in `shoal-work.slice`, with
`OOMPolicy=continue`, wherever the wrapper runs, such as in a terminal host's
place: the command keeps the wrapper's terminal and its own environment,
including the runtime directory and bus its configuration sets. A wrapper
names its command's scope `shoal-<space ID>-<run>.scope` for the space whose
work it does. The daemon, places, workers and
wrappers, and the pushes and merges Shoal runs, stay out of it. A CI job's
scope ends whole with any process of it the kernel's OOM killer takes. Where
no scope can start, as without a user bus, from a login whose processes the
user's service manager cannot move, or before systemd 254, and in a
development build, the command starts directly in the wrapper's control
group.

With a [`memory_limit`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine),
which on Linux `load` sets where the config leaves it out, the daemon gives the slice `MemoryHigh` at the limit and `MemoryMax` a
twentieth above it, at startup and on `shoal config reload`, and lifts both
once the limit is removed: the kernel throttles work above the limit,
reclaiming its cache and swapping it out, and where that cannot keep it below
`MemoryMax`, its OOM killer kills inside the slice only, in the order of the
OOM scores above, while the daemon and the desktop keep their memory.
`memory_limit = "none"` removes the limit. A development build leaves the
slice to the installed Shoal, so `load` sets it no limit. The service manager keeps the cap until the next boot.

With [memory protection](load.md) on, overload protection and
dynamic admission count the limit as the machine's memory too, so Shoal warns
agents, holds back starts and stops work before the kernel throttles it: the work's memory against the limit, and on Linux
the slice's own stall, at the same levels as the machine's, press as the
machine's do, and a start fits only where both leave room. On Linux the
work's memory is what the slice holds that reclaim cannot free, its page
cache aside, with what it swapped out, and the memory the latest usage
sample counts for work running outside it, as a command whose scope could
not start; before the slice exists, and on
macOS, where the kernel does not enforce the limit, it is the memory the
latest usage sample counts for every space's processes and containers and
the one-shot agents' processes.
Without a current sample, or with one that could not read every workspace's
containers, there is no reading of the work: nothing starts and no stopped
agent recovers until there is, while protection still acts on the machine's
own memory. This holds under the limit `load` sets too. Once the samples
begin to miss containers, a `load_warning` alert names what could not be read
and how to clear it, or to lift the limit with `memory_limit = "none"`.

The wrapper reads its command's scope every second while it runs and once as
it ends: how many processes the kernel's OOM killer killed there, and the
most memory the scope held, counted as the slice's is. A CI job's peak raises
the memory its [cost](ci.md#job-admission) counts, and its kills add a `job_ended` entry
to its workspace's [log](log.md) naming them, such as a build's compiler
killed for memory. An agent's kills show in its `agent_exited` or
`agent_stopped` entry, as `oom_kills`, and in the alert its exit sends. A
scope the kernel killed whole is gone before its last read, so a command it
ended with SIGKILL counts a kill when the slice counted one since the
previous read.

When space available to unprivileged processes on a filesystem holding
workspaces or daemon state falls below `disk.cleanup_free_gib`, the daemon
deletes the copies of [files](files.md) fetched from other machines
there that nobody changed since, and the Xcode data there that removing a workspace of a repository using
the [`xcode` preset](setup.md#build-tool-presets) deletes, for every worktree in the
repository's directory that no longer exists,
removes the workspaces there that idle cleanup would remove, without waiting for
their idle delay, then empties [shared caches](setup.md#shared-caches) there nothing is
using, then deletes the [files](files.md) kept there for removed
workspaces and chats, oldest first and recording `results_deleted` alerts, until
enough space is available. A repository with idle
cleanup disabled keeps its workspaces. Removals record `workspace_removed`
alerts and the `disk_space` event cause; a pass that cannot free enough
space, or fails, repeats at most every 30 seconds. While space stays below
`disk.stop_free_gib` after cleanup, whether or not it failed, the daemon stops every running agent as
memory protection does, each after the agents below it, then every other tracked execution as `shoal stop` does,
including ones started later, recording `agent_stopped` alerts, or
`stop_failed` when stopping fails. Agents restore under the recovery rules below
once `disk.cleanup_free_gib` is available; otherwise free disk space, then run
`shoal resume`. A filesystem whose reading fails is skipped, so it removes and
stops nothing, and no agent restores until every reading succeeds.

Before stopping anything, Shoal warns the agents. While memory, CPU or free
disk space is past its `[machine.overload.warning]` threshold and that protection is
enabled, the daemon queues an [alert](alerts.md) for each
space with a running agent, naming the threshold. On Linux memory warns
when memory in use passes `memory_used_percent` or some task has stalled
waiting for memory for `memory_stall_percent` of the last ten seconds. A memory or CPU warning
tells the agent that load is high and that it needs no action; a disk warning
asks a workspace's agent to free space in its own worktree, deleting a build's
output whole or not at all, and to leave other worktrees and Shoal's directories
beside them to Shoal, and tells a chat's agent, which has no worktree, only that
space is low. CPU must stay above its warning threshold for `cpu_sustained_seconds`.
Each space hears about each signal at most once per `repeat_minutes`, and
agents started later are warned at the next sample. A memory or CPU warning,
and a disk warning to a chat, arrive with the agent's next update or turn without waking an idle agent; a disk warning wakes a
workspace's. Warnings stop and remove
nothing; a failed reading warns about nothing. Warning thresholds are not checked
against stop thresholds; a warning at or above one arrives with the stop.

[`[machine.overload]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#overload) sets the thresholds, from defaults
that [`load`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine) chooses, and `config reload` applies them.
A failed reading resets that signal’s timer and cannot authorize a stop;
memory protection remains independent of CPU readings.

An agent restores through its tool's
[`resume_command`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools). Without one, an AI tool with resume
arguments (`resume` and `resume_latest`, built in for Claude and Codex)
continues the session its
hooks last reported, otherwise the latest session in the workspace, without a
session picker. An agent stopped mid-turn, or whose turn is unknown, such as one
without Shoal's hooks, is prompted to continue its unfinished assignment, told when
and why it stopped and that its subagents and background commands ended. One
that stopped or exited on its own between turns, such as an asleep agent, opens
its session with no prompt and waits; when updates or messages wait for it, a
built-in resume opens with the prompt that wakes an idle agent, whose hook
delivers them as its first message. Resume commands receive the prompt through
`{prompt}`, and an argument that is exactly `{prompt}` is left out when there is
none;
other agents need a `resume_command`.
A failed command lookup warns and
disables automatic recovery for that launch. Overload alerts include the
pressure reason, execution ID, and automatic or manual recovery path, and the
alert of an agent whose automatic restore Shoal cancelled on its own says why. After the
wrapper stops the agent, Shoal also stops processes that carry the execution's
identity outside the agent's process group, such as background shell commands,
as `shoal stop` does. Shoal saves
the agent identity and pressure reason before signaling the wrapper, so a wrapper killed
under pressure can still be resumed after reconciliation. With one configured, the wrapper
stays connected while the agent is stopped and automatically runs that command
once healthy readings persist for the recovery interval and the
[agent queue](queue.md) admits the restore. An agent stopped while idle
restores instead once an update arrives for it, such as an agent message or
activity on its workspace's issues and PRs, which it receives as it starts.
Recovery from load requires
headroom below each enabled signal’s recovery threshold, on Linux memory in use
and memory stall of some task over the last minute both, whose thresholds must be
positive and below their stop thresholds, and with disk protection enabled at
least `disk.cleanup_free_gib` available on every monitored filesystem. Agents restore one at a time, each with
a fresh healthy interval, which a reload changing `[machine.overload]` restarts. Missing
readings, manual stop/removal, an exited wrapper, changed workspace ownership
or a done assignment prevent recovery, except from an [idle stop](sleep.md#idle-sleep). A
failure that comes from what the restore waits out, such as a store that cannot
write while the disk is full, delays it, and it tries again until it runs or the
agent is resumed, stopped or removed. Surviving processes Shoal cannot stop, or cannot prove stopped,
delay it: a `recovery_blocked` alert and the agent's terminal name
them, and the wrapper keeps waiting until they exit. Reloading with recovery disabled
finishes waiting agents at once, keeping their records for `shoal resume`.

`shoal stop [space...]` stops tracked agents and commands through their wrappers,
which print what stopped them and how to restore them, retaining work and resource leases. It stops
the spaces below each selected one too, workspaces and chats, unless `--skip-children`; a chat
started `--in` a workspace is not below it. Without a space it takes the current workspace,
else a picker of the workspaces and the running chats, never the chat it runs in. Its agents, and those
stopped there before, stay down until `shoal resume` continues them: each
becomes a last session, as an agent that exits on its own leaves, which holds
no heavy slot, keeps nothing from cleanup and nothing restores on its own. To
take agents down briefly, use `shoal restart` instead. Commands started
with `run` save their arguments, which `shoal resume` reports and never
reruns; arguments that are not UTF-8 are not saved. Setup, merges and other internal commands save nothing.
Agents waiting in the [agent queue](queue.md) to start in a stopped workspace, such as
a launch a workspace process started waiting for a heavy slot, have no session to save, so the stop removes them
from the queue and reports them in `cancelled`; `shoal resume` does not bring them back. One
already starting is left to its agent. Naming a queued agent of a new workspace instead of a
workspace removes it from the queue, as `shoal rm` does; a number always names a space,
since a queued agent has none.
Several spaces, or `-A`/`--all` for every running chat and every ready or failed workspace with a running
execution, a pending restore or a queued agent waiting to start in it, stop in parallel, reporting each failure without keeping the others running.
A stop whose wrappers do not finish in time fails naming each execution it
waited on and its step: not started, still running, or verifying after exit.
An agent whose wrapper or agent process receives a hangup or terminate signal, as
closing its terminal, logging out or shutting down sends, is saved as stopped
work with the signal as its stop reason, before its wrapper stops it, and its restore waits in the
[agent queue](queue.md) once its execution has ended, so it comes back after
a reboot without a command; run `shoal stop --all` first to keep agents down.
On Linux the daemon also holds a systemd-logind shutdown delay lock while it
runs. When a shutdown starts, it stops what `shoal stop --all` stops, each
wrapper saving its agent as for that signal, but keeps waiting and requested
restores, and admits nothing from the agent queue until the shutdown is
cancelled; then it releases the lock, which logind
honours for at most its `InhibitDelayMaxSec`. A `system_shutdown` alert
names the `shoal resume` command the work needs, naming done workspaces, which
`--all` skips, or says the agent queue restores it; a workspace that does not
stop records `stop_failed`, and a refused lock `environment_problem`. A signal
to the daemon stops work the same way unless a shutdown's stop already did, and
records `daemon_stopped` instead of `system_shutdown`. On a machine joined
to a hub, the stop then [hands its spaces over](hub.md#hand-over-at-shutdown) and
the alert names where each went. An
agent that was idle comes back only when an update arrives for it, unless
processes it started outside its process group were still running. An
agent whose restore needs a session picker, or whose assignment is done, waits
for `shoal resume` instead. Other commands just end. After a reboot, Shoal
forgets the executions of the earlier boot; an agent among them that saved
nothing becomes a last session. An execution whose wrapper left before it
started its command, such as a setup's under memory pressure, is forgotten once
no process at its wrapper's PID has the wrapper's birth and nothing of it runs,
so `shoal setup` retries the setup without `shoal stop`.
For an execution whose wrapper is gone, `shoal stop <workspace>` stops the
processes it verifies as the execution's and forgets the record once nothing of
it is visible. Processes it cannot verify or read fail the stop with their PIDs,
for you to check and stop yourself. An execution Shoal never recorded the launch
of cannot be proven ended even then, so stop asks you to confirm it did, and
`--yes` confirms without asking; a scoped process confirms only for the spaces
its scope reaches. Stopping several workspaces does not ask: without
`--yes`, it reports such a workspace as failed. Stopping cancels a waiting
restore and requires explicit resume even with a `resume_command` configured;
a stop that fails keeps the agents it stopped as stopped work.
A scoped process, a workspace's or a chat's agent, stops, restarts and
restores the spaces its scope reaches but its own, workspaces and chats alike:
its own space and those below it, or every space for a [master
session](chats.md#master-session). It stops them with `shoal stop <space>` or `--all`,
`--yes` included, restarts their agents with `shoal restart <space>`, `--all`
or a picker, discards their stopped agents with `shoal restore --discard-agent`, and
queues with `shoal resume <space>`, `--all` or the current space the restores
of their stopped agents, whoever stopped them, which the daemon starts
detached, and start them regardless of the limits, with `--now` or
`--focus`, as their space's [`limits` permission](modes.md#permitted-actions) allows. A space outside the scope is refused with an error naming it.

`shoal stop --interrupt [space...]` cancels the current turn of each agent
in the selected workspaces and running chats and keeps the agent running with
its background shells and subagents, ready for the next message. Shoal types
the agent's own interrupt key, Esc for Claude Code and Ctrl-C for Codex, into
the terminal session the daemon recorded for it through that session's
terminal host, as `shoal demo send` types into demos, and records the agent
idle. An agent without a running turn, including one whose turn Shoal has not
heard of since a daemon restart, is left alone. An agent that runs outside a
terminal host, such as in Happy, or that Shoal knows no interrupt key for
fails the command. Messages and item activity reach an interrupted
agent as they reach any idle one. `shoal resume <space>` continues the turn,
until the agent starts another: the daemon sends the agent `Continue where
you left off.` as a message, which wakes it as any message does. A restore that names spaces with interrupted turns and
others with stopped work or removed workspaces continues the turns and
restores the others, reporting the turns in `continued` beside them; one that
names a running chat without an interrupted turn has nothing to restore.
Several spaces are interrupted or continued together, reporting each failure
without keeping the others from it. A scoped process interrupts and continues
only the agents of the spaces below its own.

`shoal restart [space...]` restarts running agents in new wrappers: it
stops the agent, its wrapper finishes and its terminal closes, and the agent
queue starts the saved session again at once with the continuation prompt in a
new place, where it ran when that place is free, as `shoal resume --now` does;
`shoal attach` reaches it there. The run names that place when it differs from
where the agent ran, as a background start does, and `--json` gives each
restarted agent's `place`. The new wrapper runs the installed Shoal in a
new process tree, so an agent whose terminal lost access to the workspace, such
as to an external volume, starts again. `--in-place` instead has the wrapper
start the agent again in the same terminal, as an automatic restore does; a
wrapper whose Shoal binary an upgrade replaced continues from the installed one
in the same process, and wrappers of 0.13.0 and earlier keep their binary until
the agent is stopped and resumed. Either way the restore command finds the
agent through `PATH` again, so an updated agent binary runs. `--all` selects every workspace and chat of this
machine, a named space is restarted on whichever [joined machine](hub.md) runs
it, `--machine <name or ID>`, repeatable, selects every space of that machine
instead, those of an offline one skipped, naming it, and `--agent <name>`
only agents of that name on any machine. A report names the machine of a
space elsewhere, as `--json` does in `machine`. The command asks the daemon to
restart the agent of each selected space, which has at most one, and the
daemon restarts them together; `--sequential` has it restart them in one line
per machine with its other sequential restarts, one at a time, each once the
previous one runs again and at least 10 seconds after it started, and the
command reports each as pending once it joins the line. With
`--wait`, the daemon keeps each restart until that agent's turn ends, where a
permission prompt or an agent without turn reports counts as working, through
daemon restarts, and the command returns at once, reporting the agent as
pending. `shoal status` and `shoal list` show a kept restart, which goes once
it happens, its agent stops or exits first, its space goes, or `shoal restart
--cancel [space...]`, with restart's selection, drops it with the permissions
it would give, reporting a space it could not ask about, as on an offline
machine, and going on with the others; one that fails later reaches the coordinator whose process asked
for it, or else the user as an alert. Interrupting the command leaves
its restarts to the daemon. A newer restart of a space supersedes the earlier
one: their `--permission` changes merge, the newer winning on a key both set,
and when and how come from the newer; it joins an earlier restart that has not
started the agent yet, and otherwise restarts the agent again once it runs. A
restarted agent keeps its heavy slot, and commands keep running. Agents without
a command to resume their session and executions whose wrapper is gone are
skipped, for `shoal stop` and `shoal resume`. `--permission KEY=VALUE`,
repeatable, restarts each agent with that [permission](modes.md#mode-permissions)
given, as [`shoal start`](modes.md#mode-permissions) gives it, `KEY=mode` dropping
one given before, while the keys it leaves out keep theirs. A value the
caller does not hold is refused before any agent stops. With `--in-place` the
wrapper restarts the agent under them, as a new one would; one of a release
before that leaves the agent stopped for a restore in a new wrapper. An agent protection stopped waits
for its restore and is left out; a named space whose only agent waits is
reported with why it stopped and that `shoal queue start` restores it now. An agent that does not start
again, as when no terminal host can open its new place, stays stopped for
`shoal resume`, with the permissions `--permission` gave it, and the exit
status is 1. One whose stopped
processes do not exit is reported as pending with their PIDs and the command that
ends them, and restarts once they exit unless `shoal stop` cancels it.

`shoal resume [space...]` brings back the agent of each space, a workspace's or
a chat's alike: it asks the daemon to resume each space's agent, one request
per space, and the daemon queues the restore of each agent a saved recovery
record or last session keeps after its wrapper exits, or only the
`--execution <id>` one; the command reports how many it queued, and the
[agent queue](queue.md) starts them as the agent limits allow, each
where it stopped or in a background place as `--now` places them. An [asleep](queue.md) agent's restore then waits only
for its turn in the queue. A space that is [removed](removal.md#restore-a-removed-space)
is `shoal restore`'s. Without a space, resume uses the current
workspace when its agent can resume, otherwise a picker of the spaces whose
agent can resume, the workspaces, saying what each would resume, then the chats,
unfinished work first; without a terminal, or with `--json`, it lists them
instead, each led by the number that selects it.
A workspace whose setup failed, or never started, is refused, naming
`shoal setup <space>`, which retries the setup,
creating the worktree first when that failed too, after which the agent
waiting for it in the queue starts as the queue admits it.
A named space whose agent's launch [failed to start](queue.md), a
workspace or a chat, queues that launch again exactly as it was, with its
task, prompt and permissions, as the queue admits it, or at once with `--now`.
Completion describes each workspace the same way.
A workspace without stopped agents continues its last sessions at once when
resumed by itself, placed as `--now` places an agent, left by `shoal stop`
or by an agent that exited on its own, such as one the user quit, or the
`--execution` one. A workspace keeps one per agent; the next new agent started
there or `shoal done` replaces them. Unlike stopped work they use no heavy slot and never delay
cleanup.
Stopped commands are included in the restored agent's prompt, asking it to
rerun the ones still needed, on their own for an agent stopped between turns:
built-in agents receive it as their prompt argument,
and resume commands through `{prompt}`. Otherwise, and when no agent was
stopped, restore prints them as `shoal run` commands. Either way they are reported once.
Several spaces, each with stopped work or a last session, or `--all` for every
space whose assignment is not done, restore together and report commands
without an agent; one that fails is reported without keeping the others from
resuming, and a done space's agent restores only when its space is named. Restoring it says the work is
not finished, so it withdraws the space's completion, as `shoal done
--undo` does, and a later protection stop restores the agent; Shoal's own
restores and restarts keep the completion.
`--now` has the daemon start the agents at once regardless of the limits, every
one a space saved, or only the `--execution` one. In an
interactive place of an enabled host that outlives its agent, such as a Herdr pane with
`terminal.herdr.enabled`, it restores each agent in the place it
stopped in once that is back at its prompt. Otherwise it starts each in the background, as `shoal start` does: a
background place per agent, labelled as `shoal start` labels it, of the host serving
the calling terminal where its config lets it open places, such as `terminal.herdr.new_tab`,
or of `terminal.default_host` on a terminal no host serves, such as a detached zmx
session, or for a call without a terminal. The command reports the entries
as a restore without `--now` does, and `--json` emits them. When no host can
start a space's agents in the background, its resume, with or without `--now`,
is refused before any of them is restored, naming `--focus` to restore one
agent here. `-f`/`--focus` restores one agent in the current terminal at once,
regardless of the limits; with several stopped in one workspace, use
`--execution` or pick the agent by name, stop time, reason and whether it was
interrupted mid-turn or idle on a terminal; an agent without turn reports
counts as interrupted.
`shoal restore --discard-agent [space...]` discards the stopped agent, last
sessions, stopped commands and failed launch of each space, every space's with
`--all`, or the `--execution` one, without launching anything or touching files,
branch or worktree, allowing normal idle cleanup again; an agent whose wrapper
waits to restore it stays. Without a space it selects as `shoal resume` does.
`--permission KEY=VALUE`, repeatable, restores each selected agent
with that [permission](modes.md#mode-permissions) given, `KEY=mode` dropping one
given before, while the keys it leaves out keep theirs; its records keep them
from then on. It needs the spaces named or picked. A value the caller does not
hold is refused before anything is restored, and so is an agent whose
interrupted turn continues, since it keeps the launch it runs under. A wrapper
waiting to restore its agent restores it under them; one of a release before
that leaves the agent stopped for a restore in a new wrapper.
A [chat's](chats.md) agent resumes as a workspace's does, without waiting for a
heavy slot, and an interrupted turn continues in a chat as in a workspace.
`--all` never reopens removed spaces.
When a stopped agent's execution is unknown, after a daemon restart or a stop
Shoal could not prove, the daemon forgets it once its wrapper has exited and none
of its processes remain, keeping the record for `shoal resume`, which repeats the
check for the execution it selects. When processes survive or Shoal cannot prove
that none do, the execution stays, a `recovery_blocked` alert names the
problem, and the daemon checks again until they exit, or `shoal stop` stops
them. An agent still running when its wrapper
reattaches drops a protection stop that never reached it. Unrelated executions may keep running.
The command uses
the current restore configuration; without one, built-in terminal agents continue
the saved session. Other agents require a configured restore
command. Restore shows the saved pressure reason. Records
survive daemon restart and suppress idle cleanup until the replacement process
is registered; removal keeps them in its [removal record](removal.md#removal-records). A failed launch retains its records;
a later stop or overload creates a record for the replacement execution. The original
task prompt is never replayed.
