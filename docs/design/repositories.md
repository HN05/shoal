# Repositories

This file covers `shoal-repositories`: registered repositories and their
directories, contributions, seed builds, presets and shared caches.
[Repositories](../reference/repo.md) says what the `repo` commands and
`shoal contribute` do, and [setup](../reference/setup.md) how seeds, presets,
shared caches and temporary directories work; this file gives the reasons.

## Repository directories

Every repository owns a directory in `machine.root_dir`. Its workspaces go
there by default, and a URL clone lives there as `.checkout`, a name no
workspace can take. So worktrees are grouped per repository, never nested in a
checkout, and lie outside the state directory. A root inside the state
directory, a checkout or a recorded repository directory is refused.

Repository roots and explicit workspace paths resolve existing symlinks and
lexical `..` before their boundary checks and creation, even when trailing
components are missing. The checks thus see the directory that creation
writes.

The daemon reserves each directory atomically and never reuses it. Cleanup
owns a worktree, never its parent.

An explicit workspace path changes where one creation goes. The repository
directory stays as it was. A checkout registered in place at
`~/shoal/<name>/<anything>` adopts that directory.

### Spotlight on macOS

On macOS, a root that Shoal creates is a link to `<parent>/.<name>.noindex`,
which holds its contents. Spotlight skips a directory by that name. It ignores
`.metadata_never_index` below a volume root, and its privacy list needs root,
so only the name keeps build output out of the index. The link keeps
`root_dir` the same path on every system.

Shoal uses an existing root as it finds it. Only `repo move` changes it.

## Registration

Registration is idempotent. It matches a registration by normalized origin
URL, then by canonical path. It never fetches.

