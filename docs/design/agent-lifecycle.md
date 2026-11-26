# Agent stops and restores

This file covers how agents stop, sleep, restore, restart and are interrupted,
and the recovery records that carry them. [Overload protection and
stops](../reference/load.md) and [Sleep](../reference/sleep.md) say what each
command does; this file gives the reasons.

## Asleep and stopped

An agent Shoal stopped on its own, and will bring back on its own, is asleep.
An agent that waits for `shoal resume` is stopped. The two states let the user
tell an agent that comes back from one that needs them.

Every stop that queues a restore Shoal starts on its own puts the agent to
sleep: protection, idle and blocked stops, signal stops and making room.
`shoal stop` stops an agent, and so does the agent quitting on its own. The
agent of a removed or converted space is stopped too.

A stop that lasts until `shoal resume` stops the spaces below first. A parent
may sleep before its children, since their reports wake it.

The asleep state belongs to the restore. The restore keeps why the agent sleeps
and what wakes it, so lists, status and the inbox read it without the recovery
records. An agent that nothing will wake sleeps too, since the user wakes it
with `shoal attach` or Enter on its sleep screen.

## Recovery records

Before it delivers a stop, the daemon saves a minimal recovery record with the
stop's reason. A wrapper lost after that still leaves the handoff. A record
that fails to save warns, and protection stays on.

A load or disk stop that the store cannot record, as on a full disk, still
stops the agent. It stops it as `shoal stop` does and leaves it for
`shoal resume`.

A record names the stop's stage. The daemon saves `stopping` before it
delivers a stop, and the wrapper saves `stopped` after it ends the agent.

A saved record stands for unfinished work until a restore uses it or the user
discards it. Records persist across daemon restarts, so manual recovery does
too.

Stop and exit alerts carry the pressure reason and the way back. They tell an
automatic restore from manual recovery, and both from a missing record.

## Automatic restore

An agent restores on its own once load and disk space have recovered. This
holds for the built-in Codex and Claude Code resume commands, and for another
agent once its resume command is configured. Restores wait in the
[agent queue](queue.md), which decides what each waits for.

An agent stopped between turns has nothing to do until an update arrives for
it. That update restores it, and until then its restore holds no heavy slot.

A woken agent reports no idle turn until it has worked one. It thus never
sleeps again before it acts on what woke it.

### Done spaces

