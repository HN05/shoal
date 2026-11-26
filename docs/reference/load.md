# Overload protection and stops

This file says how Shoal protects the machine's memory, CPU and disk, where the
work it starts runs, and how agents stop, restart and resume.
[Overload protection](../design/protection.md) and
[agent stops and restores](../design/agent-lifecycle.md) give the reasons.

## Protection

The daemon stops work when memory pressure turns critical, or when the CPU
stays overloaded where CPU protection is on. It stops one entry of the
[stop order](#stop-order) at a time. After each stop it waits for the cooldown,
samples again, and stops another entry while the pressure lasts.

[`[machine.overload]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#overload)
sets the thresholds, from defaults that
[`load`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
chooses. `shoal config reload` applies them. The configuration reference says
what each reading measures.

A failed reading resets its signal's timer and authorizes no stop. Memory
protection does not depend on CPU readings.

Protection does not guarantee that the OS never reaches its OOM limit.

### Memory pressure

On Linux, memory pressure is critical when either of these holds:

- Memory in use passes `memory.used_percent`. This acts at once.
- Every task has stalled waiting for memory for at least
  `memory.stall_percent` of the last ten seconds, and that has held for
  `memory.sustained_seconds`. Only a reading below `memory.stall_percent`
  ends that stretch, however far apart the samples come.

macOS uses its native critical pressure.

While memory is past a warning or stop level, the daemon log records the
readings protection acts on: memory in use, the ten-second stall of some tasks
and of every task, and the work's memory against its
[memory limit](#memory-limit). It logs a level again after 30 seconds, and a
higher level at once.

### CPU overload

The CPU is overloaded once its use stays above `cpu.used_percent` for
`cpu.sustained_seconds`.

### Stop order

The stop order lists the running work in the order a stop of it loses least,
first first:

1. CI jobs this machine runs, newest first. A rerun loses nothing.
2. [Demos](demos.md). Shoal runs their commands again.
3. Agents, a workspace's or a chat's. An agent comes only after every running
   agent below its space, so a coordinator never stops while its children
   work.

Among the agents free to go, these rules decide, each only where the ones
before it tie:

- An agent of a mode that is not
  [light](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
  goes before a light one.
- Then the [agent queue's order](queue.md#queue-order) reversed decides: the
  agent you [interacted](sleep.md#interaction) with longest ago, or never,
  goes first, then an [idle](#idle-agents) agent before a working one, then
  the agent whose space ranks last, lowest priority first.
- Within a space, the agent idle longest goes first, otherwise the newest.

### What a stop must free

Protection stops the first entry whose stop frees what the pressure presses:

- A CI job's stop always does, so agents stop only once no job runs.
- A demo's or a heavy agent's stop does under CPU pressure. Under memory
  pressure it does only where its space's processes used at least 64 MiB at
  the latest usage sample, or have no sample.
- A light agent never stops for its own sake.

Stopping an agent first stops every running agent below it, light ones
included.

A running [reviewer](reviews.md#local-reviews) stops before anything in the
order, the newest first.

### Leases of asleep agents

Under memory pressure, protection stops a simulator or containers that an
asleep agent [keeps](resources.md#leases-while-asleep) before it stops an agent.
It stops one lease at a time, that of the asleep agent ranking lowest in the
agent queue first. It stops an agent only once no asleep agent holds such a
lease. It also stops one when nothing in the order is left to stop.

### Protection stops

Shoal saves the agent's identity and the pressure reason before it signals the
wrapper. A wrapper killed under pressure can then still be resumed after
reconciliation.

The wrapper ends the agent's process group through its normal grace period.
Shoal then stops the processes outside that group that carry the execution's
identity, such as background shell commands, as `shoal stop` does.

The stop keeps workspaces, execution recovery records and resource leases. It
records an `agent_stopped` alert. The alert names the pressure reason, the
execution ID, and whether the agent [restores on its own](#automatic-restore)
or needs `shoal resume`.

Protection never selects ordinary commands, desktop handoffs or disconnected
executions.

### Idle agents

An agent is idle when all of these hold:

- Its hooks last reported a finished turn.
- It runs no [subagent](agents.md#agent-state).
- None of its processes runs outside its process group, such as a background
  command whose completion would start its next turn.

An agent without turn reports counts as working.

### Stopped CI jobs

A CI job that protection stops ends as one whose runner outlived its run does.
Shoal stops its workspace's processes, then its runner. The job's end in the
workspace's [log](log.md) says what it was stopped for.

On Forgejo, Shoal first cancels that job alone. The job shows as cancelled,
and the run's other jobs go on. GitHub offers no such cancel, so there the job
shows as failed.

Shoal reruns the job on its forge as soon as its run has finished there,
whatever this machine's readings. Each runner's [queue](queue.md) admits it as
any other job. When the run allows no rerun within a day, Shoal records a
`runner_failed` alert.

### OOM scores

On Linux the kernel's OOM killer follows the stop order. The daemon sets the
`oom_score_adj` of each process:

- At startup the daemon lowers its own score to that of the service manager
  that started it, the lowest it may set without privilege. The places and
  workers it starts keep that score.
- Every 10 seconds it gives each entry's processes the score of the entry's
  place in the order, from 900 for the first entry down to 200 for the last.
- An entry's processes are those that carry its execution's identity, short of
  its wrapper. A CI job's are every process of the scope its runner runs in.
- A command a wrapper starts begins at 200, so what it starts scores above
  Shoal's own processes before the daemon places it.

Within one score, the kernel takes the largest process first. When the kernel
kills one process of a CI job's scope, the whole job ends.

## Warnings

Shoal warns agents before it stops anything. While memory, CPU or free disk
space is past its `[machine.overload.warning]` threshold, and that protection
is on, the daemon queues an [alert](alerts.md) for each space with a running
agent. The alert names the threshold.

- On Linux, memory warns when memory in use passes `memory_used_percent`, or
  when some task has stalled waiting for memory for `memory_stall_percent` of
  the last ten seconds.
- CPU warns once it stays above its threshold for `cpu_sustained_seconds`.

What a warning asks depends on its signal:

- A memory or CPU warning tells the agent that load is high and that it needs
  no action.
- A disk warning asks a workspace's agent to free space in its own worktree.
  The agent deletes a build's output whole or not at all, and leaves other
  worktrees and Shoal's directories beside them to Shoal.
- A disk warning tells a chat's agent, which has no worktree, only that space
  is low.

A disk warning wakes a workspace's agent. The other warnings arrive with the
agent's next update or turn, without waking an idle agent.

Each space hears about each signal at most once per `repeat_minutes`. Agents
started later are warned at the next sample.

Warnings stop and remove nothing, and a failed reading warns about nothing.
Shoal does not check the warning thresholds of memory in use, CPU and disk
against stop thresholds. A warning at or above a stop threshold arrives with
the stop.

## Work slice

On Linux, the work Shoal starts runs in `shoal-work.slice`, each command in a
transient systemd scope of its own with `OOMPolicy=continue`. That work is:

- every command an execution wrapper starts for work: an agent, a CI job's
  runner, a demo, a setup or a `shoal run` command;
- every one-shot agent the daemon runs: a [reviewer](reviews.md#local-reviews)
  or the gate.

A command runs in its scope wherever its wrapper runs, such as in a terminal
host's place. It keeps the wrapper's terminal and its own environment,
including the runtime directory and bus its configuration sets. A wrapper names
the scope `shoal-<space ID>-<run>.scope`, for the space whose work it does.

The daemon, places, workers and wrappers stay out of the slice, and so do the
pushes and merges Shoal runs.

A CI job's scope ends whole when the kernel's OOM killer takes any process of
it.

A command starts directly in its wrapper's control group where no scope can
start:

- without a user bus;
- from a login whose processes the user's service manager cannot move;
- before systemd 254;
- in a development build.

[Execution](../design/execution.md) gives the reasons.

### Memory limit

A [`memory_limit`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
caps the memory the slice holds. The daemon gives the slice `MemoryHigh` at
the limit and `MemoryMax` a twentieth above it. It sets both at startup and on
`shoal config reload`, and lifts both once the limit is removed. The service
manager keeps the cap until the next boot.

Above the limit, the kernel throttles the work: it reclaims the work's cache
and swaps it out. Where that cannot keep the work below `MemoryMax`, the
kernel's OOM killer kills inside the slice only, in the order of the
[OOM scores](#oom-scores). The daemon and the desktop keep their memory.

A development build leaves the slice to the installed Shoal, so `load` sets it
no limit.

### Protection under a limit

With memory protection on, protection and [dynamic admission](queue.md#dynamic-admission) count
the limit as the machine's memory too. Shoal then warns agents, holds back
starts and stops work before the kernel throttles it.

The work's memory against the limit presses as the machine's memory does, at
the same levels. On Linux, the slice's own stall presses the same way. A start
fits only where both the machine and the work leave room.

The work's memory is:

- on Linux, what the slice holds that reclaim cannot free, its page cache
  aside, with what it swapped out and the memory the latest usage sample counts
  for work running outside the slice, such as a command whose scope could not
  start;
- before the slice exists, and on macOS, the memory the latest usage sample
  counts for every space's processes and containers and the one-shot agents'
  processes.

### Missing readings of the work

There is no reading of the work without a current usage sample, or with one
that could not read every workspace's containers. Until there is one, nothing
starts and no stopped agent recovers. Protection still acts on the machine's
own memory. This holds under the limit `load` sets too.

Once the samples begin to miss containers, a `load_warning` alert names what
could not be read and how to clear it, or to lift the limit with
`memory_limit = "none"`.

### Kills and peaks

The wrapper reads its command's scope every second while the command runs, and
once as it ends. It reads how many processes the kernel's OOM killer killed
there, and the most memory the scope held, counted as the slice's memory is.

- A CI job's peak raises the memory its [cost](ci.md#job-admission) counts. Its kills
  add a `job_ended` entry to its workspace's [log](log.md) that names them,
  such as a build's compiler killed for memory.
- An agent's kills show as `oom_kills` in its `agent_exited` or
  `agent_stopped` entry, and in the alert its exit sends.

A scope the kernel killed whole is gone before its last read. A command it
ended with SIGKILL therefore counts a kill when the slice counted one since the
previous read.

## Disk space

Disk protection reads the space available to unprivileged processes on each
filesystem that holds workspaces or daemon state. A filesystem whose reading
fails is skipped, so protection removes and stops nothing there. No agent
restores until every reading succeeds.

### Disk cleanup

When space on a filesystem falls below `disk.cleanup_free_gib`, the daemon
frees space there in this order:

1. It deletes the copies of [files](files.md) fetched from other machines that
   nobody changed since.
2. It deletes the Xcode data that removing a workspace of a repository using
   the [`xcode` preset](setup.md#build-tool-presets) deletes, for every
   worktree in the repository's directory that no longer exists.
3. It removes the workspaces that idle cleanup would remove, without waiting
   for their idle delay. A repository with idle cleanup disabled keeps its
   workspaces.
4. It empties the [shared caches](setup.md#shared-caches) that nothing uses.
5. It deletes the [files](files.md) kept for removed workspaces and chats,
   oldest first, until enough space is available. Each deletion records a
   `results_deleted` alert.

Removals record `workspace_removed` alerts and the `disk_space` event cause. A
pass that cannot free enough space, or fails, repeats at most every 30
seconds.

### Disk stops

While space stays below `disk.stop_free_gib` after cleanup, whether or not
cleanup failed, the daemon stops:

- every running agent, as memory protection does, each after the agents below
  it;
- every other tracked execution, as `shoal stop` does, including ones started
  later.

It records `agent_stopped` alerts, or `stop_failed` when a stop fails. The
agents [restore](#automatic-restore) once `disk.cleanup_free_gib` is available
again. Otherwise, free disk space, then run `shoal resume`.

## Restore commands

An agent restores through its tool's
[`resume_command`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ai-tools).
Without one, a tool with `resume` and `resume_latest` arguments, built in for
Claude Code and Codex, continues the session its hooks last reported, otherwise
the latest session in the workspace, without a session picker. Other agents
need a `resume_command`.

What the restored agent is told depends on how it stopped:

- An agent stopped mid-turn, or whose turn is unknown, such as one without
  Shoal's hooks, is prompted to continue its unfinished assignment. The prompt
  says when and why it stopped, and that its subagents and background commands
  ended.
- An agent that stopped or exited on its own between turns, such as an asleep
  agent, opens its session with no prompt and waits.
- When updates or messages wait for such an agent, a built-in resume opens
  with the prompt that wakes an idle agent. Its hook delivers them as the
  agent's first message.

A `resume_command` receives the prompt through `{prompt}`. The original task
prompt is never replayed.

## Automatic restore

After a protection stop, the wrapper stays connected while its agent is
stopped. It runs the restore command once both of these hold:

- Healthy readings have lasted for the recovery interval.
- The [agent queue](queue.md#restores) admits the restore.

Readings are healthy when each enabled signal is below its recovery threshold.
On Linux, memory in use and the memory stall of some task over the last minute
must both be below theirs. With disk protection on, every monitored filesystem
needs at least `disk.cleanup_free_gib` available.

An agent stopped while idle restores instead once an update arrives for it,
such as an agent message or activity on its workspace's issues and PRs. It
receives the update as it starts.

Agents restore one at a time, each after a fresh healthy interval. A reload
that changes `[machine.overload]` starts the interval again.

### What prevents a restore

A failed command lookup warns and turns automatic restore off for that launch.
These also prevent an automatic restore, except of an agent from an
[idle stop](sleep.md#idle-sleep):

- missing readings;
- a manual stop or removal;
- an exited wrapper;
- changed workspace ownership;
- a done assignment.

The alert of an agent whose automatic restore Shoal cancelled on its own says
why. Reloading with recovery disabled ends the wait of the waiting agents at
once. Their records stay for `shoal resume`.

### What delays a restore

A failure that comes from what the restore waits out, such as a store that
cannot write while the disk is full, delays the restore. Shoal tries again
until it runs, or until the agent is resumed, stopped or removed.

Surviving processes that Shoal cannot stop, or cannot prove stopped, delay it
too. A `recovery_blocked` alert and the agent's terminal name them, and the
wrapper waits until they exit.

## `shoal stop`

```sh
shoal stop 3                    # Stop a space's agents and commands, and the spaces below it
shoal stop 3 --skip-children    # Only that space
shoal stop --all                # Every space with something to stop
shoal stop 3 --yes              # Confirm that an execution Shoal cannot prove ended did
```

`shoal stop [space...]` stops tracked agents and commands through their
wrappers. The wrappers print what stopped them and how to restore them. The
stop keeps work and resource leases.

It stops the spaces below each selected one too, workspaces and chats, unless
`--skip-children`. A chat started `--in` a workspace is not below it.

Without a space, it takes the current workspace. Otherwise it opens a picker of
the workspaces and the running chats, which never offers the chat it runs in.

### Stopped agents stay down

A stopped agent stays down until `shoal resume` continues it, and so do the
agents stopped there before. Each becomes a [last session](#last-sessions), as
an agent that exits on its own leaves. To take agents down briefly, use
[`shoal restart`](#shoal-restart) instead.

A stop cancels a waiting restore. The agent then needs `shoal resume`, even
with a `resume_command`. A stop that fails keeps the agents it stopped as
stopped work.

### Stopped commands

Commands started with `shoal run` save their arguments. `shoal resume` reports
them and never reruns them. Arguments that are not UTF-8 are not saved. Setup,
merges and other internal commands save nothing.

### Queued agents

An agent waiting in the [agent queue](queue.md) to start in a stopped
workspace, such as a launch waiting for a heavy slot, has no session to save.
The stop removes it from the queue and reports it in `cancelled`. `shoal
resume` does not bring it back. An agent already starting is left to start.

Naming a queued agent of a new workspace, instead of a workspace, removes it
from the queue, as `shoal rm` does. A number always names a space, since a
queued agent has none.

### Stop several spaces

Several spaces stop in parallel. `-A`/`--all` selects every running chat, and
every ready or failed workspace with a running execution, a pending restore or
a queued agent waiting to start in it. A failure is reported without keeping
the others running.

A stop whose wrappers do not finish in time fails. It names each execution it
waited on and its step: not started, still running, or verifying after exit.

### Executions whose wrapper is gone

For an execution whose wrapper is gone, `shoal stop <workspace>` stops the
processes it verifies as the execution's. It forgets the record once nothing
of the execution is visible. Processes it cannot verify or read fail the stop
with their PIDs, for you to check and stop yourself.

Shoal cannot prove that an execution whose launch it never recorded has
ended, even then. `shoal stop` asks you to confirm that it did, and `--yes`
confirms without asking. A scoped process confirms only for the spaces its
scope reaches. A stop of several workspaces does not ask: without `--yes`, it
reports such a workspace as failed.

## Signals and shutdown

### Signals to an agent

A hangup or terminate signal to an agent's wrapper or agent process saves the
agent as stopped work, with the signal as its stop reason. Closing its
terminal, logging out and shutting down send such signals. The wrapper saves
the agent before it stops it.

Once its execution has ended, the agent's restore waits in the
[agent queue](queue.md#restores-without-a-wrapper). The agent thus comes back after a reboot without a
command. Run `shoal stop --all` first to keep agents down.

### System shutdown

On Linux the daemon holds a systemd-logind shutdown delay lock while it runs.
When a shutdown starts, the daemon:

- stops what `shoal stop --all` stops, each wrapper saving its agent as for a
  signal, but keeps the waiting and requested restores;
- admits nothing from the agent queue until the shutdown is cancelled;
- then releases the lock. logind honours the lock for at most its
  `InhibitDelayMaxSec`.

A `system_shutdown` alert names the `shoal resume` command the work needs, or
says that the agent queue restores it. It names done workspaces, which `--all`
skips. A workspace that does not stop records `stop_failed`, and a refused lock
records `environment_problem`.

### Signals to the daemon

A [signal to the daemon](daemon.md#signals) stops work as a shutdown does,
unless a shutdown's stop already did. It records `daemon_stopped` instead of
`system_shutdown`.

On a machine joined to a hub, the stop then [hands its spaces over](hub.md#hand-over-at-shutdown), and
the alert names where each went.

### What comes back

- An agent that was idle comes back only when an update arrives for it, unless
  processes it started outside its process group were still running.
- An agent whose restore needs a session picker, or whose assignment is done,
  waits for `shoal resume`.
- Other commands just end.

## Interrupt a turn

```sh
shoal stop --interrupt 3        # Cancel the agent's turn; it keeps running
shoal resume 3                  # Continue the interrupted turn
```

`shoal stop --interrupt [space...]` cancels the current turn of each agent in
the selected workspaces and running chats. The agent keeps running with its
background shells and subagents, ready for the next message.

Shoal types the agent's own interrupt key, Esc for Claude Code and Ctrl-C for
Codex. It types it into the terminal session the daemon recorded for the agent,
through that session's terminal host, as `shoal demo send` types into demos.
It then records the agent idle. Messages and item activity reach the agent as
they reach any idle one.

The command leaves alone an agent without a running turn, including one whose
turn Shoal has not heard of since a daemon restart. It fails for an agent that
runs outside a terminal host, such as in Happy, or whose interrupt key Shoal
does not know.

`shoal resume <space>` continues the turn, until the agent starts another. The
daemon sends the agent `Continue where you left off.` as a message, which wakes
it as any message does.

Several spaces are interrupted or continued together. A failure is reported
without keeping the others from it.

## `shoal restart`

```sh
shoal restart 3                 # Restart the space's agent in a new wrapper
shoal restart 3 --in-place      # In the same terminal
shoal restart --all --sequential  # Every agent on this machine, one at a time
shoal restart 3 --wait          # Once its turn ends
shoal restart --cancel 3        # Drop a restart that waits
```

`shoal restart [space...]` restarts running agents in new wrappers. It stops
the agent, its wrapper finishes and its terminal closes. The
[agent queue](queue.md) then starts the saved session again at once with the
continuation prompt, as `shoal resume --now` does. It starts it in a new place,
where the agent ran when that place is free. `shoal attach` reaches it there.

The run names the new place when it differs from where the agent ran, as a
background start does. `--json` gives each restarted agent's `place`.

The new wrapper runs the installed Shoal in a new process tree. An agent whose
terminal lost access to the workspace, such as to an external volume, thus
starts again.

A restarted agent keeps its heavy slot, and its commands keep running.

### Restart in place

`--in-place` has the wrapper start the agent again in the same terminal, as an
automatic restore does. A wrapper whose Shoal binary an upgrade replaced
continues from the installed one in the same process. Wrappers of 0.13.0 and
earlier keep their binary until the agent is stopped and resumed.

Either way, the restore command finds the agent through `PATH` again, so an
updated agent binary runs.

### Select agents

- A named space is restarted on whichever [joined machine](hub.md) runs it.
- `--all` selects every workspace and chat of this machine.
- `--machine <name or ID>`, repeatable, selects every space of that machine
  instead. It skips an offline machine and names it.
- `--agent <name>` selects only the agents of that name, on any machine.

A report names the machine of a space elsewhere, as `--json` does in
`machine`.

The command asks the daemon to restart the agent of each selected space, which
has at most one. The daemon restarts them together. Interrupting the command
leaves its restarts to the daemon.

### Sequential restarts

`--sequential` has the daemon restart the agents one at a time, in one line per
machine with its other sequential restarts. Each starts once the previous one
runs again, and at least 10 seconds after the previous one started. The command
reports each as pending once it joins the line.

### Restart once a turn ends

With `--wait`, the daemon keeps each restart until that agent's turn ends,
through daemon restarts. A permission prompt, or an agent without turn reports,
counts as working. The command returns at once and reports the agent as
pending.

`shoal status` and `shoal list` show a kept restart. It goes once it happens,
once its agent stops or exits first, or once its space goes.

`shoal restart --cancel [space...]`, with restart's selection, drops a kept
restart with the permissions it would give. It reports a space it could not
ask about, as on an offline machine, and goes on with the others.

A kept restart that fails later reaches the coordinator whose process asked for
it, or else the user as an alert.

### A newer restart

A newer restart of a space supersedes the earlier one. Their `--permission`
changes merge, the newer winning on a key both set. When and how come from the
newer. A newer restart joins an earlier one that has not started the agent
yet. Otherwise it restarts the agent again once it runs.

### Restart with permissions

`--permission KEY=VALUE`, repeatable, restarts each agent with that
[permission](modes.md#mode-permissions) given, as `shoal start` gives it.
`KEY=mode` drops one given before. The keys it leaves out keep their values. A
value the caller does not hold is refused before any agent stops.

With `--in-place`, the wrapper restarts the agent under the new permissions, as
a new one would. A wrapper of a release before that leaves the agent stopped
for a restore in a new wrapper.

### Agents it does not restart

- An agent without a command to resume its session, and an execution whose
  wrapper is gone, are skipped. They are left to `shoal stop` and
  `shoal resume`.
- An agent that protection stopped waits for its restore and is left out. A
  named space whose only agent waits is reported with why it stopped, and that
  `shoal queue start` restores it now.
- An agent that does not start again, as when no terminal host can open its
  new place, stays stopped for `shoal resume`, with the permissions
  `--permission` gave it. The exit status is 1.

An agent whose stopped processes do not exit is reported as pending, with their
PIDs and the command that ends them. It restarts once they exit, unless
`shoal stop` cancels it.

## `shoal resume`

```sh
shoal resume 3                  # Queue the restore of the space's agent
shoal resume                    # The current workspace, else a picker
shoal resume --all              # Every space whose assignment is not done
shoal resume 3 --now            # Start it now, regardless of the limits
shoal resume 3 --focus          # Restore one agent in this terminal
```

`shoal resume [space...]` brings back the agent of each space, a workspace's or
a chat's alike. It asks the daemon to resume each space's agent, one request
per space. The daemon queues the restore of each agent that a saved recovery
record or a last session keeps after its wrapper exits, or only the
`--execution <id>` one. The command reports how many it queued.

The [agent queue](queue.md) starts the restores as the agent limits allow. Each
starts where it stopped, or in a background place as `--now` places them. An
[asleep](sleep.md) agent's restore then waits only for its turn in the queue. A
[chat's](chats.md) agent resumes as a workspace's does, without waiting for a
heavy slot.

The restore uses the current [restore command](#restore-commands) and shows the
saved pressure reason.

### Select spaces

Without a space, `shoal resume` takes the current workspace when its agent can
resume. Otherwise it opens a picker of the spaces whose agent can resume: the
workspaces first, saying what each would resume, then the chats, unfinished
work first. Without a terminal, or with `--json`, it lists them instead, each
led by the number that selects it. Shell completion describes each workspace
the same way.

A [removed](removal.md#restore-a-removed-space) space is `shoal restore`'s.

`--all` selects every space whose assignment is not done. It never reopens
removed spaces.

Several spaces, each with stopped work or a last session, restore together. The
command reports the stopped commands no agent receives. A failure is reported
without keeping the others from resuming.

When one command names spaces with interrupted turns beside spaces with
stopped work or removed workspaces, it continues the turns and restores the
others. It reports the turns in `continued`. A running chat without an
interrupted turn has nothing to restore.

### Failed setups and launches

A workspace whose setup failed, or never started, is refused. The refusal names
`shoal setup <space>`, which retries the setup, creating the worktree first
when that failed too. The agent waiting for it in the queue then starts as the
queue admits it.

A named space whose agent's launch [failed to start](queue.md#failed-starts) queues that
launch again exactly as it was, with its task, prompt and permissions. It
starts as the queue admits it, or at once with `--now`.

### Last sessions

An agent leaves a last session when `shoal stop` stops it, or when it exits on
its own, such as when you quit it. A workspace keeps one per agent. The next
new agent started there, or `shoal done`, replaces them. Unlike stopped work,
last sessions use no heavy slot and never delay cleanup.

A workspace without stopped agents continues its last sessions at once when
resumed by itself, or only the `--execution` one. It places them as `--now`
places an agent.

### Stopped commands in a restore

The restored agent's prompt lists the stopped commands and asks it to rerun the
ones still needed. An agent stopped between turns gets them as its prompt on
their own. Built-in agents receive the prompt as their prompt argument, and
resume commands through `{prompt}`.

When no agent was stopped, or no agent restores, `shoal resume` prints the
commands as `shoal run` commands. Either way, they are reported once.

### Done spaces

`--all` leaves out a done space. Its agent restores only when its space is
named. Restoring it says the work is not finished, so it withdraws the space's
completion, as `shoal done --undo` does. A later protection stop then restores
the agent on its own. Shoal's own restores and restarts keep the completion.

### Start at once

`--now` has the daemon start the agents at once, regardless of the limits:
every agent a space saved, or only the `--execution` one.

- In an interactive place of an enabled host that outlives its agent, such as
  a Herdr pane with `terminal.herdr.enabled`, it restores each agent in the
  place it stopped in, once that place is back at its prompt.
- Otherwise it starts each agent in a background place of its own, labelled as
  `shoal start` labels one. The place belongs to the host serving the calling
  terminal, where its config lets it open places, such as
  `terminal.herdr.new_tab`.
- On a terminal no host serves, such as a detached zmx session, and for a call
  without a terminal, the place belongs to `terminal.default_host`.

The command reports the entries as a restore without `--now` does, and `--json`
emits them.

When no host can start a space's agents in the background, its resume is
refused, with or without `--now`, before any agent is restored. The refusal
names `--focus`, which restores one agent here.

### Restore in this terminal

`-f`/`--focus` restores one agent in the current terminal at once, regardless
of the limits. With several stopped in one workspace, use `--execution`, or
pick the agent by its name, stop time and reason, and whether it was
interrupted mid-turn or idle on a terminal. An agent without turn reports
counts as interrupted.

### Resume with permissions

`--permission KEY=VALUE`, repeatable, restores each selected agent with that
[permission](modes.md#mode-permissions) given. `KEY=mode` drops one given
before, and the keys it leaves out keep their values. The agent's records keep
them from then on. It needs the spaces named or picked.

A value the caller does not hold is refused before anything is restored. So is
an agent whose interrupted turn continues, since it keeps the launch it runs
under.

A wrapper waiting to restore its agent restores it under the new permissions.
A wrapper of a release before that leaves the agent stopped for a restore in a
new wrapper.

## Discard a stopped agent

```sh
shoal restore --discard-agent 3     # Discard the space's stopped agent
```

`shoal restore --discard-agent [space...]` discards each space's stopped
agent, last sessions, stopped commands and failed launch. `--all` selects every
space, and `--execution` one execution. Without a space, it selects as
`shoal resume` does.

It launches nothing and touches no files, branch or worktree. Normal idle
cleanup then applies to the space again. An agent whose wrapper waits to
restore it stays.

## Scoped processes

A scoped process is a workspace's or a chat's agent. Its scope is its own space
and the spaces below it, or every space for a
[master session](chats.md#master-session). It stops, restarts and restores the
workspaces and chats its scope reaches, but not its own space:

- It stops them with `shoal stop <space>` or `--all`, `--yes` included.
- It restarts their agents with `shoal restart <space>`, `--all` or a picker.
- It discards their stopped agents with `shoal restore --discard-agent`.
- It queues the restores of their stopped agents, whoever stopped them, with
  `shoal resume <space>`, `--all` or the current space. The daemon starts
  them detached.
- It starts those restores regardless of the limits, with `--now` or
  `--focus`, as its space's [`limits` permission](modes.md#permitted-actions)
  allows.

A space outside the scope is refused with an error naming it. A scoped process
interrupts and continues only the agents of the spaces below its own.

## Unknown executions

A stopped agent's execution becomes unknown after a daemon restart, or after a
stop Shoal could not prove. The daemon forgets it once its wrapper has exited
and none of its processes remain. It keeps the agent's record for
`shoal resume`, which repeats the check for the execution it selects.

When processes survive, or Shoal cannot prove that none do, the execution
stays. A `recovery_blocked` alert names the problem. The daemon checks again
until they exit, or until `shoal stop` stops them.

An agent still running when its wrapper reattaches drops a protection stop that
never reached it. Unrelated executions may keep running.

An execution whose wrapper left before it started its command, such as a
setup's under memory pressure, is forgotten once both of these hold: no process
at its wrapper's PID has the wrapper's birth, and nothing of the execution
runs. `shoal setup` then retries the setup without `shoal stop`.

After a reboot, Shoal forgets the executions of the earlier boot. An agent
among them that saved nothing becomes a [last session](#last-sessions).

## Recovery records

Recovery records survive a daemon restart. They keep the workspace from idle
cleanup until the replacement process registers. Removal keeps them in its
[removal record](removal.md#removal-records).

A failed launch keeps its records. A later stop or overload creates a record
for the replacement execution.
