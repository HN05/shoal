# Setup and hooks

This file says what `shoal setup` does and how workspace setup, hooks, build
tool presets, seed builds, caches and temporary directories work.

A repository's [`setup.command`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#setup) prepares each new worktree,
and [`[hooks]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hooks), also global defaults, name untracked lifecycle
executables. Hooks that run while the worktree exists use it as their working
directory.

`hooks.pre_setup` runs in the daemon before tracked setup, after ownership and
execution checks, with a 60-second limit. It works without a `setup.command`.
Failure marks the workspace failed and skips setup;
retry with `shoal setup`. Lifecycle and permit changes are rejected while it runs.

Tracked setup starts by copying the repository checkout's ignored files, such as
`.env` or local signing settings, into the workspace before `setup.command`,
cloning them where the filesystem can. It runs without a `setup.command`, and a
failure fails setup. It prints how many files it copied, and nothing when it
copied none. [`[setup] copy_ignored = false`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#setup) turns it off.
Files the workspace already has are kept, including paths cloned from a
[seed](#seed-builds), and nothing is written through a symbolic link to outside
the workspace. A [copy](create.md#copies) of a workspace's current files skips it, since
it has that workspace's ignored files. Without a `.worktreeinclude` in the
checkout every ignored file is copied, build caches included; its gitignore-style
patterns limit the copy to what they match, and `[setup] copy_ignored_exclude`
leaves out what its patterns match. A directory holding another worktree, and anything inside one, is never
copied. The state directories
of other version control systems and worktree tools (`.bzr`, `.conductor`,
`.entire`, `.hg`, `.jj`, `.pijul`, `.sl`, `.svn` and `.worktrees`) are left out
where Git lists them as ignored, and inside ignored directories the patterns
look into; an ignored directory Git lists whole is otherwise copied whole.

`setup.command` runs through the tracked execution wrapper with workspace scope and
your CLI environment; for a workspace another machine of the hub runs, `shoal setup`
and `shoal refresh` run setup and the post-setup hook there, in that machine
daemon's environment. Setup and the post-setup hook, like the push of `shoal pr open`
and `shoal merge`, run in a process the daemon starts: the command shows their output,
and interrupting the command interrupts them, while they go on when the command ends
otherwise, such as when its terminal closes. `shoal start` from a terminal, or
with `--wait`, waits for setup before entering the worktree or launching an
agent; the workspace stays `preparing` until it exits 0 with no
surviving processes. When that command is interrupted before setup starts,
`shoal setup` prepares the workspace and `shoal rm` removes it. On failure, interactive mode asks whether to retry setup, delete the
new workspace and branch, ignore the failure and continue, or keep it for
inspection (default); a setup that never started, which the daemon refused,
offers only retry or keep, and deleting a workspace another machine runs names
that machine. JSON/noninteractive mode returns nonzero, keeps the
workspace, and sends setup output to stderr. The agent `shoal start` or an app
launches there waits in the [agent queue](queue.md) for the setup,
holding no slot, even when the process that prepared the workspace ended
before it could queue the agent; `shoal queue cancel` removes it. Once a retried or ignored setup gets past the post-setup
hook, from the prompt or `shoal setup`, the queue starts it with its mode,
task and permissions, linking the PR a start from a PR names, and a failed
hook keeps it waiting. Then choose explicitly; `--skip`
leaves what setup did, which a scoped process does as its space's
[`admin` permission](modes.md#permitted-actions) allows:

```sh
shoal setup 3                         # Rerun setup and the post-setup hook
shoal setup 3 --skip                  # Accept the failure after ownership checks
shoal rm 3 --yes                            # Delete this workspace
```

`--skip-setup` on `shoal start` and `shoal create` creates a workspace without
setup, for one that only needs a checkout, such as one a CI job or a reviewer
reads: no `hooks.pre_setup`, ignored-file copy, `setup.command` or
`hooks.post_setup`, and since they tear down what setup made, no
`hooks.pre_remove` or `hooks.post_remove` either. The [seed](#seed-builds) clone
still happens. `status` shows `Setup: skipped` and `--json` `setup_skipped`;
`shoal setup` runs the full setup later and clears it. A removed workspace
without setup is restored without it too.

Hooks are untracked: they run as your own processes with `SHOAL_HOOK` (the key
without `_cmd`), `SHOAL_SPACE` (the space's ID), `SHOAL_STATE_DIR` and, for a space with a
worktree, `SHOAL_WORKSPACE` (its `{workspace}`) and `SHOAL_WORKSPACE_PATH`, without a scope token, port variables or `SHOAL_OUTPUT`, so whatever they leave running (a tmux
server, say) is not a Shoal execution. `hooks.post_setup` runs without a terminal
after `start` or `setup` has a ready workspace and before any `--agent`; a
nonzero exit keeps the workspace, skips the agent, and fails the command.
`hooks.pre_remove` runs inside the daemon for `rm`, `repo rm`, and automatic cleanup,
after the removal checks pass and managed commands stop, without a terminal and with
a 60-second limit; a nonzero exit or timeout retains the workspace with the hook's
stderr as its error. It is skipped when the worktree directory is already gone.

`hooks.post_remove` runs after successful removal from the shared daemon path,
with a 60-second limit and no terminal. Its path and working directory are relative
to the repository checkout, whose copy of the script must exist; `SHOAL_WORKSPACE_PATH`
still names the removed worktree. The command is selected before removal.
Failure cannot undo deletion: removal still succeeds, with a CLI warning,
JSON `hook_error`, and a `hook_failed`
alert. It is skipped for already-missing worktrees and is not replayed
following a daemon restart. Use a pre-remove hook when failure must retain ownership.

## Build tool presets

[`setup.presets`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#setup) names the build tools a repository uses, and
Shoal sets each one up so its workspaces stop repeating the same work:

| Preset | Sets up |
|---|---|
| `cargo` | Two rustc wrappers that keep the workspace crates' incremental state reusable, a [seed](#seed-builds) running `cargo build --all-targets`, `cargo test --no-run`, `cargo check --all-targets` and `cargo clippy --all-targets`, keeping `target`, and `CARGO_INCREMENTAL=1` for [runner](ci.md#runner) jobs |
| `npm` | A seed running `npm ci`, keeping `node_modules` |
| `pnpm` | A seed running `pnpm install --frozen-lockfile`, keeping `node_modules` |
| `yarn` | A seed running `yarn install --frozen-lockfile`, keeping `node_modules` |
| `bun` | A seed running `bun install --frozen-lockfile`, keeping `node_modules` |
| `go` | `GOFLAGS=-trimpath`, and a seed running `go build ./...` |
| `xcode` | One compilation cache for every workspace's `xcodebuild`, with shared Swift package checkouts, and Xcode's MCP server, `xcrun mcpbridge`, for agents |
| `zig` | The shared cache `preset.zig` as `ZIG_LOCAL_CACHE_DIR`, and a seed running `zig build` |
| `ccache` | ccache as the compiler launcher of CMake builds, with the workspace as its base directory |

Go's build cache is shared by every
checkout, but it keys a package by its directory unless paths are trimmed; with
`-trimpath`, binaries, panics and `runtime.Caller` report module paths instead
of file system paths. The seed fills that cache from the default branch.

The `cargo` preset keeps a [shared cache](#shared-caches) named `preset.cargo`
holding two rustc wrappers. It sets `RUSTC_WORKSPACE_WRAPPER` to the first, so
Cargo runs the workspace's own crates through it. rustc ignores incremental state built in
another working directory, and the seed and every workspace build in
directories of their own. For each crate it builds, the wrapper adds
`-Zremap-cwd-prefix=.`, which leaves rustc's options the same in every
directory. A rustc that refuses the flag, such as a stable one, gets the crate
added to `RUSTC_BOOTSTRAP`, which lets it take the flag for that crate alone,
with `-Zallow-features=` so the crate still uses no unstable feature.
Dependencies, which Cargo builds without the wrapper, and Cargo's target
queries, which it passes through unchanged, build as the toolchain builds them. rustc also regenerates all of a crate's code when its
output directory moves, so the wrapper passes `--out-dir` relative to the
workspace root when it lies inside it. Debug info of the workspace's crates
records `.` as their working directory. The wrapper leaves rustc's arguments unchanged under
`RUSTC_BOOTSTRAP=-1` or when rustc no longer takes the flag. A
`RUSTC_WORKSPACE_WRAPPER` in `[env]` runs without it.

`cargo clippy` replaces `RUSTC_WORKSPACE_WRAPPER` with `clippy-driver`, so the
preset also sets `RUSTC_WRAPPER` to the second wrapper, which applies the first
one's mapping to clippy's builds of the workspace's crates and passes every
other rustc run through unchanged. The preset's `RUSTC_WRAPPER` replaces a
wrapper set in the inherited environment or as Cargo's `build.rustc-wrapper`,
such as sccache; one set in `[env]` replaces the preset's, and clippy then
builds without the mapping. When the clippy step fails, such as without
clippy installed or on a lint that denies by default, the seed keeps the
outputs of the other steps and notifies; new workspaces start without
clippy's, and the seed is rebuilt only once the default branch or its
settings change.
The seed's clippy results serve only clippy runs with the same lint arguments,
and the seed passes none after `--`. To fail on warnings, use Cargo's
`build.warnings` instead, as in `CARGO_BUILD_WARNINGS=deny cargo clippy
--all-targets`.

Cargo keeps the outputs of every earlier build of a crate whose build inputs
changed, so the seed first runs `cargo clean --workspace`, which deletes the
workspace crates' outputs and keeps third-party ones, when its toolchain
(`rustc -vV` in the seed worktree) or settings changed since the build the
snapshot holds, the snapshot is missing or its commit is gone, or the new
commit changes `Cargo.lock`, a `Cargo.toml`, `rust-toolchain`,
`rust-toolchain.toml` or `.cargo/config.toml`. Other refreshes build on the
outputs they have.

Cargo keeps separate builds of the workspace's crates for each
wrapper, so a Cargo command run without the preset's environment, such as from
a shell `shoal internal env` did not set up, builds them from scratch.

The runner sets `CI` for every job, and Cargo leaves incremental compilation
off under `CI`, which would rebuild every workspace crate the seed built
incrementally. The preset therefore gives each job `CARGO_INCREMENTAL=1`,
which turns incremental compilation on in every profile, `--release`
included. `CARGO_INCREMENTAL` in the workflow's `env` or in `[env]` replaces
it, so `"0"` there turns it off.

The `ccache` preset needs ccache installed and CMake 3.17 or later, and applies
when a build directory is first configured. With the workspace as ccache's base
directory and its working directory left out of the hash, a compile another
workspace ran hits, and debug info records that workspace's directory.

The `xcode` preset keeps an xcconfig and an `xcodebuild` wrapper in a [shared
cache](#shared-caches) named `preset.xcode-files`, and Xcode's compilation cache
and Swift package checkouts in one named `preset.xcode`. It sets
`XCODE_XCCONFIG_FILE` to the xcconfig, and puts the wrapper first on
the PATH of every process Shoal starts in a workspace and of `shoal internal env`. The
xcconfig turns compilation caching on and maps the workspace and its DerivedData
folder out of cache keys, so a workspace reuses what another compiled. Packages
checked out per workspace would still miss, so the wrapper passes
`-clonedSourcePackagesDirPath` keyed by the content of the `Package.resolved`
of the project it builds, unless the command passes its own. Mapping replaces
the workspace path in `#filePath` and debug info with `/^workspace`. Builds from
the Xcode app, root-level Swift packages (whose package name is the folder
name) and a configured `XCODE_XCCONFIG_FILE` do not use it. Each workspace
still keeps its own DerivedData, since Xcode copies cached results into it.
Removing a workspace deletes the DerivedData folder of every project inside it,
found by the `WorkspacePath` its `info.plist` records, in Xcode's custom
DerivedData location when that is absolute and in
`~/Library/Developer/Xcode/DerivedData` otherwise. It also deletes the package
checkouts that no build used for a day and no `Package.resolved` pins in the
registered checkout, an existing worktree or the default branch's tip.

A preset's MCP server reaches every Claude Code and Codex agent of the
repository's workspaces and chats, beside the servers the agent's own config
names; [`[mcp.NAME]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#mcp-servers)
changes or turns it off, and defines other servers. Claude Code receives them
as one `--mcp-config`, Codex as one `-c mcp_servers.NAME` table each, which
replaces a server of the same name in its own config whole.

## Seed builds

A repository can keep a build of its default branch that new workspaces start
from, so a branch rebuilds only what it changed and shares the rest on disk.
[`[setup.seed]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#seed-builds) and the [presets](#build-tool-presets)
name its build commands and the outputs it keeps.

The daemon keeps a locked, detached worktree at the local default branch's tip in
the repository directory's `.seed`, and runs the build there when that branch or
the seed's settings change. It checks every minute, one repository at a time, and
waits while memory or free disk space is past its `[machine.overload.warning]` threshold;
a build that failed is retried only after such a change or a daemon restart. The
settings come from the repository's local config and its registered checkout's
file. Its command runs like `setup.command`; preset commands run in the worktree's root. The
build gets the repository's `[env]`, with placeholders
rendered for the seed worktree, but no inherited `SHOAL_*` variables or agent secrets. It has a
two-hour limit and writes each command's output to `.seed/build.log` after a
`$` line naming it; anything a command leaves
running in its process group is stopped when it exits.

A successful build freezes the outputs as a copy-on-write snapshot: APFS clones on
macOS, reflinks on Linux, never a full copy. On btrfs each output becomes a
subvolume in the seed worktree, after a check that Shoal can remove the
subvolumes it makes there, so freezes, spares and new worktrees take a
snapshot of it, which takes the same time for any number of files; where that
check or a snapshot fails, the output is cloned with reflinks. A new worktree whose own settings
match the snapshot's clones its paths before setup, keeping any path that already
exists. The daemon keeps one spare clone of them in `.seed/snapshot`, which a new
worktree without any of the paths takes instead of cloning, and clones the next
one in the background while memory and disk space are clear of their warning
thresholds; a [copy](create.md#copies) of a workspace's current files has that workspace's
outputs instead. When it clones at least one, it gives every tracked file its commit left
unchanged, and every directory holding tracked files in which it changed no path,
the seed's modification time, so tools that compare modification times, such as
Cargo and make, rebuild only changed files; next to outputs the worktree
already had, older times could hide changed sources. Cloned files share disk blocks with the snapshot
until rewritten. An output that records an absolute path, such as a path a
test binary was compiled with, names the seed worktree until it is rebuilt, and
the seed worktree moves on with the default branch; read such paths at run time.
A failed build or clone records a `seed_failed` alert, and the workspace
starts without the seed.

For a Cargo repository with the `cargo` preset, a new workspace's first build
reuses the seed's dependency builds as they are and the incremental state of
the workspace's own crates. It recompiles only the code its branch changed,
plus whatever depends on that code. Without the preset's
[wrapper](#build-tool-presets), rustc ignores the seed's incremental state, and
the workspace's own crates build from scratch.

Removing the repository or its `[setup.seed]` stops a running build and deletes the
seed worktree, snapshot and log, keeping anything else in `.seed`. Shoal marks
`.seed` and the seed worktree's Git metadata with the repository, and neither
builds in nor deletes a seed whose marks are missing. Point outputs a tool keeps
outside the worktree at a path inside it, such as
`xcodebuild -derivedDataPath build/DerivedData`, so workspace removal deletes them
too.

## Shared caches

Tool caches that lock or address their entries by content can serve every
workspace of a repository at once. Each [`[caches.NAME]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#shared-caches) is a directory in the repository directory's
`.caches/shared`, which `{cache.NAME}` names in `[env]`, so every process Shoal
starts in a workspace, and the seed build, uses it. The daemon creates the directory when it starts a process or exports the
environment, and marks `.caches` with the repository, refusing to start one while
an unmarked `.caches` is in the way. Build output directories such as Cargo's `target/` do not belong here:
concurrent builds serialize or corrupt them, so use a [seed](#seed-builds).

`shoal cleanup caches` and [disk space](load.md#disk-cleanup) cleanup empty a
repository's caches, configured or not, apart from `preset.cargo` and
`preset.xcode-files`, which hold only the [presets'](#build-tool-presets) own
files, while none of its tracked executions and
no seed build of it is running; disk space cleanup records a `caches_emptied`
alert. Processes started with an exported environment are not tracked.
Removing the repository deletes `.caches`.

## Temporary directories

Every workspace and chat has its own directory for temporary files, `.tmp/<id>`
in the repository directory, named by the workspace's or chat's ID. Every process
Shoal starts in a workspace, its hooks other than `hooks.post_remove`, `shoal internal
env` and a chat's agent get it as `TMPDIR`, so their scratch files stay out of the
shared `/tmp`; a `TMPDIR` in `[env]` replaces it. The daemon creates the
directory, readable only by you, when it starts a process in the workspace,
exports its environment or records the chat, reserving the repository directory
for a registration without one. It marks `.tmp` with the repository and refuses
to start the process or chat while an unmarked `.tmp` is in the way. A chat
without a repository has its directory in the `.tmp` of the `root_dir` it started
in, where it stays when `root_dir` changes until [`repo move`](repo.md#move-repositories-to-a-new-root) takes it along, and the daemon marks that `.tmp` with
its state directory: a daemon with another state directory that
shares `root_dir` neither uses nor deletes it, and starts no chat without a
repository there.

Nothing in it is kept: removing the workspace or chat deletes the
directory without a checkpoint, and cleanup deletes those that belong to no
workspace or chat of the repository, or to no chat without a repository.
Removing the repository deletes `.tmp`.
