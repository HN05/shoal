# Machine power and pauses

This file says how Shoal keeps the machine awake, what it does once the user
walks away, and what `shoal pause` and `shoal unpause` do.

## Walking away

[`[machine] keep_awake`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
chooses what the machine does once the user walks away from it. The user walks
away by closing the machine's lid. On a machine without a lid, or one docked
with its lid closed, the user walks away by leaving it without input until the
platform reads it as idle. The user is back once the lid opens or input
returns.

On Linux, logind's `LidClosed` says the lid is closed and its `Docked` that
the machine is docked. Logind's idle hint for the user's sessions says the
user is idle. A user without a session that takes input, such as one with
only its service manager's, counts as present.

On macOS, IOKit's `AppleClamshellState` says the lid is closed. A Mac is
docked when a closed lid does not sleep it, as with an external display. The
user is idle after as long as the system's idle sleep timer.

When Shoal cannot read the platform, the user counts as present.

## Keeping the machine awake

Under `on`, the daemon keeps the machine from sleeping while an agent has work.
Under `finish`, it does so while the user is there. Once they walk away, only a
working turn keeps the machine awake. Once no agent has work, the machine may
sleep again.

Under `handover`, only a [hand-over](#handing-work-over) keeps the machine
awake. Under `off`, nothing does.

An agent has work while its [turn](agents.md#agent-state) is working. An idle
agent also has work while it receives the activity of a [watched](links.md) PR
not yet seen merged or closed, since that activity starts its next turn. The
PR may be its own space's or a [child](create.md)'s without an agent.

## Sleep inhibitor

On Linux, the daemon holds a `systemd-inhibit` lock on sleep, idle and the lid
switch. Logind obeys it, as do idle daemons that honour its inhibitors.

On macOS, the daemon runs `caffeinate -i -s`. It prevents idle sleep, and on
AC power system sleep too. A closed lid still sleeps a Mac that is not in
closed-display mode.

The inhibitor ends with the daemon. When it cannot start or ends on its own,
such as when polkit refuses the lock, the daemon records a `keep_awake_failed`
alert. It tries again only once no agent has work or the config reloads.

## Finishing turns

Once the user walks away under `finish`, the machine admits no new work. The
[agent queue](queue.md) starts no agent, restore, CI job or review, and the
hub's [placement](hub.md) skips the machine. Queued work waits for the user's
return.

Running turns go on. Once they end, the machine sleeps, and its sleep announcement hands what is left to
the [hub](hub.md).

## Handing work over

Under `handover`, the machine hands its work over once the user walks away or
it is about to sleep, as when its lid closes. It then admits no new work, as
under `finish`.

On a machine joined to a hub, other than the hub's own, the spaces with
something to do go to other machines as a [shutdown's hand-over](hub.md) sends
them. A space has something to do while its agent works on a turn or waits in
the queue to be restored. Its agent stops as `shoal stop` stops it, saving its
session, and its final package goes to the hub, which continues it on another
machine. A workspace and the chats in its worktree go
together when one of them has something to do. Spaces with nothing to do stay,
and the sleep announcement covers them. An alert says where each space went.

The machine stays awake until the hub confirms every package, for at most 10
minutes. Before a sleep, the hand-over takes most of the time the platform
allows and leaves the rest for the sleep's announcement. The machine may then
sleep. A shutdown meanwhile, even during the hand-over, hands over the spaces
still here.

Once the user is back, or the machine wakes from such a sleep, it admits work
again. It retires its copies of the spaces that went, as after any takeover.

## Pausing a machine

```sh
shoal pause                    # Stop all work here, send it elsewhere, admit nothing new
shoal unpause                  # Admit work again and restore what stayed
shoal pause --machine desktop  # Another machine of the hub, by name or ID
```

`shoal pause` frees a machine, such as a gaming PC, for its user at once. The
pause lasts through daemon restarts and reboots until `shoal unpause`. Only the
user's own terminal pauses or unpauses a machine, since that acts on every
space there. `--machine` pauses or unpauses another machine of the hub through
the hub.

A paused machine admits no new work, as under `finish`. Every agent stops as
`shoal stop` stops it, saving its session. On a machine of a hub, the hub's own
included, the spaces with something to do go to other connected machines, as
under [`handover`](#handing-work-over).

The CI jobs the machine runs end and run again elsewhere, as a sleep ends them.
Its running reviews wait in the queue again, and its demos stop.

A `handed_over` alert names where each space went, or why it stayed: the
machine is in no hub, no other connected machine can take it, or its package
did not reach the hub in time. A space that stays is stopped with its session
saved.

The hub's own machine keeps routing requests, syncing default branches and
following the forge while paused. `shoal status` outside a space says since
when the machine is paused, and `shoal hub ls` names each paused machine with
since when.

## Unpausing a machine

`shoal unpause` admits work again. The queue restores the agents the pause
stopped here. The machine retires its copies of the spaces that went, as after
any takeover.

An unpause is refused while the pause still stops work and hands spaces over,
until its `handed_over` alert arrives, since a space it sends would then run on
two machines.