Each repository has a stable ID apart from its display name, so a rename
changes no record that refers to it. A selector keeps the caller's argument
beside its canonical path. [Select a repository](../reference/repo.md#select-a-repository)
says which match wins.

## File access that waits

### Writing a new directory

Before registration, workspace creation or an export writes a directory, the
daemon creates a file there itself, under a time limit. The daemon's file
access can differ from the terminal's. On macOS, opening a file on a volume
the daemon lacks access to can wait for a permission prompt no one sees.

### Waits in a worktree

File access in a worktree can wait without end. While macOS waits for an
answer to a privacy prompt, for any process, every new process that opens a
file in the protected location waits too. It waits until someone answers the
prompt or the kernel interrupts it with `Interrupted system call`. Each Git
the daemon starts there waits the same way.

Shoal cannot end the wait. It keeps the wait from holding up unrelated work,
wherever the worktree is and whichever permission it waits on.

The daemon's own reads in a worktree can wait too, such as reading its
ownership marker or whether it exists. No time limit interrupts a read that
waits in the kernel. So requests and sweeps read worktree files on the
blocking pool, where a read holds no runtime thread and a check's time limit
ends the wait for it.

### Stalled directories

The daemon's checks of a worktree run under a time limit and report what they
could not read. They include the startup audit and its recheck, the sweep's
branch observation and cleanup checks, the Git reads of `shoal status`, and
the reads of a checkout's or worktree's config file, which every command that
resolves settings waits on.

A check that runs out of time or is interrupted marks the directory that
holds the worktree as stalled. The daemon reports it once as an alert, and in
`shoal status` until a check there finishes. Heavy memory pressure can slow a
check that much with no prompt waiting. So the report says the check timed
out, and names a prompt as a possible cause only on macOS.

The stalled directory comes from the path alone, since reading the filesystem
could stall too. It groups a repository's workspaces, which a prompt for their
location holds up together.

Checks skip a stalled directory until a recheck is due, and while another
check rechecks it. So each sweep rechecks it once. Work that must not act on
what it cannot read waits for the directory:

- The sweep's cleanup phases skip a workspace or checkout in a stalled
  directory. When a recheck is due, they read its HEAD themselves, since
  branch observation left nothing there for them to read.
- An unattended removal, deleted-worktree cleanup included, is refused while
  a workspace below it is in a stalled directory.
- PR cleanup skips a base while a workspace stacked on it is in one.

### Listings and pickers

Listings read HEAD for review marks under one short limit for the whole list,
and skip stalled directories, leaving staleness unknown. So `shoal attach` and
`shoal list` answer from the daemon's state.

Repository pickers start Git only to tell apart repositories that share a
name. They read those origins at once, under a short limit. A repository whose
origin they cannot read shows its recorded source.

## Moving the root

A repository directory moves only whole, into a new `root_dir`, while nothing
runs in it or uses it. Checkouts and workspaces outside it stay where they
are.

The daemon reserves the directory's workspaces and records the move before
anything moves. Then it switches the records and repairs Git's worktree links.
So a daemon that stopped during a move finishes it once the directory has
arrived, and rolls it back otherwise. A rollback deletes only directories that
carry their recorded identity.

`repo move` stops the commands and agents running there as `shoal stop` does.
It queues the agents' restores for after the move, since a stop keeps
sessions, which resume by ID from any directory. The daemon carries the
agents' trust entries to the new paths.

### Chats without a repository

The chats without a repository run in `root_dir`, so they go with the root.
Their agents stop the same way. Their temporary directories, attached files and
kept session files move into the directory that holds the repositories. Their
records name the configured root, which is that directory or links to it. So
a root that becomes a link is empty once they have moved.

## Removing a repository

`repo rm` binds its confirmation to each machine's resolved registration, by
the ID that machine gives it. So a confirmation never removes a repository the
user did not see listed.

The removal verifies the checkout's identity and boundaries before it deletes
the checkout. It removes the repository directory only once that is empty. It
removes every workspace through the shared removal path. It persists its
progress, and until a retry finishes an interrupted removal, the repository
takes no new workspace.

## Contributions

A contribution is a URL clone registered for work in a repository the user
does not maintain. `origin` stays the repository itself, so issues, PRs and
syncs use it. When the user's login cannot push there, the user's fork becomes
the `fork` remote and `remote.pushDefault`. Git, `pr open` and cleanup all
honour that setting.

An existing registration never becomes a contribution, since its checkout is
the user's own.

### Removal

Cleanup removes a contribution through `repo rm`'s path once nothing of it has
been left for the unattended grace period.
[Remove a contribution](../reference/repo.md#remove-a-contribution) lists
what counts. It keeps anything a later command could use and any work the
remotes lack, since no one confirms the removal.

A running `shoal contribute` holds its contribution through its daemon
connection, in memory, from registration until the command ends. A command
whose daemon restarted holds it again on its new connection. So every
contribution counts as held for the first minute the daemon runs.

The final check records the removal in the same transaction, under the gates
that workspace creation takes. Queue and chat records refuse a repository
being removed. So nothing new starts in it once the check passes.

Removal never deletes the fork. Shoal does not record whether it created it.
Deleting a forge repository cannot be undone, and the fork may hold the only
copy of a PR's branch.

## Temporary directories

Each workspace and chat gets its own temporary directory as `TMPDIR`. It lies
outside the worktree, so it neither dirties the worktree nor enters
checkpoints. It is named by the space's ID, which never changes or comes back
as a name can. Its path stays short enough for the Unix sockets that tools
create in it on macOS.

The directory of a chat without a repository lies in `root_dir`'s `.tmp`. The
daemon marks that `.tmp` with its state directory, so another daemon sharing
`root_dir` leaves it alone.

The directory exists only while its space's record does. The daemon creates
it once the record exists, and the removal of the space deletes it. The
cleanup sweep deletes any directory without a record, with the directories in
it that the owner may not write, since nothing there is worth keeping. The
post-remove hook runs after the directory is gone, so it keeps the inherited
`TMPDIR`.

## Seed builds

A repository's `[setup.seed]` keeps one build of its local default branch that
new worktrees start from. Live workspaces share no build directory: a shared
directory serialises or corrupts concurrent builds. A copy-on-write clone
costs no space until a branch rewrites a file, and needs no support from the
build tool.

The daemon owns a locked, detached seed worktree in the repository directory.
It builds there when the branch moves, then freezes the configured outputs as
a snapshot. So a running build never reaches a workspace.

### Cloning into a new workspace

A clone of a large build tree takes minutes. So creation clones after its Git
operations, outside the repository's Git gate. The workspace is neither ready
nor set up until the clone ends.

The daemon keeps one spare clone of the snapshot ready. Creation moves it into
the new worktree, and a background task clones the next. Creation clones
itself only when no spare is ready.

Clones read the snapshot under the seed's own lock, shared. A swap or deletion
of the snapshot takes it exclusively. So creations and the refill clone at the
same time, and only taking the spare is exclusive. The daemon clones a new
snapshot and deletes the old one outside the Git gate, which it takes only to
swap them.

### Modification times

Tools that compare modification times treat every freshly checked-out file as
new. So creation gives the seed's modification times to the tracked files its
commit left unchanged. It does the same for the directories holding them in
which it changed no path, since Cargo's `rerun-if-changed` on a directory
reads the directory's own time.

Creation does so only when it cloned an output. Beside outputs the worktree
already had, older times could hide changed sources.

### Clones and subvolumes

A clone still costs time per file. So on btrfs each output is a subvolume,
whose snapshot takes the same time for any number of files. A freeze turns a
plain output directory into one, once a probe shows that Shoal can remove the
subvolumes it makes. Removal destroys them before it deletes the rest.

Other filesystems use reflinks or APFS clones. ZFS datasets and APFS snapshots
cover more than one directory, so neither serves. A platform without clones
skips the seed rather than copy it.

### Best effort

A seed is best effort. Its failures notify and never fail creation. Builds and
spare clones wait while the host is under pressure, so they never add load an
agent would feel.

A build also takes admission in the agent queue at the memory its earlier
builds used, as a `shoal run` command does. It reads its steps' process trees
every second for its peak. Protection stops it before any agent. A stopped
build is no failure, and builds again once readings recover.

## Build tool presets

A repository's `presets` name the build tools it uses. Each preset sets up
what its tool needs to stop repeating work across workspaces, so common stacks
need no script.

A tool whose build outputs survive a move into another path gets a seed. A
preset's seed runs the default branch's own build code, as
`setup.seed.command` would. A tool whose cache already serves every checkout
gets only what makes the cache's keys independent of the workspace path, or one
cache where each checkout would keep its own.

### Cargo's seed

Cargo gets a seed rather than a shared compiler cache such as sccache. A
shared cache needs another tool and gives up incremental builds.

The seed builds what each common command needs, since Cargo builds each with
its own settings. `cargo test` builds a proc-macro member with other settings
than `cargo build --all-targets` does. `cargo check` makes the dependency
builds that `cargo clippy` uses too. Cargo keeps clippy's builds of the
workspace crates apart from `cargo check`'s, so without its own step every
workspace would type-check them from scratch for clippy.

Clippy's lint arguments are rustc options, and rustc discards incremental
state built under other options. So the seed passes none, and serves clippy
runs that fail on warnings through Cargo's `build.warnings` instead. A step
without lint arguments also denies nothing, so a default branch with warnings
still seeds.

A failure of the clippy step alone still freezes the snapshot and notifies.
Its outputs serve only clippy runs, and clippy is an optional component that a
toolchain can lack. A timeout or cancellation there fails the build, as in any
step.

### Cargo's clean builds

Cargo never deletes the outputs of a unit whose hash changed. Releases,
lockfile and toolchain updates and crate moves change those hashes. So the
seed `target` would keep every earlier build, and each freeze, spare and clone
would copy them.

The seed therefore runs `cargo clean --workspace` first when something those
hashes read has changed since the snapshot's build, and only then. It keeps
third-party units. Most refreshes rebuild a few crates incrementally, while
one that changes unit hashes rebuilds most of the workspace anyway.

### Cargo's wrappers

A seed serves the workspace crates only if their incremental state survives
the move. rustc reuses that state only under the same compiler options, which
include the working directory. It reuses generated code only for the same
output directory. So the preset runs the workspace's own crates through a
wrapper that maps the working directory to `.` and passes the output directory
relative to the workspace root. Cargo's `trim-paths` does not help, since its
flags name each package's directory.

The mapping is an unstable flag. Stable rustc takes it only for the crates
`RUSTC_BOOTSTRAP` names. So the wrapper names each crate it builds there and
allows it no unstable features, while a nightly rustc gets only the flag, and
crates keep the features they enable.

Dependencies and Cargo's target queries stay as the toolchain builds them.
Cargo builds dependencies without the wrapper. A grant would change the cfgs
that target queries print. The wrapper drops the flag for a rustc that no
longer takes it.

`cargo clippy` replaces the workspace wrapper with `clippy-driver`. So the
preset also sets `RUSTC_WRAPPER`, which Cargo runs before `clippy-driver` for
the workspace crates and before rustc for everything else. A second script
there hands only the `clippy-driver` runs to the workspace wrapper.

That replaces a user's own `RUSTC_WRAPPER` or `build.rustc-wrapper`, such as
sccache, whose work the seed already covers. One set in `[env]` wins, as every
variable there does, and clippy then runs without the mapping.

### Cargo in CI runners

CI runners set `CI`, under which Cargo leaves incremental compilation off. It
then misses the seed's builds of the workspace crates. So the preset gives
Shoal [runner](runner.md) jobs `CARGO_INCREMENTAL=1`, the only setting that
overrides `CI`. Agent workspaces do not get it, since it would make their
release builds incremental too.

The setting reaches the steps through the runner's own environment. A
workflow's `env` replaces that on both forges, so a workflow can still turn
it off. Forgejo's `runner.envs` would replace that `env`, and GitHub's
`GITHUB_ENV` would override it.

### Caches independent of the path

Go trims paths. ccache takes the workspace as its base directory. Zig shares
its local cache.

Xcode's DerivedData records absolute paths, so no seed survives the move.
Xcode instead shares one compilation cache, with the workspace and DerivedData
paths mapped out of its keys. Xcode also needs one package checkout per set of
resolved pins, which only an `xcodebuild` argument can choose. So the preset
puts a wrapper first on the `PATH` of workspace processes.

These presets trade real source paths in debug info and stack traces for the
shared cache. That is why they are opt-in.

### Xcode's own directories

The shared cache does not shrink a workspace's own build directory, since
Xcode copies cached results into each DerivedData. That folder lies outside
the worktree, keyed by the project's path. So the preset makes removal delete
it, and disk space cleanup delete what earlier removals left. Xcode rebuilds
it on demand.

Package checkouts are shared by pins rather than owned by a worktree. So only
those nothing pins go, and only after a day unused, since a build that
resolves packages rewrites its pins while it runs.

Xcode trims the compilation cache itself.

## MCP servers

A preset also brings the MCP servers its tool's agents use, as `xcode` brings
Xcode's bridge. `[mcp.NAME]` adds, changes or turns off servers by name in
every layer, as `[env]` does for a preset's variables.

Shoal gives each workspace and chat agent its repository's servers in the
agent's own syntax, beside the servers in the user's own config. So a server
that only some repositories need belongs to their config, rather than to the
user's, which every session starts. Shoal never edits the agent's own config
to do it. An agent whose integration has no such syntax starts none.

## Shared caches

A tool that locks its cache entries or addresses them by content, such as
`GOCACHE` or sccache's directory, can share one cache between live workspaces.
`[caches.NAME]` gives a repository one directory per cache, so workspaces stop
repeating downloads and compilation. The scope is explicit, since a cache that
live workspaces share is safe only for such tools.

A cache is disposable, so `shoal cleanup caches` and disk space cleanup empty
it. They do so only while no tracked execution or seed build of the
repository runs. Registering an execution prepares the directories in the
transaction that records it. So emptying under the store's write lock cannot
race a start.

## The crate's boundary

The `Repositories` service registers repositories and looks them up. It also:

- prepares the `root_dir` that holds them, and moves their directories into a
  new root,
- resolves the config layers of repositories and their spaces,
- keeps each checkout's default branch in sync,
- builds the seeds and starts new workspaces from their snapshots,
- empties the shared caches,
- removes repositories and sweeps finished contributions.

The crate's interface reaches what stays with the daemon:

- the spaces a removal or move takes, and whether a workspace's worktree may go
  or move with it,
- where the chats without a repository keep their files,
- the subsystems that work and keep files in a repository's directories, and
  the agents that trust its paths,
- the hooks' executables and environments, which read ports and temporary
  directories,
- fetching and fast-forwarding a default branch, which the refresh of a
  workspace's branch shares,
- whether the host has headroom for a build.
