# Power

This file covers `shoal-power`'s side of the daemon: keeping the machine awake
and saving work before a shutdown.

Keeping the machine awake is opt-in and machine-wide: a laptop held awake with
its lid closed drains its battery and heats up in a bag, so only its user can
choose it. While enabled and an agent's turn is working, or an idle agent
watches an open PR whose activity will start its next turn, the daemon holds the
platform's sleep inhibitor as a child process that lasts as long as a pipe only
the daemon holds, so the inhibitor cannot outlive a daemon that crashes or is
killed. A failed inhibitor notifies and waits for the work to end or the config
to reload instead of retrying in a loop.

A hangup or terminate signal to an agent's wrapper or agent saves the agent as
manual stop does: closing a terminal, logging out and shutting down end the
agent's session without the agent choosing to quit, and the daemon may already
be gone. The wrapper saves it before stopping the agent, as the daemon does
before a protection stop, since shutting down may kill the wrapper first. Unlike
a manual stop, the agent then comes back on its own through the agent queue: a
request beside its record asks for the restore, which the daemon queues once the
execution has ended, so agents a reboot interrupted return without a command.
The signals alone depend on each wrapper receiving one and saving before the
session ends, so on Linux the daemon holds logind's shutdown delay lock and,
once a shutdown starts, stops running work itself with the same save-first stop
while it and the wrappers still run, then releases the lock. A delay lock, not a
blocking one: a shutdown never waits on Shoal beyond logind's limit, and the
records saved first survive a stop that outlasts it. Elsewhere the signal that
ends the daemon starts the same stop, which the service definition's stop
timeout bounds; after a logind stop the signal leaves work alone. As after a
protection stop, an agent stopped between turns returns only for an update,
counting processes still running outside its process group as a turn in flight,
and its restore keeps its workspace from no cleanup meanwhile unless a `shoal
sleep` timer will wake it, which gives it work at that time; `shoal resume` asks
for the agent itself, so its restore waits for nothing but admission. The
request is its own file so that forgetting it, as a stop does, never replaces a
record a restore holds.
