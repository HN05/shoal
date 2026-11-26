# Leases

This file covers `shoal-leases`: secrets, ports, simulators, resource permits
and containers, and the verbs that lease them.

`[secrets]` gives values the CLI reads, for an agent's environment at launch or,
for listed tools, through private PATH launchers each time the tool runs, which
take a written one from the daemon so no file holds it; the daemon reads only a
webhook trigger's, to check each request's signature, and no state keeps a
value: a value may cross the hub link to the joined machine that asked for it,
which keeps it only in memory; hooks drop them. A tool secret may require
approval: the launcher reads it only after the daemon confirms a workspace grant
bound to its value, a written one as its digest, and tools, and fails instead of
waiting. Environment secrets cannot require approval, since they are fetched
before the agent can ask. A setting that takes its value from outside the
config, such as a secret or a proxy's upstream key, shares one form: as written,
`{ env }` from the agent's environment, or `{ cmd }` run when the agent starts,
and Shoal prints no written value. A `{ cmd }` with `on` runs on the joined
machine it names, so a credential set up there serves every machine: only a
machine's daemon asks for it through the hub, never a device, and a scoped
process asks its own daemon by the setting, whose config gives the command. The
machine that asked keeps the last value in memory for while the named machine
cannot be reached, and drops it when its daemon exits, it leaves the hub or
learns it was revoked, or the forge refuses it. The hub, which routes every
request, records which setting each machine fetched from which, so that revoking
a machine names the settings to rotate even while the machines that served them
are offline. A machine that should not use a shared credential sets the key in
its own file, which overrides `shared.toml` per key; one not trusted with
credentials is not joined, since joined machines run agents as the user on each
other.

Reservations and leases belong to a worktree, survive command exit, restarts,
and failed removal, are allocated atomically with idempotent lease names, and
are released explicitly or by successful removal. Resources are lazy, never
claimed at creation.

