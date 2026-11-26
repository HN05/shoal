# Shared resources

This file says what `shoal acquire`, `release`, `leases`, `catalog` and
`history` do for ports, simulators, containers, resource pools and related
repositories. [Leases](../design/leases.md) gives the reasons.

## Acquire

`shoal acquire <kind>` leases one of these for the current workspace, or for
the one `--space` names:

- a [port](#ports);
- a [simulator](#simulators);
- the workspace's [containers](#containers);
- a [resource permit](#resource-pools);
- a [related repository](#related-repositories).

`shoal acquire secret` instead asks for access to a
[secret that requires approval](agents.md#secret-approval), and
[`shoal acquire hold`](removal.md#holds) keeps a space from cleanup.

Simulator, resource and repository leases take `--name`, `default` when
omitted. A workspace holds several leases of one kind under different names.
Repeating a name returns the lease it already holds.

`--reason` says what the lease is for. `--queue` waits, without a time limit,
until the lease is granted or its approval is denied. The
[queue](#queued-requests) section says in what order queued requests get
capacity.

A request that capacity keeps busy exits 2, unless `--queue` waits for it.

A lease belongs to the workspace. It outlives the command that acquired it
and daemon restarts. [What keeps a workspace](removal.md#what-keeps-a-workspace)
says which leases keep it from cleanup.

## Who holds a lease

A simulator, containers, resource or repository lease records who acquired
it: the user, an agent or a demo.

The user's lease is one acquired from an unscoped terminal. It stays until
`shoal release` releases it or the workspace is removed.

An agent's lease is one the agent, or a command it started, acquired for its
own space or a space below it. It belongs to the agent of that space. It ends
when that agent ends for good:

- the agent quits on its own;
- `shoal stop` stops it;
- its space is removed or converted;
- its restore is cancelled.

An agent's lease outlives the agent's [sleep](#leases-while-asleep), a
restore that starts it in a new execution, and `shoal restart`.

A [demo's](demos.md) lease ends with the demo, once its execution exits or is
stopped.

When the user acquires an agent's lease again, it becomes the user's. When an
agent acquires the user's lease, it stays the user's.

Shoal releases an ended lease as `shoal release` does:

- a simulator keeps its apps and data;
- containers stop and keep their volumes;
- a permit runs `hooks.pre_resource_release`.

A release that fails keeps the lease as the user's. It records a
`release_failed` alert that names the command that releases it.

Port reservations, holds and secret grants have their own lifetimes, which
their sections give.

## Leases while asleep

An [asleep](sleep.md) agent keeps its leases.

A simulator or containers lease keeps something running. Shoal stops it once
the agent has slept for
[`[sleep] keep_leases_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#sleep).
The count starts when the agent's harness stopped. Waking the agent while its
restore still waits in the agent queue does not restart the count. A stopped
simulator shuts down and keeps its apps and data.

Resource and repository permits stay until the agent ends.

Under memory pressure, [overload protection](load.md#leases-of-asleep-agents) stops an asleep agent's
simulator or containers sooner, before it stops any agent.

### Taken for a busy request

A request that capacity keeps busy takes the lease it needs from an asleep
agent, whether it waits with `--queue` or not. It takes:

- a resource or repository permit on the member it needs, or on any member
  when the pool is full;
- for a writer, every reader in its way, only when each of those readers is
  asleep;
- a simulator: a booted one under `max_booted`, or any one under
  `max_devices`.

Containers are never taken: no other space can use them.

Shoal releases each taken lease as `shoal release` does. It then allocates the
request as usual, so the request gets the device that fits it best.

The asleep agent whose space ranks lowest in the
[agent queue](queue.md#queue-order) loses its lease first. A request takes
only from an agent whose space's priority is no higher than that of the space
the request is for. Your own request takes from any asleep agent. A queued
request takes nothing until the requests that arrived before it are served.

### Leases taken back

The agent's first message once it runs again names each lease Shoal took back
while it slept, and why. The message gives the `shoal acquire` that gets the
lease back, which waits its turn as any request does.

## Queued requests

Queued requests get capacity in arrival order. Each pool keeps its own order,
and simulators share one.

While any request is queued, every other request for new capacity is busy,
even when capacity is free. Renewing a held lease and requesting approval are
not affected.

A request keeps its place while it is in progress or has polled in the last
30 seconds. Stopping the command gives up its place.

The daemon keeps the queue in memory. A daemon restart loses the order, and
the commands still polling join again.

## Release

`shoal release` releases every lease and active approval request the
workspace holds. It releases them in order and stops at the first failure. It
leaves holds, which `shoal release hold` releases.

These narrow it:

- a kind releases only that kind;
- a resource or repository name releases only that pool;
- `--name`, or a port's name, releases one lease.

A release that selects nothing fails.

## List leases and what they draw from

`shoal leases` shows the workspace's leases of every kind. Each simulator,
containers and resource lease shows who acquired it: `user`, `agent` or
`demo`. A kind shows only that kind, and lists repositories apart from other
resources. `--all` covers every workspace.

`shoal catalog` shows what each kind's leases draw from: its configuration
and what the machine offers. It shows the current workspace's, the one
`--workspace` names, or one a picker offers. A kind shows only that kind.

## Ports

```sh
shoal acquire port web --space 3 --reason "Frontend dev server"
shoal acquire port api --port 3001 --env API_PORT --reason "HTTP API"
shoal catalog port --workspace 3        # Automatic range, named ports and conflict policy
shoal leases port --space 3             # Current reservations
shoal leases port --all                 # Every workspace
shoal release port web --space 3
shoal run --workspace 3 -- sh -c 'my-server --port "$API_PORT"'
```

A port reservation is a named TCP port. It outlives `shoal stop`, and a
successful removal releases it.

Repeating a name returns the same port, and `--reason` updates its reason.
Changing the port or its variable needs a release first.

The workspace's environment exports and its tracked commands receive the port
in `SHOAL_PORT_<NAME>`, or in the variable `--env` names. Running processes
keep their environment. Nested executions drop the parent's port variables.

Without `--port`, Shoal takes a free port from the
[`[ports]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ports)
range. `--port` names any nonzero port. Shoal checks that the port is free on
IPv4 and IPv6, and that no other workspace reserved it.

A named port such as `web` is a `[ports.<name>]` entry. `shoal acquire port
web` reserves it on request, and flags override the entry.

When the preferred port is taken, Shoal suggests a free one:

- on a terminal, a prompt offers to take it;
- with `--json`, the command exits 2 with `reserved: false` and the
  suggestion;
- `--port <suggested>` or `--on-conflict auto` takes it.

Reservations are cooperative: an unrelated process can still bind the port.
UDP is not supported.

## Simulators

```sh
shoal acquire sim                   # Current worktree, configured preference
shoal acquire sim --profile phone --name tests --queue
shoal catalog sim                   # Profiles, limits, device types and runtimes
shoal leases sim                    # Managed devices and their leases; --all for all
shoal release sim --name tests      # Or every simulator lease
```

On macOS, Shoal creates, boots, shares and deletes its own Xcode simulators.
[`[machine.simulators]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#simulators)
sets their limits and profiles, and `shoal config reload` applies them.

`shoal acquire sim` returns a ready device's UDID. Name it explicitly to
`simctl`, or to `xcodebuild -destination 'platform=iOS Simulator,id=<UDID>'`.

### Choose a device

[`[simulators] preferred`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#simulator-preferences)
orders the profiles Shoal tries, and `--profile` or `--device` overrides it.
`allow_any` permits a `--device` no profile names, with a `--reason`.

A profile without `runtime`, or `--device` without `--runtime`, takes the
latest installed, available iOS runtime that supports the device. Shoal
downloads no runtime.

`shoal catalog sim` lists the profiles, the limits and the repository's
preferences, with the installed device types and runtimes. Where `simctl`
cannot run, it lists the configuration with the reason the inventory is
missing.

### Capacity

The workspace's removal deletes its devices.

At capacity, Shoal shuts down idle managed devices first. It never touches a
leased device or a simulator of your own. Every device not confirmed shut down
counts toward the limit.

A released device keeps its apps and settings until its idle timer deletes
it. Shoal checks the timers every 15 seconds.

A failed operation keeps its record, so Shoal retries it.

Shoal covers only the default CoreSimulator device set, used by one daemon.

### Clean devices

```sh
shoal acquire sim --clean --reason "Verify first-launch permission prompts"
shoal history sim                  # --workspace W or --all, --limit up to 50, --before <id>
```

A normal handoff keeps the device's state. `--clean` returns a fresh or erased
device instead, and needs a `--reason`. It cannot erase a leased device. Shoal
prefers spare capacity. Otherwise it erases or replaces the idle device with
the fewest known apps of yours.

`shoal history sim` lists the clean-device requests, busy and failed ones
included. Each entry records:

- the workspace, the execution and the reason;
- the outcome and the device;
- the planned resets and the apps estimated lost;
- whether the erase completed.

The history outlives removals and daemon restarts. A scoped command sees only
its own entries. Erasing a device outside Shoal leaves no entry.

## Containers

```sh
shoal acquire containers --reason "Postgres for the API tests"
shoal leases containers             # The project, its services and their ports
shoal catalog containers            # The configuration, and whether the runtime runs
shoal release containers            # Stop them, keeping their volumes
```

A repository whose development needs services, such as a database or a cache,
names a Compose file in
[`[containers]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#containers).
Each workspace then gets a Compose project of its own. The containers run
beside the workspace, and the agent and its tools stay on the host. Chats
have no containers.

Shoal never reads the Compose file. It runs `docker compose` or
`podman compose`, as
[`[machine.containers]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#container-runtime)
names, with a project name made from the workspace's ID.

### Start and stop

`shoal acquire containers` first reserves the named ports `[containers] ports`
lists, as `shoal acquire port` does. It records the project, then runs
`compose up --detach --wait` in the worktree with each port in its variable.
The file maps a port as `"${SHOAL_PORT_DB}:5432"`.

When one of those ports waits for approval, or its preferred port is taken,
the command exits 2 with the port's request or suggestion, and nothing starts.

The command returns the services with the ports they published, as the
runtime lists them. Repeating it returns the running project.

`shoal release containers` runs `compose stop`. It keeps the volumes and the
port reservations, and the next acquire starts the same project again.

### Start with setup

With `start = "setup"`, setup starts the project once its command readies the
workspace. The project starts before the post-setup hook and before the agent
that waits for setup. A failure marks the workspace failed, and `shoal setup`
retries it.

The project then belongs to the workspace's own agent once it starts, and
until then to whoever created the workspace. An agent that starts in such a
workspace while the project is stopped starts it first.

### Lifetime

The project is a lease, held as [who holds a lease](#who-holds-a-lease) says.

Removal runs `compose down --volumes --remove-orphans` after
`hooks.pre_remove`. It runs with the Compose file while the worktree has it,
and by the project's name when the worktree has none or the file does not
read. A failure keeps the workspace.

Checkpoints keep no volume data, so a restored workspace starts fresh
containers.

The project's record outlives daemon restarts, and the containers keep running
through one. Shoal never stops or removes a project it has no record of.

### Usage

A running project's CPU and memory count in the workspace's
[usage](workspaces.md#rows). The runtime's `stats` reads the containers
Compose labels with the project's name. While the runtime cannot read them,
the usage counts the workspace's processes alone.

## Resource pools

```sh
shoal catalog resource                       # Pools, members and approval settings
shoal leases resource                        # Usage and own leases
shoal acquire resource devices               # Any available member
shoal acquire resource devices --member beta --name tests --reason "Integration tests"
shoal acquire resource signing --queue
shoal leases resource --all                  # Every workspace
shoal release resource devices --name tests
```

[`[resources]` and `[resource_pools]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#resource-pools)
define resources. The global config's are shared across repositories. A
repository's are shared across its workspaces, so the same name in another
repository is another resource.

Each lease takes one permit from the pool and one from the member. A member
named with `--member` never changes to another. Repeating a pool and lease
name returns the permit already held. With `--json`, a busy request exits 2
with `acquired: false`.

A successful removal releases a permit.

A global change applies on `shoal config reload`. Shoal reads the repository's
config on each request. Definitions that conflict refuse new permits until
they agree or their leases are released.

Shoal counts permits only. Stop using a resource before you release it.

### Shared readers and exclusive writers

`kind = "rwlock"` on a resource or pool member makes it a lock. Acquire it
with `-m`/`--mode` `read` or `write`. A new lock lease defaults to `write`, and
a semaphore's mode is `permit`.

Readers hold it together and share one pool permit, which the last release
frees. A writer holds it alone. Its capacity must be 1.

Repeating a lease name returns its mode. Changing mode needs a release first.
Writers get no priority over readers.

`shoal leases resource` shows the reader and writer counts, with read and
write availability apart.

### Related repositories

A [resource of kind `repo`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#related-repositories),
in the global or the repository's config, names a registered repository by
its name, ID or remote URL. A URL matches the registration with the same
remote over any transport. Shoal never clones it: register it first with
`shoal repo add <url>`.

`shoal catalog repo` lists them. `shoal acquire repo server` returns a read
lease, with `repository.id` and `repository.path` in its JSON. A scoped caller
acquires one without the `admin` permission. Only `read` leases exist, and
readers share the one permit.

Approvals and resource hooks apply as for other permits. An approval binds the
repository's ID and path.

The path is the registered checkout's live working tree, for cooperative
reading. Shoal sets no file permissions, fetches nothing, selects no ref and
creates no other checkout. Name the path in a project's repository override,
and read committed files at a chosen ref with Git.

A checkout that is missing, or a repository being removed, cannot be leased.
Acquiring the lease again keeps its recorded repository even when the config
changed.

While another repository's workspace holds a lease, the repository cannot be
removed. A release or the workspace's removal only drops the lease, and leaves
the checkout as it is.

### Resource hooks

[`hooks.post_resource_acquire` and `hooks.pre_resource_release`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hooks)
run for resource permits, by the [hook path and environment rules](setup.md).
They run in the daemon without a terminal, for at most 60 seconds.
`SHOAL_RESOURCE_LEASE` holds the lease as JSON, with its `id`, `scope`,
`pool`, `resource`, `name`, `mode`, `reason` and `acquirer`.

Acquisition records the permit before it runs its hook, also when it returns a
permit already held. A busy request runs no hook. When the hook fails, the
command fails and the permit stays: acquire again to retry, or release it.

Release runs its hook before it frees the permit. When the hook fails, the
permit stays.

A workspace's removal runs the release hooks after `hooks.pre_remove`, and
keeps every permit until the removal succeeds. Shoal skips the hooks when the
worktree is already gone.

A hook may run again for the same lease, and a reader's hook runs once per
lease, not once per member. Write hooks that tolerate both. A hook may read
Shoal's state. While it runs, other permit and lifecycle changes in its
workspace fail.
