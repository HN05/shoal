# Repositories

This file covers `shoal-repositories`: registered repositories and their
directories, contributions, seed builds, presets and shared caches.

Every repository owns `~/shoal/<name>/` (`machine.root_dir`): its workspaces
default to it and a URL clone lives there as `.checkout`, a name no
workspace can take, so worktrees are grouped per repository, never nested in a
checkout, and outside the state directory; a root inside either, or inside a
recorded repository directory, is refused.
On macOS a root Shoal creates links to `<parent>/.<name>.noindex`, which holds
its contents: Spotlight ignores `.metadata_never_index` below a volume root and
its privacy list needs root, so only a skipped name keeps build output out of
the index, and the link keeps `root_dir` the same on every system. An existing
root is used as found; only `repo move` changes it, by default to the link,
or back to a regular directory with `--regular-dir`.
Before registration, workspace creation or an export writes a directory, the
daemon creates a file there itself under a time limit: its file access can
differ from the terminal's, and on macOS opening a file on a volume it lacks
access to can wait for a permission prompt no one sees.
File access in a worktree can wait without end: while macOS waits for an
answer to a privacy prompt, whichever process it is for, every new process that
opens a file in the protected location waits until the prompt is answered or the
kernel interrupts it (`Interrupted system call`), so each Git the daemon starts
does. Shoal cannot end the wait, only keep it from holding up unrelated work,
and does not depend on where the worktree is or which permission it waits on.
The daemon's own reads in a worktree, such as its ownership marker or whether
it exists, can wait too, and no time limit interrupts a read waiting in the
kernel, so requests and sweeps read worktree files on the blocking pool, where
a read holds no runtime thread and a check's time limit ends the wait for it.
The startup audit, its recheck, the sweep's branch observation and cleanup
checks, the Git reads of `shoal status`, and the reads of a checkout's or
worktree's config file, which every command that resolves its settings, such
as `shoal attach`, waits on, check a worktree within 30 seconds, a status
reporting what it could not read; one that runs out or is interrupted
records the directory holding the worktree as stalled, which the daemon reports
once as an alert and in Shoal's `status` until a check there finishes. A check
can also run that long under heavy memory pressure with no prompt waiting, so
the report says the check timed out and, only on macOS, names a prompt as a
possible cause. The directory comes from the path alone, since reading the filesystem could stall
too, and groups a repository's workspaces, which a prompt for their location
holds together.
Those checks skip a stalled directory until 20 seconds after its last check, and
while another check rechecks it, so each sweep rechecks it once; the sweep's
cleanup phases skip a workspace or checkout in a stalled directory, reading its
HEAD themselves when a recheck is due, since none may be left there that branch
observation can read; an unattended removal, deleted-worktree cleanup included,
is refused while a workspace below it is in a stalled directory, and PR cleanup
skips a base while one stacked on it is. Listings read
HEAD for review marks within three seconds in all and skip stalled directories,
leaving staleness unknown, so `shoal attach` and `shoal list` answer from the
daemon's state. Repository pickers start Git only to tell apart repositories
that share a name, reading their origins at once within three seconds, and
label one whose origin they cannot read by its recorded source.
Repository roots and explicit workspace paths resolve existing symlinks and lexical
`..` before boundary checks and creation, even when trailing components are missing.
An explicit workspace path overrides the destination for one creation, without
changing the repository directory; reject existing paths and overlaps with state,
checkouts, workspaces, or other repositories' directories. Cleanup owns the worktree,
never its parent. Directories are reserved atomically and never reused; an
in-place checkout placed as `~/shoal/<name>/<x>` adopts that directory, and
`--path` clones elsewhere. A repository directory moves only whole, into a new
`root_dir`, while nothing runs or uses it: its workspaces are reserved and the
move is recorded before anything moves, then the records switch and Git's
worktree links are repaired, so a stopped daemon finishes a completed move and
rolls back an incomplete one, deleting only directories with their recorded identity. Checkouts and workspaces outside it stay.
`repo move` stops the commands and agents running there as `shoal stop`
does and queues the agents' restores after the move, whose trust entries the
daemon carries to the new paths: a stop keeps sessions, which resume by ID from
any directory. The
chats without a repository, which run in `root_dir`, go with the root: their
agents stop the same way, their temporary directories, attached files and kept session files move
into the directory that holds the repositories, and their records name the
configured root, which is that directory or links to it, so a root that
becomes a link is empty once they have moved. Registration is idempotent by normalized origin URL,
then canonical path, never fetches, and keeps a stable UUID separate from the
display name. Repository selectors preserve the caller's argument alongside its
canonical path. IDs and sources win before path lookup; a registered name survives
unrelated path collisions, while a name and path matching different registrations
require disambiguation.
`repo rm` binds confirmation to each machine's resolved registration, by
the ID that machine gives it, and shows its source, checkout path and the
spaces it deletes there with the work they lose. After verifying the checkout's identity and
boundaries, it deletes the checkout recursively, then removes the repository
directory only when that directory is empty.
Each workspace and chat gets its own temporary directory as `TMPDIR`, in the
repository directory's `.tmp` under its ID, or `root_dir`'s for a chat without a
repository, which the daemon marks with its state directory so another daemon
sharing `root_dir` leaves it alone. The ID never changes or comes back
like a name. Outside the worktree it neither dirties it nor enters checkpoints,
and its path stays short enough for the Unix sockets tools create in it on
macOS. It is created only once its record exists and deleted with it, by
the removal of its space; the cleanup sweep
deletes any without a record, with the directories in it the owner may not
write, since nothing there is worth keeping. The
post-remove hook runs after it is gone and keeps the inherited `TMPDIR`.
A contribution is a URL clone registered for work in a repository the user does
not maintain. `origin` stays the repository itself, so issues, PRs and syncs use
it; when the user's login cannot push there, the user's fork, created or reused,
becomes the `fork` remote and `remote.pushDefault`, which Git, `pr open` and
cleanup all honour. It goes through `repo rm`'s path once nothing of it has been
left for the unattended grace period: no running `contribute`, whose daemon
connection holds it in memory from its registration until the command ends,
resuming on a new one after a restart, while every contribution counts as held
for the first minute the daemon runs, no
workspace, queued agent, chat, pending
issue proposal, kept branch or borrowed resource, nothing `shoal resume` could
use, no files a removed space keeps, and no work in its checkout that the remotes' advertised branches and tags
lack. The final
check records the removal in the same transaction, under the gates creation
takes, and queue and chat records refuse a repository being removed. Removal
never deletes the fork: Shoal does not record whether it created it, deleting a
forge repository cannot be undone, and the fork may hold the only copy of a
PR's branch. An existing registration is never turned into one.

