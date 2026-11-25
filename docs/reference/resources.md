# Shared resources

This file says what `shoal acquire`, `release`, `leases` and `catalog` do for
ports, simulators, containers and resource pools.

## Leases

`shoal acquire <kind>` reserves a port, simulator, resource permit, or related
repository for the current workspace, or the one named by `--space`, starts
its [containers](#containers), or requests access to a
[secret](agents.md#agent-secrets) that requires approval.
Simulator, resource, and repository leases take `--name` (default `default`) to
hold several, `--reason`, and `--queue` to wait, without a time limit, until the
lease is acquired or its approval denied. `acquire repo` selects only `kind = "repo"` members.

A simulator, container, resource or repository lease records who acquired it. The user's,
from an unscoped terminal, stays until `shoal release` or the workspace's
removal. An agent's, acquired by the agent or a command it started, for its
own space or one below, belongs to its space's agent and ends when that agent
ends for good: when it quits on its own or `shoal stop` stops it, when its
space is removed or converted, or when its restore is cancelled. It outlives
the agent's [sleep](#leases-while-asleep), a restore that starts it in a new
execution and `shoal restart`. A [demo's](demos.md) ends with the demo, once its
execution exits or is stopped. Shoal releases an ended lease as `shoal
release` does, so a simulator keeps its apps and data, containers stop with
their volumes kept and a permit runs `hooks.pre_resource_release`. A failed release keeps the
lease as the user's and records a `release_failed` alert naming the
command that releases it. The user acquiring an agent's lease again makes it the
user's; an agent acquiring the user's leaves it the user's. Ports, holds and
secret grants keep their own rules.

### Leases while asleep

An [asleep](sleep.md) agent keeps its leases, so an agent that sleeps after most
turns does not give up and reacquire slow resources, such as a simulator Shoal
must boot again or containers it must start again. A lease that keeps something
running, a simulator or containers, stops once the agent has slept past
[`[sleep] keep_leases_minutes`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#sleep),
30 by default, counted from when its harness stopped; `0` stops them as it goes
to sleep, and waking it while its restore still waits in the agent queue does
not restart the count. A stopped simulator shuts down with its apps and data
kept. Resource and repository permits stay. Under memory pressure,
[overload protection](load.md) stops them sooner, before it stops
any agent.

A request that capacity keeps busy, waiting with `--queue` or not, takes what
frees it from asleep agents: a resource or repository permit on the member it
needs, or on any member when the pool is full, and for a writer every reader in
its way, only when each of them is asleep; and a simulator, a booted one under
`max_booted` and any under `max_devices`. Shoal releases them as `shoal
release` does, running `hooks.pre_resource_release` and keeping a simulator's
apps and data, then allocates the request as usual, so it gets whichever
device fits it best. The asleep agent whose space ranks lowest in the
[agent queue](queue.md#queue-order) loses its lease first, and only one whose space's
priority is no higher than that of the space the request is for, its own or
the one `--space` names; your own request takes from any asleep agent. A
queued request takes nothing before the requests that arrived earlier are
served. Containers are never taken for a request: no other space can use them.

The agent's first message once it runs again names each lease Shoal took back
while it slept and why, with the `shoal acquire` that gets it back, which
takes its turn as any request does. Taking it back runs the lease's release as
`shoal release` does; a release that fails keeps the lease as the user's and
records `release_failed`.

Queued requests get capacity in arrival order, per pool and for simulators. While
any are queued, every other request for new capacity is busy, even when capacity is
free; renewing a held lease and requesting approval are not affected. A request
keeps its place while it is in progress or has polled in the last 30 seconds, so
stopping the command gives the place up. The daemon keeps the queue in memory: a
restart loses the order, and commands still polling join again.

`shoal release` releases every lease and active approval request the workspace
holds, in order, and stops at the first failure. A kind narrows it to that kind, a resource or repository name
to its pool, and `--name` or a port name to one lease; selecting nothing fails.
Holds are released separately.

`shoal leases` shows every kind's leases, each simulator, container and
resource lease with who acquired it: `user`, `agent` or `demo`; a kind shows
only that kind, with repositories listed apart from other resources, and `--all`
covers every workspace. `shoal catalog` shows what each kind's leases draw from,
its configuration and what the machine offers, for one workspace (`--workspace`,
otherwise the current one or a picker); a kind shows only that kind.

## Port reservations

```sh
shoal acquire port web --space 3 --reason "Frontend dev server"
shoal acquire port api --port 3001 --env API_PORT --reason "HTTP API"
shoal catalog port --workspace 3 # Automatic range, named ports and conflict policy
shoal leases port --space 3  # Current reservations
shoal leases port --all                 # Every workspace
shoal release port web --space 3
shoal run --workspace 3 -- sh -c 'my-server --port "$API_PORT"'
```

Named TCP reservations belong to the worktree, persist across command exits, `stop`,
and restarts, and are released by successful removal. Repeating a name returns the
same port (`--reason` may update it); changing the number or variable requires
release first. Workspace environment exports and tracked commands receive
`SHOAL_PORT_<NAME>` or the `--env` variable; running processes keep their
environment, and nested executions drop the parent's port variables. Automatic
allocation uses [`[ports]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ports) `start`–`end`, 49152–65535 by default; `--port` may name any nonzero port. Shoal probes IPv4/IPv6 availability and
prevents duplicates within the daemon, but reservations are cooperative and
unrelated processes can still bind. UDP is not supported.

Named ports such as `web` are [`[ports.<name>]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#ports) entries.
`shoal acquire port web` allocates on request; CLI flags override. A conflict
suggests a free port: fzf offers to accept it, `--json` returns `reserved:
false` with exit 2, and `--port <suggested>` or `--on-conflict auto` accepts.

## Simulators (macOS)

Shoal creates, boots, shares, and deletes its own Xcode simulators.
[`[machine.simulators]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#simulators) sets their limits and profiles,
and `config reload` applies them.

Omitting `runtime` from a profile or `--runtime` with `--device` selects the latest
installed, available iOS runtime compatible with the device. `shoal catalog sim`
lists the profiles, limits and the repository's preferences with the installed
device types and runtimes; nothing is downloaded. Where simctl cannot run, it
lists the configuration with the reason the inventory is missing.
[`[simulators] preferred`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#simulator-preferences) orders the profiles;
flags override it, and `allow_any` permits unconfigured `--device` requests with
a `--reason`.

```sh
shoal acquire sim                   # Current worktree, configured preference
shoal acquire sim --profile phone --name tests --queue
shoal catalog sim                   # Profiles, limits, device types and runtimes
shoal leases sim                    # Managed devices and their leases; --all for all
shoal release sim --name tests     # Or every simulator lease
```

Acquisition returns a ready device UDID; use it explicitly with `simctl` or
`xcodebuild -destination 'platform=iOS Simulator,id=<UDID>'`. Repeating a lease name
returns the same device. Leases belong to worktrees, survive command exit and
restarts, block automatic removal, and are deleted with the workspace; an
agent's [ends with the agent](#leases). At capacity,
idle managed devices shut down first; active leases and personal simulators are
never touched, every device not confirmed shut down counts toward the limit, and busy requests
exit 2 unless `--queue`. Released devices keep apps and settings until the
idle timer deletes them (checked every 15 seconds). Failed operations retain records
for retry. Only the default CoreSimulator device set and one daemon are covered.

### Clean devices and audit history

```sh
shoal acquire sim --clean --reason "Verify first-launch permission prompts"
shoal history sim                  # --workspace W or --all, --limit 50, --before <id>
```

Normal handoff preserves device state. `--clean` returns a fresh or erased
device, needs a reason, and cannot erase an active lease. Shoal prefers spare
capacity, otherwise erases or replaces the idle device with the fewest known
user apps. History records workspace, execution, reason, outcome, device,
planned resets, estimated app loss, and whether the erase completed, including
busy and failed attempts; it survives removal and restarts, and scoped commands
see only their own entries. Erasing devices outside Shoal bypasses this audit.

## Containers

A repository whose development needs services in containers, such as a
database or a cache, names its Compose file in
[`[containers]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#containers),
and each workspace gets a Compose project of its own, so parallel workspaces
never share or collide on them. The containers run beside the workspace: the
agent and its tools stay on the host. Shoal never reads the file; it runs
`docker compose` or `podman compose`, as
[`[machine.containers]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#container-runtime)
names, with a project name derived from the workspace ID. Chats have no
containers, since they run nothing on this machine.

```sh
shoal acquire containers --reason "Postgres for the API tests"
shoal leases containers             # The project, its services and their ports
shoal catalog containers            # The configuration, and whether the runtime runs
shoal release containers            # Stop them, keeping their volumes
```

`acquire containers` reserves the named ports `[containers] ports` lists, as
`shoal acquire port` does, records the project, then runs `compose up --detach
--wait` in the worktree with each port in its variable, so the file maps
`"${SHOAL_PORT_DB}:5432"`. A port that waits for approval, or whose preferred
port is taken, exits 2 with the port's request or suggestion, and nothing
starts. It returns the services with the ports they published, as the
runtime lists them; repeating it returns the running project. `release
containers` runs `compose stop`, keeping the volumes and the port
reservations, and the next acquire starts the same project again.

With `start = "setup"`, setup starts the project once its command readies the
workspace, before the post-setup hook and the agent that waits for setup; a
failure marks the workspace failed, and `shoal setup` retries. The
project is then the workspace's own agent's, once it starts, else whoever
created the workspace's. An agent that starts in such a workspace while the
project is stopped starts it first.

The project is a [lease](#leases): an agent's stops when the agent does, the
user's runs until released, and a project that is not stopped keeps the
workspace from automatic removal, since a failed `up` can leave some services
running. Removal runs `compose down --volumes --remove-orphans` after
`hooks.pre_remove`, with the file while the worktree has it, then by the
project's name when it has none or the file no longer reads, so a deleted
worktree or a broken file still removes it; a failure keeps the workspace. Checkpoints keep
no volume data: a restored workspace starts fresh containers. The record
survives daemon restarts, during which the containers keep running, and Shoal
never stops or removes a project it has no record of. A running project's CPU
and memory count in the workspace's usage, as the runtime's `stats` reads the
containers Compose labels with the project's name; while the runtime cannot
read them, the usage is its processes' alone.

## Cooperative resource pools

Global config defines resources shared across repositories; repository config
defines resources shared across that repository's branches, both as
[`[resources]` and `[resource_pools]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#resource-pools) entries.

```sh
shoal catalog resource                       # Pools, members and approval settings
shoal leases resource                        # Usage and own leases
shoal acquire resource devices               # Any available member
shoal acquire resource devices --member beta --name tests --reason "Integration tests"
shoal acquire resource signing --queue
shoal leases resource --all                  # Every workspace
shoal release resource devices --name tests
```

Each lease takes one slot from the pool and the chosen member; an explicit member
never changes silently. Repeating a pool and lease name (default `default`) returns
the existing permit. `--json` returns the lease or `acquired: false` with exit 2;
`--queue` waits in arrival order. Repository pools are keyed by repository, so equal names
elsewhere are independent. Global changes apply on reload, repository config is read
per request, and conflicting definitions block new claims until they agree or leases
drain. Leases survive command exit and restarts, block automatic cleanup, and are
released by successful removal; an agent's [end with the agent](#leases). Shoal accounts for permits only; stop using a
resource before releasing it.

### Related repository paths

A [resource of kind `repo`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#related-repositories), in global or
repository config, names a registered repository. A URL keeps tracked config independent of local registration names. It matches
the registration with the same remote over any transport, and is never cloned:
register it first with `shoal repo add <url>`.

`shoal catalog repo` lists them; `shoal acquire repo server` returns a read lease with `repository.id` and
`repository.path` in JSON. Scoped callers can acquire it without repository
administration access. New leases default to `read`; other modes are rejected.
Capacity must be 1, and readers share one pool slot. Resource approvals and hooks
apply normally; approvals bind the resolved repository ID and path.

The path is the registered checkout's live working tree, with cooperative read
access. Shoal does not enforce filesystem permissions, fetch, select a ref, or
create a sibling path. Use the returned path in a project's repository override;
use Git to read committed files at a chosen ref. Missing checkouts and repositories
being removed cannot be acquired. Repeated acquisition preserves the recorded
binding even if configuration changes. A lease prevents repository deletion while
another repository's workspace holds it; release and workspace removal only
discard the lease, leaving the related checkout untouched.

### Resource hooks

Global or repository config can name [`hooks.post_resource_acquire`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hooks) and
`hooks.pre_resource_release`, using the [hook path and environment rules](setup.md).
They apply to generic permits, run in the daemon without a terminal, and have a
60-second limit. `SHOAL_RESOURCE_LEASE` contains the lease as JSON, including its
`id`, `scope`, `pool`, `resource`, `name`, `mode`, `reason` and `acquirer`.

Acquisition records the lease before running its hook, including when an existing
lease is returned; busy requests run no hook. A failed acquisition hook returns
an error but retains the permit: repeat acquisition to retry, or release it.
Release runs its hook before freeing capacity; failure retains the lease.
Workspace removal runs release hooks after `hooks.pre_remove`, keeping every lease
until removal succeeds. Hooks are skipped when the worktree is already missing.
Make hooks idempotent: retries can repeat them, and reader hooks run per lease,
not once per shared member. Hooks may inspect state, but overlapping permit or
lifecycle changes in the same workspace fail while a resource hook is running.

### Shared readers and exclusive writers

Set `kind = "rwlock"` on a standalone resource or pool member and acquire with
`-m`/`--mode` `read` or `write` (new rwlock leases default to write; semaphores
use `permit`). Readers coexist and share one pool slot, freed by the last
release; a writer excludes everyone. Capacity must be 1. Repeating a lease
name returns its mode; changing mode requires release first, and there is no
writer priority. `shoal leases resource` shows reader and writer counts with
separate read/write availability.