Ports are cooperative TCP reservations: probe, record, export to later
executions, never hold a socket. Simulator leases are exclusive over
Shoal-created devices only: persist claims before mutating, keep them after
failure, never preempt active leases, count external devices toward capacity,
preserve device state on handoff, require `--clean --reason` for erasure with
an audit record written before the destructive step, and delete devices on
removal or idle expiry. Workspace lookups index the JSON record's current owner,
falling back to its last owner only when unclaimed; SQLite maintains the index
on every write and rejects malformed ownership fields, and journals a lease when
its record enters or leaves the leased state, or goes while leased. Allocation planning is pure
over recorded devices and inventory; its executor revalidates ownership and live capacity under the simulator
gate before ordered audit, claim and simctl operations. Native simulator states
use an adapter enum of their own that keeps unfamiliar strings and simctl's
spellings; only a confirmed shutdown frees running capacity. An omitted runtime selects
the latest installed, available iOS version compatible with the requested device;
explicit runtimes stay pinned and existing leases keep their runtime. Generic permits consume
pool and member capacity in one transaction; rwlock members allow unlimited readers sharing one slot or one
writer, default to write, and require release to change mode. Definition drift
blocks new claims but never revokes permits. `kind = "repo"` resources name a registered repository, or a remote URL matching
one, and return its checkout's path with cooperative read access; readers share one slot.
A URL keeps tracked config independent of each machine's registration names.
The lease binds its repository ID and path, including for approval, survives
restart, and prevents repository deletion while borrowed by another repository's
workspace. Release and workspace cleanup only drop the lease; they never modify
the borrowed checkout. The view is live, with no automatic fetch or pinned ref.
Optional daemon hooks run after a
permit is persisted and before it is released; failure retains ownership.
Repeated acquisition reruns its hook against the same lease, so scripts must be
idempotent. Hooks receive lease JSON, use normal config precedence and executable
path rules, and cannot overlap permit or lifecycle transitions in their workspace.
Release hooks also run in the shared removal path while the worktree exists;
all permits remain owned until removal succeeds.
Simulator leases and permits record their acquirer: the user for an unscoped
caller, else the space whose agent the caller runs or was started by, not its
execution. An agent's lease ends once that space's agent ends for good, since
capacity it no longer uses would otherwise keep other workspaces waiting and its
workspace from removal, but not when it sleeps, restores in a new execution or
restarts: an agent that sleeps after most turns would otherwise keep giving up
and reacquiring slow resources. One daemon operation releases an acquirer's
leases through each kind's release once a demo's execution's end is recorded,
or once an agent's space has no agent execution, queued restore or restart
left. An asleep agent's lease that keeps something running, a booted
simulator or started containers, still costs the machine, so the cleanup
sweep stops it once the agent has slept past `[sleep] keep_leases_minutes`,
counted from when its restore entered the agent queue, which it leaves only as
the agent runs again; permits cost nothing while held and stay. Under memory
pressure, overload protection stops one such lease, the lowest-ranked asleep
agent's first, wherever it would stop an agent, and stops an agent only once
no asleep agent holds one: an asleep agent loses less than a running one. A request that capacity keeps busy
takes a permit or simulator an asleep agent keeps instead of waiting for an
agent that may sleep for hours: the claim's transaction names the sets of held
permits any of which would grant it, a single permit or every reader on one
member, a simulator request every leased device the limit it met counts, and the
request takes the set wholly held by asleep agents that may yield to it whose
highest-ranked holder ranks lowest, releases it through the normal release and
claims again, a simulator under the lock its allocation holds, so the request
plans over the released device before any other. An agent yields to a request for a space of no lower priority, and
to any of the user's. The agent reads
each lease Shoal took back in a message that waits for its next turn without
waking it. A lease whose workspace another lifecycle or resource operation holds then, such
as the stop that ended the agent or a resource hook, waits for it to end; the
stop then releases the leases of the agents that ended in its space or hold
leases there, and the cleanup sweep, which also runs as the daemon starts, in
every workspace. A failed release hands the lease to
the user, so a failing hook does not rerun on every sweep. The user's lease
stays until released; the user's acquire takes an agent's lease over, never the
reverse. A demo outlives the agent that starts it, so a simulator demo takes
over the lease an agent acquired for it, and what a demo's command acquires is
the demo's; both end with the demo. A workspace's container project is such a
lease: recorded with its runtime before any `compose` call, so Shoal only ever
stops or removes a project it recorded, stopped on release with its volumes
kept, and taken down in the shared removal path. Each call reads the file,
with the workspace's ports in their variables, while the worktree has it, as
podman-compose needs; a stop or removal that fails with it runs again by the
project's name, so a deleted worktree or a broken file never keeps a project.
A project that is not stopped keeps its workspace from cleanup, since a failed
`up` can leave some services running. Chats have no containers, since they run
nothing on the machine. One setup starts for
the workspace's own agent stays the user's until that agent starts and takes
it over. Ports, holds and secret grants keep their own lifetimes. User scripts own integrations
with the underlying resources.

Busy resource and simulator requests may queue. The daemon keeps queued lease names
in arrival order per pool, and one queue for simulators; while any remain, only the
earliest may claim new capacity, so every other request is busy even when capacity
is free. Strict order lets a request for a busy member or a writer hold back requests
that could proceed, so no request starves. Renewals and approval requests ignore the
queue; grants, approval outcomes and errors give up the place. Queued commands poll,
and a place lasts while a request is in progress or polled within 30 seconds. The
queue lives in memory, so a daemon restart loses the order; commands still polling
join again.

Leases use top-level verbs that take the kind first: `acquire <kind>`,
`release [kind [item]]` and `leases [kind]`, with `--space` instead of a
positional space. Like `unlink`, omitting the item or kind widens release
to every lease of that kind or in the workspace. Holds use the same verbs but
keep a space, a workspace or a chat, rather than share capacity; releasing leases without a kind
preserves holds. `leases` shows leases and the usage that limits new ones;
`catalog [kind]` shows the configuration and machine inventory they draw from,
secrets without their commands; `history <kind>` shows a kind's audit trail,
starting with clean-device requests. There is no separate `sim` group.
All-workspace overviews that need independent per-workspace reads use bounded
concurrency, retain workspace order and report every failure.