A repository's `[setup.seed]` keeps one build of its local default branch that new
worktrees start from, instead of sharing build directories between live
workspaces: a shared directory serialises or corrupts concurrent builds, while a
copy-on-write clone costs no space until a branch rewrites a file and needs no
tool support. The daemon owns a locked, detached seed worktree in the repository
directory, builds it when the branch moves and freezes the configured outputs as
a snapshot, so a running build never reaches a workspace. A clone of a large
build tree takes minutes, so creation clones after its Git operations, outside
the repository Git gate, and the workspace is neither ready nor set up until the
clone ends. The daemon also keeps one spare clone of the snapshot ready:
creation moves it into the new worktree, a background task clones the next, and
creation clones only when no spare is ready. Clones read the snapshot under a
shared lock of the seed's own, which a swap or deletion of the snapshot takes
exclusively, so starts and the refill clone at the same time and only taking the
spare is exclusive. A new snapshot is cloned and the old one deleted outside the
Git gate, which it takes only to swap them. Creation restores the
seed's modification times on tracked files its commit left unchanged, and on
the directories holding them where it changed no path inside, since
mtime-based tools otherwise treat every freshly checked-out file as new, and
Cargo's `rerun-if-changed` on a directory reads the directory's own time.
Clones of a file still cost time per file, so on btrfs each output is a
subvolume, whose snapshot takes the same time for any number of files: a freeze
turns a plain output directory into one, once a probe shows Shoal can remove the
subvolumes it makes, and removal destroys them before deleting the rest. Other
filesystems use reflinks or APFS clones; ZFS datasets and APFS snapshots cover
more than one directory, so neither serves. Platforms without clones skip the
seed rather than copy. Seeds are best effort:
failures notify and never fail creation, and builds and spare clones wait under
host pressure so they never add load an agent would feel.
A repository's `presets` name the build tools it uses, and each sets up what
that tool needs to stop repeating work across workspaces, so common stacks need
no script. Tools whose build outputs survive a move into another path get a
seed: Cargo, whose seed covers it better than a shared compiler cache such as
sccache, which needs another tool and gives up incremental builds, and the Node
package managers. Cargo's seed also runs `cargo test`, which builds a proc-macro
member with other settings than `cargo build --all-targets` does, `cargo
check`, which makes the dependency builds `cargo clippy` uses too, and `cargo
clippy`: Cargo keeps clippy's builds of the workspace crates apart from
`cargo check`'s, so without its own step every workspace type-checks them from
scratch for clippy. Clippy's lint arguments are rustc options, and rustc
discards incremental state built under other options, so the seed passes none
and serves clippy runs that fail on warnings through Cargo's `build.warnings`
instead; a step without them also denies nothing, so a default branch with
warnings still seeds. A failure of the clippy step alone still freezes the
snapshot and notifies, since its outputs serve only clippy runs and clippy is an
optional component a toolchain can lack; a timeout or cancellation there fails
the build as in any step.
Cargo never deletes the outputs of a unit whose hash changed, which
releases, lockfile and toolchain updates and crate moves do, so the seed
`target` would keep every earlier build and each freeze, spare and clone would
copy them. The Cargo seed therefore runs `cargo clean --workspace` first, which
keeps third-party units, when its toolchain, settings or a file those hashes
read has changed since the snapshot's build, and only then: most refreshes
rebuild a few crates incrementally, while one that changes unit hashes rebuilds
most of the workspace anyway.
A seed serves the workspace crates only if their incremental state survives
the move, but rustc reuses it only under the same compiler options, which
include the working directory, and reuses generated code only for the same
output directory. So the Cargo preset runs the workspace's own crates through
a wrapper that adds `-Zremap-cwd-prefix=.` and passes the output directory
relative to the workspace root. Cargo's `trim-paths` does not help: its flags
name each package's directory. Stable rustc takes the unstable flag only for
the crates `RUSTC_BOOTSTRAP` names, so for a rustc that refuses it the wrapper
names each crate it builds and allows it no unstable features; a nightly rustc
gets only the flag, so crates keep the features they enable. Dependencies,
which Cargo builds without the wrapper, and Cargo's target queries, whose
printed cfgs a grant would change, stay as the toolchain builds them, and the
wrapper drops the flag for a rustc that no longer takes it. `cargo clippy`
replaces the workspace wrapper with `clippy-driver`, so the preset also sets
`RUSTC_WRAPPER`, which Cargo runs before `clippy-driver` for the workspace
crates and before rustc for everything else, to a second script that hands
only `clippy-driver` runs to the workspace wrapper. That replaces a user's own
`RUSTC_WRAPPER` or `build.rustc-wrapper`, such as sccache, whose work the seed
already covers; one set in `[env]` wins, as every
variable there does, and clippy then runs without the mapping. CI runners set `CI`, under which
Cargo leaves incremental compilation off and so misses the seed's builds of
the workspace crates, so the preset gives Shoal runner jobs
`CARGO_INCREMENTAL=1`, the only setting that overrides `CI`, and not agent
workspaces, where it would make release builds incremental too. It reaches
the steps through the runner's own environment, which a workflow's `env`
replaces on both forges, while Forgejo's `runner.envs` would replace that
`env` and GitHub's `GITHUB_ENV` would override it. Preset seeds run the default branch's own build code, as
`setup.seed.command` would. Tools whose caches already serve every checkout get only what
makes their cache keys independent of the workspace path, or one cache where
each checkout keeps its own: Go trims paths, ccache takes the workspace as its
base directory, Zig shares its local cache, and Xcode, whose DerivedData records absolute paths so no seed survives the move,
shares one compilation cache with the workspace and DerivedData paths mapped
out of its keys. Xcode also needs one package checkout per set of resolved
pins, which only an `xcodebuild` argument can choose, so the preset puts a
wrapper first on workspace processes' PATH. These presets trade real source
paths in debug info and stack traces for the shared cache, which is why they
are opt-in. Neither shrinks a workspace's own build directory: Xcode copies
cached results into each DerivedData. That folder lies outside the worktree,
keyed by the project's path, so the preset makes removal delete it and disk
space cleanup delete what earlier removals left; Xcode rebuilds it on demand.
Package checkouts are shared by pins rather than owned by a worktree, so only
those nothing pins go, and only after a day unused, since a build that resolves
packages rewrites its pins while it runs. Xcode trims the compilation cache
itself, by default to half the space available to it.

A preset also brings the MCP servers its tool's agents use, as `xcode` brings
Xcode's bridge, and `[mcp.NAME]` adds, changes or turns off servers by name in
every layer, as `[env]` does a preset's variables. Shoal gives each workspace
and chat agent its repository's servers in the agent's own syntax, beside the
servers the user's config names, so a server only some repositories need
belongs to their config rather than to the user's, which every session starts.
Shoal never edits the agent's own config to do it, and an agent whose
integration has no such syntax starts none.

Tool caches whose tools lock or address entries by content, such as `GOCACHE` or
sccache's directory, are the exception: `[caches.NAME]` with `scope = "shared"`
gives a repository one directory per cache, and `[env]` maps it to the tool's
variable with `{cache.NAME}`, so workspaces stop repeating downloads and
compilation. The scope is explicit because a cache shared by live workspaces is
only safe for such tools. A cache is disposable, so `cleanup caches` and disk
space cleanup empty it, but only while no tracked execution or seed build of the
repository runs; registering an execution prepares the directories in the
transaction that records it, so emptying under the store's write lock cannot
race a start.
