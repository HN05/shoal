# Sleep

This file says when Shoal puts an agent to sleep, what wakes it, and what
`shoal sleep` does. [Agent stops and restores](../design/agent-lifecycle.md)
gives the reasons.

## Asleep and stopped

An agent is asleep when Shoal stopped it on its own and will bring it back on
its own. It comes back through the [agent queue](queue.md#restores) once what it waits
for arrives. These stops put an agent to sleep:

- [overload and disk protection](load.md),
- an [idle stop](#idle-sleep),
- a blocked space,
- a signal that ended the agent's session,
- [making room](#making-room) for queued work.

An agent that `shoal stop` stopped, or that quit on its own, is stopped
instead. It stays down until `shoal resume`.

An asleep agent [keeps its leases](resources.md#leases-while-asleep).

A restore resumes the session from the agent's launch arguments. What you
changed inside the session does not carry over, such as a permission mode
switched with Shift-Tab, approvals given for the session or a `/model` switch.

## Asleep in lists and status

`shoal list` shows an asleep agent's space as `asleep`, and the
[inbox](workspaces.md#inbox) lists it. `shoal status` adds two lines:

- `Asleep` says why the agent sleeps and what wakes it. It names
  `shoal attach` and `shoal queue start`, which wake it now.
- `Keeps` lists the leases the agent keeps. Each simulator or containers lease
  shows when Shoal stops it, such as
  `simulator default (until 14:30), resource signing`.

With `--json`, status gives `asleep` with `reason`, `wakes_when` and `keeps`.
Each entry of `keeps` has its `kind`, `name` and `until`.

## The sleep screen

An asleep agent's wrapper may wait in a terminal for its restore. That
terminal keeps what the agent last showed. Below it, a block says what wakes
the agent and why it sleeps. The wrapper ignores every key but these:

- Enter wakes the agent there, as `shoal queue start` does, once its processes
  have stopped.
- Ctrl-C stops the wait and leaves the agent stopped for `shoal resume`.

Closing the terminal, or ending its session, leaves the restore to the agent
queue, which restores the agent elsewhere.

## Idle sleep

An [idle](agents.md#agent-state) heavy agent, a workspace's or a chat's,
sleeps once it has waited for you:

- When you are not [engaged](#engagement) with it, it waits a few seconds
  after its turn ends.
- When you are engaged with it, it waits twice your
  [reply time](#reply-time). The wait counts from the later of the turn's end
  and your last interaction. It lasts at least those few seconds and at most
  [`[sleep] max_wait_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#sleep).
  Before you have come back to any agent, you have no reply time, and it
  waits `max_wait_minutes`.

An agent sleeps only when it is idle, has a restore command, and runs no
subagent and no process outside its process group. A
[light](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
agent keeps running. `[sleep] enabled = false` keeps every idle agent running.
Neither the prompt cache nor
[`machine.schedule`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
changes when an agent sleeps.

The idle stop saves the agent as [overload protection](load.md#protection-stops) does and
records an `agent_stopped` alert. An update for the agent restores it, even
when its assignment is done and whatever `[machine.overload.recovery]` says.

### Interaction

You interact with an agent when you:

- press a key it reads in its terminal. Its wrapper tells this by the
  terminal's access time and leaves out the keys Shoal types itself.
- submit a prompt to it or send it a message.
- run `shoal attach` to its space, `shoal resume`, `shoal restart` or
  `shoal start --in` for it.
- follow its session in an app.
- start its queued restore, or press Enter on its sleep screen.

Each counts from any machine or device of the hub. The same commands from an
agent, such as a coordinator, are not your interaction. Nor are updates,
Shoal's alerts and idle wakes. A terminal that shows the agent plays no part.

### Engagement

You are engaged with an agent once you have interacted with it since its turn
before the last ended. A turn whose wait passes without you ends your
engagement.

### Reply time

Your reply time for a space is the average time from a turn's end to your
first interaction after it. It covers the space's last turns you were engaged
with, each counted at most as long as the longest wait. A space without such a
turn takes your average over every space.

## Making room

While an entry in the agent queue waits for a heavy slot or memory, Shoal
makes room for it. It takes the waiting entries in queue order. For each, it
puts to sleep the first idle agent in the [stop order](load.md#stop-order) that:

- has no running agent below it,
- has a priority no higher than the entry's,
- belongs to the entry's repository, when that repository's
  `agent.max_heavy_running` holds the entry back,
- holds a heavy slot, when the entry waits for one. A done space's agent holds
  none.

This reaches an agent still waiting for you, once it has idled those first
seconds, and one that `[sleep]` keeps running. An agent you are engaged with
keeps running, and so does a light agent. An agent woken from sleep works its
turn before it can sleep again.

## Agent timers

An agent's own timers end when Shoal stops it. Shoal refuses the timer tools
of the Claude Code agents it launches, `ScheduleWakeup`, `CronCreate` and
`CronDelete`, also in a `/loop`. The refusal names
[the timer Shoal keeps](#sleep-timer).

## Idle wakes

Updates wake an agent, and idle wakes back them up. Shoal sends an
[alert](alerts.md) to an agent that has stayed [idle](agents.md#agent-state)
as long as its mode's
[`idle_wake_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules).
[Modes](modes.md#modes) gives the value each built-in mode sets. A
`worker` agent idle for 60 minutes gets this alert:

> You have been idle for 60 minutes. Continue your work if any remains;
> otherwise run `shoal sleep`.

The period counts from the end of the agent's last turn. An agent that stays
idle gets another wake after each further period. Waiting threads and pending
approvals do not pause the count. Shoal never wakes the agent of a
[done](completion.md#assignment-completion) space.

## `shoal sleep`

```sh
shoal sleep                 # Asleep from the end of the turn until the next
shoal sleep --permanent     # Back to sleep after every turn until --undo
shoal sleep --for 20m -- "Check whether CI finished"   # Also a timer; or --until 14:30
shoal sleep --undo [space]  # Awake; ends either kind of sleep, cancels the timer and wakes it
```

An agent with nothing to do until something arrives runs `shoal sleep`. From
the end of its turn, it sleeps and gets no idle wakes. Shoal stops a heavy one
as it stops any [idle agent](#idle-sleep).

A plain `shoal sleep` lasts until the agent's next turn starts, by an update,
a message or you. A permission prompt in the turn that slept does not end it.

`--permanent` puts the agent back to sleep after every turn, through restarts
and restores, until `shoal sleep --undo`.

`--undo` also wakes an asleep agent. Its restore then waits only for its turn
in the agent queue.

You or a coordinator can run each of these for a space below. `shoal list`
shows a sleeping idle agent as `asleep`, and `shoal status` adds a `Sleep`
line. With `--json`, `sleep` is `turn` or `permanent`.

### Sleep timer

`--for` takes a length, such as `20m` or `1h30m`. `--until` takes a time,
`HH:MM`, and means the next time the local clock shows it. Either sets the
space's timer and replaces the one it had.

When the timer is due, Shoal sends the agent the text after `--` as an
[alert](alerts.md). Without text, the alert says only that the timer fired.
The alert reaches an awake agent through its hooks and restores an asleep one.
An update or message that comes first wakes the agent and leaves the timer
set.

The timer lasts through agent stops, restores and daemon restarts. It ends when
it fires, when `shoal sleep --undo` cancels it, or when the space is
[done](completion.md#assignment-completion) or removed. A timer never repeats:
a loop sets its next timer at the end of each turn.

The `Sleep` line of `shoal status` says when the timer fires, and `--json`
gives it as `wake_at`.
