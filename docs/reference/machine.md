# Machine power and pauses

This file says how Shoal keeps the machine awake and what `shoal pause` and
`shoal unpause` do.

## Keeping the machine awake

[`machine.keep_awake`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
chooses what the machine does once the user walks away from it: closes its
lid, as logind's `LidClosed` on Linux and IOKit's `AppleClamshellState` on
macOS say, or, on a machine without a lid or a laptop docked with its lid
closed (logind's `Docked`, or on macOS a closed lid that does not sleep the
Mac, as with an external display), leaves it without input until the
platform reads it as idle, on Linux logind's idle hint for the user's
sessions, where a user without a session that takes input, such as one
with only its service manager's, counts as there, and on macOS as
long as the system's idle sleep timer. The user is
back once the lid opens or input returns.

Under `on` and, while the user is there, `finish`, the daemon keeps the
machine from sleeping while an [agent's turn](agents.md#agent-state) is working, or
while an idle agent receives the activity of a
[watched](links.md) PR not yet seen merged or closed, from
its own workspace or chat or a [child](create.md) without an agent,
since that activity starts its next turn; once no agent has either, the
machine may sleep again. Once the user walks away under `finish`, the
[agent queue](queue.md) admits nothing new, no agent, restore, CI job or
review, and the hub's [placement](hub.md) skips the machine, while running turns
go on: only a working turn keeps the machine awake then, and it sleeps once
they end, its sleep announcement handing what is left to the
[hub](hub.md). Queued work waits for the user's return.

Under `handover`, nothing keeps the machine awake while the user is there.
Once they walk away, or once the machine is about to sleep, as when its lid
closes, the queue admits nothing new and placement skips the machine, as
under `finish`, and on a machine joined to a hub, not the hub's own, the
spaces with something to do, an agent working on a turn or waiting in the
queue to be restored, with the workspace whose worktree such a chat runs
in, go to other machines as a [shutdown's hand-over](hub.md) sends them:
their agents stop as `shoal stop` stops them, saving their sessions, and
each space's final package goes to the hub, which places it and continues it
there, a chat in a workspace's worktree going with that workspace. The
machine stays awake until the hub confirms every package, or for at most 10
minutes, before a sleep only within most of the platform's allowance,
leaving the rest for the sleep's announcement, then may
sleep; an alert says where each space went. Once the user is back, or the
machine wakes from such a sleep, it admits work again and retires its copies
of the spaces that went. A shutdown meanwhile, even during the hand-over,
hands over the rest.
Spaces with nothing to do stay, and the sleep announcement covers them.

Linux holds a `systemd-inhibit` lock on sleep, idle and the lid switch,
which logind and idle daemons that honour its inhibitors obey; macOS runs
`caffeinate -i -s`, which prevents idle sleep and, on AC power, system sleep,
but a closed lid still sleeps a Mac that is not in closed-display mode. The
inhibitor ends with the daemon. When it cannot start or ends on its own, such as
when polkit refuses the lock, the daemon records a `keep_awake_failed`
alert and tries again only once agents have no work or the config reloads.

## Pausing a machine

```sh
shoal pause                    # Stop all work here, send it elsewhere, admit nothing new
shoal unpause                  # Admit work again and restore what stayed
shoal pause --machine desktop  # Another machine of the hub, by name or ID
```

`shoal pause` frees a machine for its user, such as a gaming PC, at once.
The pause lasts through daemon restarts and reboots until `shoal unpause`.
Meanwhile the [agent queue](queue.md) admits nothing new, no agent,
restore, CI job or review, and the hub's [placement](hub.md) skips the
machine. Every agent stops as `shoal stop` stops it, saving its session,
and on a machine of a hub, the hub's own included, the spaces with
something to do, an agent working on a turn or waiting in the queue to be
restored, go to other connected machines as a [shutdown's
hand-over](hub.md) sends them, a chat in a workspace's worktree going with
that workspace. The CI jobs the
machine runs end and run again elsewhere, as a sleep ends them, its running
reviews wait in the queue again, and its demos stop. A `handed_over` alert
names where each space went, or why it stayed: the machine is in no hub, no
other connected machine can take it, or its package did not reach the hub
in time. A space that stays is stopped with its session saved. The hub's
own machine keeps routing requests, syncing default branches and following
the forge while paused.

`shoal unpause` admits work again: the queue restores the agents the pause
stopped here, and the machine retires its copies of the spaces that went,
as after any takeover; while the pause still stops work and hands spaces
over, until its alert arrives, an unpause is refused, since a space it
sends would then run on two machines. `shoal status` outside a space says since when the
machine is paused, and `shoal hub ls` names each paused machine with since
when. `--machine` on either command pauses or unpauses another machine of
the hub through it. Only the user's own terminal pauses or unpauses a
machine, since that acts on every space there.
