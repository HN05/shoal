# Leases

This file covers `shoal-leases`: the ports, simulators, resource permits,
containers and secrets that workspaces lease. The
[resources reference](../reference/resources.md) says what the lease commands
do, and [agent secrets](../reference/agents.md#agent-secrets) how agents
receive secrets.

## Principles

A lease belongs to a workspace, not to the command that acquired it. It
outlives that command, daemon restarts and a failed removal. An explicit
release or a successful removal ends it.

Shoal allocates each lease atomically. A lease's name makes acquiring it
idempotent: repeating the request returns the lease already held.

A workspace claims nothing when Shoal creates it. It acquires each lease when
its work needs one.

Shoal records and counts what it hands out, and use is cooperative. User
scripts own the integration with the underlying resources.

Chats have no leases, since they run nothing on the machine, as
[chats](chats.md) says.

## Commands

Leases follow AGENTS.md's grammar for cross-kind verbs. `acquire`, `release`
and `leases` take the kind first, such as `acquire sim`, so no kind has a group
of its own. A command selects the workspace with `--space`, never a positional
space. Like `unlink`, `release` without an item or a kind releases every lease
of that kind or of the workspace.

Holds share these verbs. A hold keeps a space from cleanup instead of sharing
capacity, so `release` without a kind leaves holds.

`leases` shows the leases and the usage that limits new ones. `catalog` shows
the configuration and the machine's inventory they draw from, secrets without
their commands. `history` shows a kind's audit trail, such as the simulators'
clean-device requests.

A listing that covers every workspace reads them with bounded concurrency. It
keeps their order and reports every failure.

## Ports

A port reservation is cooperative. Shoal probes the port, records it and
exports it to later executions. It never holds a socket, so the workspace's own
server can bind the port.

## Simulators

A simulator lease is exclusive. Shoal leases only devices it created. Devices
it did not create still count toward capacity.

Shoal records a claim before it changes a device, so a crash leaves the device
owned. A claim stays after a failure, so Shoal retries the operation.

An allocation never takes a leased device. A busy request takes a device only
from an asleep agent, through that agent's release, as
[taking a lease from an asleep agent](#taking-a-lease-from-an-asleep-agent)
says.

A handoff keeps the device's state, so the next workspace reuses its installed
apps. Erasing a device needs `--clean` with a reason. Shoal writes the audit
record before the erase, so even an interrupted erase leaves one.

Shoal deletes its devices when their workspace is removed or their idle timer
expires.

### Allocation

Allocation plans over the recorded devices and the inventory alone, without
side effects. The executor then takes the simulator gate. Under it, the
executor checks ownership and live capacity again, since both may have changed
since the plan. It then writes the audit record, records the claim and runs
`simctl`, in that order.

Only a confirmed shutdown frees running capacity. A device in any other state
may still run.

A runtime named explicitly stays pinned. An existing lease keeps its runtime
when a newer one is installed.

## Resource permits

A permit takes pool and member capacity in one transaction.

A changed resource definition never revokes a permit held under the old one.
Definitions that conflict only refuse new permits.

Resource hooks run in the daemon after Shoal records a permit and before it
frees one. A failed hook keeps the permit, so Shoal never loses track of a
resource the hook may have left in use.
[Resource hooks](../reference/resources.md#resource-hooks) says when they run
and what they receive.

### Related repositories

A resource of kind `repo` lends a registered repository's live checkout for
reading. It may name the repository by a remote URL, which keeps tracked config
independent of each machine's registration names.
[Related repositories](../reference/resources.md#related-repositories) says
what the lease binds and what Shoal leaves to the reader.

## Containers

A workspace's container project is a lease. Shoal records the project with its
runtime before any `compose` call. It thus only ever stops or removes a
project it recorded.

Each call reads the Compose file while the worktree has it, with the
workspace's ports in their variables, as podman-compose needs. A stop or
removal that fails with the file runs again by the project's name. A deleted
worktree or a broken file thus never keeps a project running.

A project that is not stopped keeps its workspace from cleanup, since a failed
`up` can leave some services running. Removal takes the project down in the
[removal path](cleanup.md#the-removal-path).

## Secrets

`[secrets]` gives values to agents. The process that launches an agent reads
them. A value goes into the agent's environment, or for listed tools through
private `PATH` launchers on each run of the tool. A launcher takes a written
value from the daemon, so no file holds it.

Of these values, the daemon reads only a
[webhook trigger's](../reference/triggers.md), to check each request's
signature. No state keeps a value.

A tool's secret can require approval. The workspace's grant binds the
secret's value, a written one as its digest, and its tools, so a changed secret
needs a new grant. [Secret approval](../reference/agents.md#secret-approval)
says how a launcher checks it.

### Values from outside the config

Every setting that takes its value from outside the config, such as a secret
or a proxy's upstream key, takes the same
[value forms](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#values).
Shoal prints no written value.

A `{ cmd }` with `on` runs on the joined machine it names. A credential set up
on one machine thus serves every machine.
[Settings from another machine](../reference/hub.md#settings-from-another-machine)
says how a value travels.

- Only a machine's daemon asks the hub for a value, never a device.
- A scoped process asks its own daemon by the setting's name. That daemon runs
  only the command its own config gives.
- A value may cross the hub link to the machine that asked for it. That machine
  keeps the last value in memory only, for while the named machine cannot be
  reached.
- It drops the value when its daemon exits, when it leaves the hub or learns
  that it was revoked, and when the forge refuses the value.
- The hub routes every request, so it records which settings each machine
  fetched from which. Revoking a machine thus names the settings to rotate,
  even while the machines that served them are offline.

A machine that should not use a shared credential sets the key in its own
file, which overrides `shared.toml` key by key. A machine not trusted with
credentials is not joined, since joined machines run agents as the user on
each other.

## Who holds a lease

Simulator, container and permit leases record their acquirer, as
[who holds a lease](../reference/resources.md#who-holds-a-lease) says. A
scoped caller acquires for the space whose agent it runs or that agent
started, never for its execution, so a lease survives the agent's restore in a
new execution.

Ports, holds and secret grants keep lifetimes of their own.

### An agent's leases

An agent's lease ends once the space's agent ends for good. Capacity the agent
no longer uses would otherwise keep other workspaces waiting and its workspace
from removal.

The lease outlives the agent's sleep, a restore and a restart. An agent that
sleeps after most turns would otherwise keep giving up slow resources and
acquiring them again.

One daemon operation releases an acquirer's leases, through each kind's own
release. It runs once a demo's execution has ended, or once an agent's space
has no agent execution, queued restore or restart left.

A release waits while another lifecycle or resource operation holds the
lease's workspace, such as the stop that ended the agent or a resource hook.
That stop then releases the leases of the agents that ended in its space or
hold leases there. The cleanup sweep, which also runs as the daemon starts,
does the same in every workspace.

A failed release hands the lease to the user. A failing hook thus does not
run again on every sweep.

### Demos

A demo outlives the agent that starts it. A simulator demo therefore takes
over the lease an agent acquired for it. What a demo's command acquires
belongs to the demo. Both end with the demo.

## Leases of asleep agents

An asleep agent's simulator or containers keep running and cost the machine.
The cleanup sweep stops them once the agent has slept past
[`[sleep] keep_leases_minutes`](../reference/resources.md#leases-while-asleep).
A permit costs nothing while held, so it stays.

Under memory pressure, overload protection stops such a lease before it stops
an agent, since an asleep agent loses less than a running one.
[Leases of asleep agents](../reference/load.md#leases-of-asleep-agents) gives
the order.

### Taking a lease from an asleep agent

A request that capacity keeps busy takes a permit or simulator an asleep agent
keeps. It does not wait for an agent that may sleep for hours.

The claim's transaction names the sets of held leases any of which would grant
it. For a permit, a set is a single permit or every reader on one member. For
a simulator, it is every leased device the limit it met counts.

The request takes the set that asleep agents wholly hold and that may yield to
it, choosing the set whose highest-ranked holder ranks lowest. It releases
that set through the normal release, then claims again. A simulator request
does this under the lock its allocation holds, so it plans over the released
device before any other request.

An agent yields to a request for a space of no lower priority, and to any of
the user's. Shoal does not wake it to say so: its next turn's message
[names the lease](../reference/resources.md#leases-taken-back).

## Queued requests

A busy resource or simulator request may queue. The daemon keeps the queued
requests in arrival order: one queue per pool, and one for simulators.

While any request is queued, only the earliest claims new capacity, even when
capacity is free for another. A request for a busy member, or a writer, thus
holds back requests that could proceed, so no request starves.

The queue lives in memory. A daemon restart loses the order, and the commands
still polling join again.
[Queued requests](../reference/resources.md#queued-requests) says which
requests ignore the queue and how long a place lasts.
