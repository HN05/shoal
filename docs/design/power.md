# Power

This file covers how the daemon keeps the machine awake and saves agents
before a shutdown, and what `shoal-power` reads from the platform.
[Machine power and pauses](../reference/machine.md) and
[Signals and shutdown](../reference/load.md#signals-and-shutdown) say what each
does; this file gives the reasons.

## Keeping the machine awake

### Only the user opts in

Keeping the machine awake is opt-in, and it holds the whole machine. A laptop
held awake with its lid closed drains its battery and heats up in a bag. So
only its user chooses it, with `[machine] keep_awake`.
[Keeping the machine awake](../reference/machine.md#keeping-the-machine-awake)
says when each value holds the machine.

### The inhibitor ends with the daemon

The daemon holds the platform's sleep inhibitor as a child process. That
process lasts only as long as a pipe that only the daemon holds open. So the
inhibitor never outlives a daemon that crashes or is killed.

### A failed inhibitor waits

A failed inhibitor raises an alert and waits instead of retrying in a loop.
What failed, such as a lock polkit refuses, fails again until something
changes, such as the config. [Sleep inhibitor](../reference/machine.md#sleep-inhibitor)
says what ends the wait.

## Saving agents before a shutdown

### Signals save the agent

Closing a terminal, logging out and shutting down end an agent's session
without the agent choosing to quit. The daemon may already be gone by then.
So a hangup or terminate signal to an agent's wrapper or agent saves the agent,
as a manual stop does.

The wrapper saves the agent before it stops it, as the daemon does before a
protection stop. A shutdown may kill the wrapper before the agent.

### The agent comes back on its own

Unlike after a manual stop, a signalled agent comes back on its own through the
[agent queue](queue.md). A request beside its recovery record asks for the
restore, and the daemon queues it once the execution has ended. So agents a
reboot interrupted return without a command.

The request is a file of its own. Forgetting it, as a stop does, thus never
replaces a record a restore holds.

### The save-first stop

Signals alone depend on each wrapper receiving one and saving before the
session ends. So on Linux the daemon holds logind's shutdown delay lock. Once a
shutdown starts, the daemon stops running work itself while it and the
wrappers still run. Each wrapper saves its agent before it stops it, as for a
signal. The daemon then releases the lock.

The lock delays a shutdown and never blocks it. So a shutdown never waits on
Shoal beyond logind's limit. The records saved first survive a stop that
outlasts that limit.

Without logind's announcement, the signal that ends the daemon starts the same
stop, as [Exits](daemon.md#exits) says. The service definition's
[stop timeout](../reference/daemon.md#signals) bounds it. After a logind stop,
that signal leaves work alone, since the work is already saved and stopped.

### What comes back

The save-first stop restores agents as a protection stop does. An agent
stopped between turns comes back only for an update, as
[Automatic restore](agent-lifecycle.md#automatic-restore) says. Processes it
left running outside its process group count as a turn in flight, as
[Overload protection](protection.md) explains.

## The crate's boundary

The crate reads the machine's power and the user's presence from the platform:

- logind's and IOKit's announcements of a sleep, and logind's of a shutdown,
- the delay locks that hold either off until the daemon is ready,
- the lid, dock and idle readings that say whether the user walked away.

What the daemon does on each stays with it, such as holding the agent queue
through a sleep or stopping agents for a shutdown.