A done assignment needs no agent, so only a resume that names a done space
restores its agent. Such a resume says the work is not finished, so it
[withdraws the completion](../reference/load.md#done-spaces), whichever wrapper
restores the agent. Shoal's own restores and restarts say nothing of the work,
so they keep it.

## What a restored agent is told

A restore of an agent stopped mid-turn, or whose turn is unknown, prompts it to
continue its unfinished assignment. The prompt says when and why it stopped,
and that its subagents and background commands ended. Every stop that saves an
agent records when and why, and a last session keeps it.

An agent that stopped or exited on its own between turns is not told to
continue. Its session opens with only the handoff it needs, with the wake
prompt whose hook delivers the updates that wait for it, or with no prompt. It
then waits.

A restore never replays the original task prompt. Commands the user ran with
`shoal run` save their arguments. A restore reports them once, as the restored
agent's first prompt or else to the user, and never reruns them: replaying an
arbitrary command is not known to be safe.

### Which session restores

Built-in agents resume the session ID their hooks report. The workspace's
latest session may belong to another agent.

A restored agent carries the session of the record it restores until its hooks
report one, with how its space changed since, such as a conversion. A restore
that exits or stops before its first report thus leaves that session to
resume.

## Waiting wrappers

A wrapper that waits for its agent's restore stays tracked. It reattaches as a
running command does, so a daemon restart or crash hands the wait to the next
daemon instead of cancelling the restore.

A daemon update waits for a stop and for a restore that waits for load. It does
not wait through a wait for an update, which may last indefinitely. That
wrapper reattaches to the updated daemon.

### The user asks for the agent

Each of these is the user asking for the agent. Each uses the waiting wrapper's
queued restore instead of refusing the agent as running or starting a second
execution beside it.

- `shoal resume` of an agent whose wrapper waits. The restore then waits only
  for a heavy slot.
- Enter on the sleep screen, which the waiting wrapper's session shows under
  what the agent last showed. The wrapper asks over its own connection, which
  only that execution's wrapper holds, so a scope token it inherited does not
  decide it.
- `shoal attach`, which wakes the agent whatever the limits and rank. Its
  pickers offer the workspace and that session as stopped. With a session
  showing the agent, it starts the restore and attaches once the agent runs,
  under every place recorded for it. Without such a session, it starts the
  restore the asleep agent waits for. Without any session left, it restores a
  stopped agent as `shoal resume --now` does, through that restore.

## The daemon owns resumes

A resume request names a space. That space's daemon decides the resume, carries
it out and answers with one outcome once it acted or queued. It does one of
these:

- It continues a turn an interrupt ended with a message that wakes the agent.
  The agent's own wrapper types the prompt in its terminal, as for any message,
  so the daemon writes to no terminal.
- It queues a failed launch again.
- It queues the restores of what a stop saved, or starts them at once for
  `--now`.

The CLI keeps only what needs the caller's terminal, `--focus`. No queue entry
or execution of another space thus crosses a resume. A discard is one request
naming a space in the same way.

## Idle sleep

An idle heavy agent, a workspace's or a chat's, sleeps once it has waited for
the user. Shoal stops it as protection stops an agent between turns. An update
restores it even when its assignment is done, since Shoal rather than the agent
ended it. Load plays no part, so the recovery policy does not apply.

The user working with an agent keeps it running, not its prompt cache. A
session's second request misses the cache, whichever process sends it. Every
later one reads the whole history, the first after a resume included. Stopping
and restoring an agent thus costs no cache that keeping it would save.

A light agent uses too little of the machine to be worth a restore. It never
sleeps for being idle, and `shoal sleep` only ends its idle wakes.

### Interaction

[Interaction](../reference/sleep.md#interaction) is what the user does with an
agent. The daemon tells it by the unscoped caller, from whatever machine or
device it came.

The wrapper finds a key the agent reads by its terminal's access time. Reading
a key moves it, and the agent's output does not. The keys Shoal types itself
do not count, and Shoal reports them before it types.

Updates, Shoal's and a coordinator's messages, idle wakes and what Shoal or a
coordinator starts are not interaction. Nor is a terminal that shows the
agent, since a user may attach and walk away.

### How long an idle agent waits

An agent the user is not [engaged](../reference/sleep.md#engagement) with
sleeps after a floor of seconds. The floor lets hooks settle and a follow-up
turn start. A turn whose wait passes without the user ends their engagement,
so later turns that updates start wait only the floor.

An agent the user is engaged with waits twice their
[reply time](../reference/sleep.md#reply-time). The reply time counts to the
user's first key after a turn, not their prompt, so that reading counts. Each
turn counts at most the longest wait, so one return after hours does not
decide the waits after it.

### Making room

Queued work that needs a heavy slot or memory may put an idle agent to sleep
before its wait ends. It may do so too where `[sleep]` keeps idle agents
running. It takes the lowest rank and priority first, never above the entry's
priority.

It never takes an agent the user is engaged with, since nothing overrides an
engaged user. Nor does it take a light one, which frees too little.

## Timers and `shoal sleep`

Timers an agent keeps itself would end with its stop. A hook refuses Claude
Code's timer tools, reading only the tool's name, and names `shoal sleep`'s
timer. The model uses the timer Shoal keeps, also in a `/loop` the user types.

`shoal sleep` is the agent saying it has nothing to do until something arrives,
so it ends the agent's idle wakes. It also takes the agent's timer, since a
timer says the same until a time.

A space has one timer, and a new one replaces it. The daemon keeps it as state
through stops, restores and restarts. It lasts until it fires as an alert,
`--undo` cancels it, or the space is done or removed. A loop sets its next
timer each turn, so Shoal parses no recurring schedule.

## Turn state and stop races

A space's reported turn state counts the turns its agent starts. The count
continues for a restored agent or another execution.

A protection stop is recorded by execution, with the count it was decided at.
The record lasts until the agent runs again or its restore is cancelled.

A stop chosen between turns and a report that starts a turn can race. Each
decides in one transaction, so whichever commits first wins:

- The stop records itself only while the agent is still idle in the turn it
  was chosen in.
- A report that starts a turn records none once a stop is recorded. It leaves
  its update for the restore.

The stop reaches the wrapper only after its record commits.

A workspace with several agents keeps the turn of the one that reported last.
The others' turns are unknown, which counts as working.

## Unknown executions

A record an earlier daemon left authorizes no signal. A reattaching wrapper
restores it. When its restore no longer waits, the daemon forgets it.

At startup, and whenever an execution becomes unknown, the daemon forgets an
unknown execution with a saved record once both of these hold:

- Its wrapper has exited and no process it owns survives.
- Its launch identity is complete, or its record reached `stopped`. The daemon
  clears a stopped child's identity only after it verifies the stop.

It rechecks a live wrapper when that wrapper exits. A wrapper that reattaches
with its command running discards a `stopping` record, since that stop never
arrived.

Survivors and incomplete evidence keep the execution. The daemon rechecks them
periodically, so it clears once they exit. They are the only outcome that
notifies. `shoal stop` names them, and a resume still reconciles the
executions it restores. Agent metadata is transient, so a restart cannot
select disconnected survivors.

### Executions of an earlier boot

An unknown execution whose wrapper, and child when recorded, ran in an earlier
boot needs no evidence. Restarting the machine ended its processes, while its
recorded process group may now name another's. The daemon forgets it and keeps
its record, or else the session its agent reported, as a last session. The
daemon records each execution's agent name for this.

### Executions that never started

An unknown execution without a record, a recorded child or a process group
never started its command, such as a setup whose wrapper left first. The daemon
forgets it once no process at its wrapper's PID has the recorded birth and none
of its processes survives. It rechecks a live wrapper when that wrapper exits.

## Manual stop

`shoal stop` ends connected tracked executions through their wrappers. It keeps
work and leases. It means the user wants the agents down until they resume
them. Each stopped agent of the workspace whose execution ended thus becomes a
last session, including one a signal or protection stopped before. To take
agents down briefly, the user runs `shoal restart`.

The wrapper saves its recovery record as for any stop. The daemon converts it
into a last session once the stop finishes, so wrappers of earlier releases
stop the same way. `shoal repo move` stops agents as `shoal stop` does but
leaves them stopped work, since it starts that work again after the move.

A launch that waits in the agent queue to start in the workspace has no session
to save. A manual stop cancels it before it stops anything, and reports it. It
cancels it under the queue gate, so admission cannot start it after. A launch
already starting belongs to its agent.

## Last sessions

An agent that exits on its own before its assignment is done leaves a last
session for `shoal resume` instead of stopped work. The user may have quit it
on purpose. A last session thus takes no heavy slot and keeps nothing from
cleanup. It comes back only when `shoal resume` names its space or `--all`
queues it.

A workspace keeps one last session per agent, so resuming several agents that
stopped together restores each. A new agent replaces them, since the user moved
on. A restored agent consumes only its own, which its wrapper names when it
registers.

## Restart

`shoal restart` starts each agent in a new wrapper. The new wrapper runs in a
new process tree, so a restart recovers an agent whose wrapper's tree broke,
such as one that lost access to an external volume.

The daemon restarts an agent in these steps:

1. It sends the stop protection sends, with the reason
   `restarted by shoal restart`, and keeps the agent for recovery whatever the
   overload policy.
2. Once the agent's processes are proven stopped, it cancels the wrapper's
   restore. Every release's wrapper finishes on that and keeps the agent's
   record. The record becomes no last session, and its end sends no exit
   alert.
3. It records the record's restore as admitted. It starts it through the queue
   start worker as `shoal resume --now` does, in the terminal host of the
   agent's recorded place.

The record keeps the agent's slot until the admitted restore takes it over. A
restart thus neither queues behind launches nor loses the slot to them.

A start that fails, or leaves the queue before the agent registers, fails the
restart. A stop or completion that cancels the workspace's restores cancels
the restart, under the queue gate the restart records its start under. Either
way, the record stays for `shoal resume`.

The restart follows the agent to the execution the new wrapper registers, with
the runs that asked for it. That execution answers them once the agent starts,
with the place it registered, so a run names where the agent runs now.

`--in-place` restarts through the same wrapper, which keeps the agent in its
terminal. The daemon resumes the agent once its processes are proven stopped,
outside the agent queue.

A chat's agent restarts through the same path.

### The daemon owns restarts

A restart request names a space, and the daemon picks its agent. The daemon
answers once it restarted the agent, recorded the restart for later, or
skipped it. A skip says why, such as a protection stop that
`shoal queue start` ends at once.

Every restart is a store record of its space, how it restarts, its permission
change and who asked. It thus outlives the request and the daemon. The
watcher of turn ends that conversions use acts on a restart that waits for the
agent's turn to end or for its place in the machine's line of sequential
restarts. A failure with no request left to answer reaches the coordinator
that asked, or else the user.

Once the daemon begins a restart, at once or when the watcher acts on it, the
record stays until the agent's restore applies its options. That happens when
the wrapper resumes the agent in place or a new wrapper registers it. The
record stays while the restart waits for the stopped agent's processes too.
Each wrapper that reattaches to a later daemon meanwhile continues the restart
with them. One that reattaches still running the agent, without the stop, lets
the restart begin again.

Lists, status and `shoal restart --cancel` take only a restart not begun yet.

### A newer restart

A newer request supersedes the earlier one as if that ran to its end first.
Permission changes merge key by key, the newer winning. When and how come from
the newer.

A restart under way takes the newer request until its wrapper settles how, and
with what permissions, it starts the agent. After that, the daemon records the
newer one, and it runs once the agent does.

An agent between its wrappers is listed as the execution it ran as. One a new
wrapper runs is listed with the executions it ran as.

### Restarts together or in sequence

The daemon restarts agents together by default. `--sequential` spreads the
load of starting them through the machine's line. The next starts once the
restarted agent runs, and at least the admission stagger after the previous
start.

### Agents a restart leaves

Agents without a restore command, and executions whose wrapper is gone, are
left to `shoal stop` and `shoal resume`.

A restart that cannot start the agent leaves its record for `shoal resume`. It
names the space whose process asked for it, which may then resume the agent as
after its own `shoal stop`.

## Wrapper upgrades

The restore command resolves the agent again, so an updated agent binary runs.

A wrapper whose binary an upgrade replaced execs the installed one when the
daemon resumes its agent in place. It keeps its process, terminal and daemon
connection. The daemon sends nothing until the restored child starts, so no
control is lost.

The resume names the binary the daemon runs. The wrapper continues only when
the installed file is that binary, whatever path either started from, since the
daemon holds back its own update until the agent starts. The installed release
bounds the commands the wrapper runs to prepare the agent.

The installed release registers the execution again on that connection. The
daemon records its place and returns a new plan, and the wrapper then restores
the agent. New wrapper code thus reaches running sessions without stopping
them.

## Interrupt

`shoal stop --interrupt` redirects an agent without stopping it. The agent's
own interrupt key cancels its turn. The agent keeps its process, background
shells and subagents, which a stop and resume would end and start again.

The CLI types the key through the terminal host of the place the daemon
recorded for the agent, a workspace's or a chat's. A client driving Shoal thus
need not know each agent's key or each host. The daemon records the turn's end,
which no hook reports.

The daemon keeps the interruption with the turn state the agent reported until
a turn starts or ends again. `shoal resume` continues the interrupted turn, as
it continues stopped work, so a client pairs one command with each.
