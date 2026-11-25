# Sleep

This file says what `shoal sleep` does and when idle agents are woken.

An agent Shoal stopped on its own and brings back on its own is asleep: it
comes back through the [agent queue](queue.md) once what it waits for
arrives. Overload and disk protection, idle stops, a blocked space, a signal
that ended its session and queued work that needs room put an agent to sleep. An agent that `shoal
stop` stopped, or that quit on its own, is stopped instead, and stays down
until `shoal resume`. `shoal list` shows an asleep agent's space as `asleep`,
the [inbox](workspaces.md#inbox) lists it, and `shoal status` adds an `Asleep` line with
why it sleeps, what wakes it, and `shoal attach` and `shoal queue start`, which
wake it now, and a `Keeps` line with the [leases it keeps](resources.md#leases-while-asleep),
each simulator or containers with when Shoal stops it, such as `simulator
default (until 14:30), resource signing`; `--json` gives `asleep` with
`reason`, `wakes_when` and `keeps`, each with its `kind`, its `name` and its
`until`. Where an
asleep agent's wrapper waits in a terminal for its restore, it keeps what the
agent last showed and draws a block under it saying what wakes the agent and
why it sleeps, ignoring other keys: Enter wakes the agent there, as
`shoal queue start` does, once its processes have stopped, and Ctrl-C stops
the wait, leaving the agent stopped for `shoal resume`. Closing that terminal,
or ending its session, leaves the restore to the agent queue, which restores
the agent elsewhere. An asleep agent [keeps its leases](resources.md#leases-while-asleep).

An [idle](agents.md#agent-state) heavy agent, a workspace's or a chat's, sleeps once it
has waited for you: seconds after its turn ends when you are not engaged with
it, and when you are, twice the time you usually take to come back to it,
counted from the later of the turn's end and your last interaction, at least
those seconds and at most
[`[sleep] max_wait_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#sleep),
which it also waits before you have come back to any agent. You interact with
an agent by a key it reads in its terminal, which its wrapper finds by the
terminal's access time, leaving out the keys Shoal types itself; by a prompt you
submit, or a message you send it; and by `shoal attach` to its space, an app
following its session, `shoal resume`, `shoal restart`, `shoal start --in`,
starting its queued restore and Enter on its sleep screen, from any machine
or device of the hub. The same commands from an agent, such as a
coordinator's, are not yours, nor are updates, Shoal's alerts and idle
wakes, and whether a terminal shows the agent plays
no part. You are engaged with an agent once you have interacted with it since
its turn before the last ended, so a turn whose wait passed without you ends
it. The time you take to come back is the average, over the space's last turns
you were engaged with, from a turn's end to your first interaction after it,
each counted at most the longest wait; a space without one takes your average
over every space. Neither the [prompt cache](agents.md#agent-state) nor the machine's
schedule plays a part: a restored session reads its history from the cache as
the running one would. A
[light](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#modes-and-rules)
agent keeps running, and `[sleep] enabled = false` keeps every idle agent
running. Only an agent idle with no subagent or processes running outside its
process group, and with a restore command, sleeps. The stop saves the agent as
[overload protection](load.md) does, records an `agent_stopped`
alert, and restores the agent once an update arrives for it, though its
assignment is done and whatever `[machine.overload.recovery]` says. A restore
resumes the session from the agent's launch arguments, so what you changed
inside the session, such as a permission mode switched with Shift-Tab,
approvals given for the session or a `/model` switch, does not carry over.

While an entry in the [agent queue](queue.md) waits for a heavy slot or memory,
Shoal makes room for it: for each such entry in queue order, it puts to sleep
the idle agent first in the [stop order](load.md) with no running
agent below it whose priority is no higher than the entry's, of
the entry's repository when that repository's `agent.max_heavy_running` holds
it, and for an entry that waits for a heavy slot one that holds one, which a
done space's agent does not. That reaches the agents still waiting for you
before their wait ends, once they have idled those first seconds, or that
`[sleep]` keeps running; an agent you are
engaged with keeps running, as do light agents, in a workspace or a chat, and an
agent woken from its sleep works its turn before it can sleep again.

Timers the agent keeps itself end with its stop. Claude Code
agents Shoal launches are refused its timer tools, `ScheduleWakeup`,
`CronCreate` and `CronDelete`, including in a `/loop`, with a reason naming the
timer of [`shoal sleep --for`](#idle-wakes), which Shoal keeps.

## Idle wakes

```sh
shoal sleep                 # Asleep from the end of the turn until the next
shoal sleep --permanent     # Back to sleep after every turn until --undo
shoal sleep --for 20m -- "Check whether CI finished"   # Also a timer; or --until 14:30
shoal sleep --undo [space]  # Awake; ends either kind of sleep, cancels the timer and wakes it
```

As a backup to the wakes its updates bring, Shoal wakes an agent that has
stayed [idle](agents.md#agent-state) as long as its mode's `idle_wake_minutes` (60 for
`worker`; unset or 0 never), counted from the end of its last turn, with an
[alert](alerts.md): "You have been idle for 60 minutes. Continue
your work if any remains; otherwise run `shoal sleep`." An agent that stays idle
is woken again after each further period. A space whose work is
[done](completion.md#assignment-completion) is never woken; waiting threads and pending
approvals do not pause the timer.

An agent that waits for something runs `shoal sleep`: from the end of its turn
it sleeps and gets no idle wakes, and Shoal stops a heavy one as it stops any
[idle agent](sleep.md). A plain `shoal sleep` lasts until its next turn starts, by an update, a
message or the user; a permission prompt in the turn that slept does not end
it. `--permanent` puts the agent back to sleep after every turn, through
restarts and restores, until `shoal sleep --undo`, which also wakes an asleep
agent: its restore then waits only for its turn in the agent queue. The user or
a coordinator can run either for a space below. `list` shows a sleeping idle
agent as `asleep`, and `status` adds a `Sleep` line; `--json` gives `sleep`,
`turn` or `permanent`.

`--for` with a length such as `20m` or `1h30m`, or `--until` with the next
time the local clock shows, `HH:MM`, also sets the space's timer, replacing the
one it had: when it is due, Shoal sends the agent the text after `--` as an
[alert](alerts.md), or says only that the timer fired, which
reaches an awake agent through its hooks and restores an asleep one. An update
or message that comes first wakes the agent and leaves the timer set. The timer
is kept through agent stops, restores and daemon restarts until it fires,
`shoal sleep --undo` cancels it, or the space is [done](completion.md#assignment-completion)
or removed. There is no recurring form: a loop sets its next timer at the end of
each turn. The `Sleep` line of `status` names when the timer fires, and
`--json` gives it as `wake_at`.
