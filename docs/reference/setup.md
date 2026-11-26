# Setup and hooks

This file says what `shoal setup` does and how workspace setup, hooks, build
tool presets, seed builds, shared caches and temporary directories work.
[Setup and hooks](../design/setup.md) and
[repositories](../design/repositories.md) give the reasons.

## Workspace setup

Setup prepares a new worktree. It runs these steps in order:

1. [`hooks.pre_setup`](#setup-hooks), when configured.
2. The [copy of ignored files](#copy-ignored-files).
3. [`setup.command`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#setup),
   when configured.
4. [`hooks.post_setup`](#setup-hooks), when configured.

A new worktree clones its [seed](#seed-builds) before setup starts. The
workspace stays `preparing` until setup exits 0 and leaves no process running.

```sh
shoal setup 3          # Rerun setup and the post-setup hook
shoal setup 3 --skip   # Accept a failed setup after ownership checks
shoal rm 3 --yes       # Delete the workspace
```

### Run setup

`setup.command` runs through the tracked execution wrapper, with workspace
scope and your CLI environment.

Setup and the post-setup hook run in a process the daemon starts, as the push
of `shoal pr open` and `shoal merge` does. The command that started them shows
their output. Interrupting that command interrupts them. When the command ends
any other way, such as when its terminal closes, they go on.

`shoal start` from a terminal, or with `--wait`, waits for setup before it
enters the worktree or launches an agent. When that command is interrupted
before setup starts, `shoal setup` prepares the workspace and `shoal rm`
removes it.

For a workspace that another machine of the hub runs, `shoal setup` and
`shoal refresh` run setup and the post-setup hook on that machine, in its
daemon's environment.

### Copy ignored files

Setup first copies the repository checkout's ignored files into the
workspace, such as `.env` or local signing settings. It clones them where the
filesystem can. The copy runs without a `setup.command` too, and its failure
fails setup. It prints how many files it copied, and nothing when it copied
none.
[`[setup] copy_ignored = false`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#setup)
turns it off.

The copy keeps the files the workspace already has, including the paths it
cloned from a seed. It writes nothing through a symbolic link that leads
outside the workspace. A [copy](create.md#copies) of a workspace's current
files skips it, since it has that workspace's ignored files.

What it copies:

- Without a `.worktreeinclude` in the checkout, it copies every ignored file,
  build caches included. With one, it copies only what the file's
  gitignore-style patterns match.
- It leaves out what the patterns of `[setup] copy_ignored_exclude` match.
- It never copies a directory that holds another worktree, or anything inside
  one.
- It leaves out the state directories of other version control systems and
  worktree tools, such as `.jj`, `.svn` and `.worktrees`, wherever Git lists
  them as ignored. Inside an ignored directory, it leaves them out only where
  the patterns look into that directory. An ignored directory that Git lists
  whole is copied whole.

### Failed setup

A failed setup keeps the workspace. On a terminal, `shoal start` asks what to
do:

- retry setup;
- delete the new workspace and its branch;
- ignore the failure and continue;
- keep the workspace for inspection, the default.

A setup that never started, because the daemon refused it, offers only retry
and keep. When another machine runs the workspace, the delete choice names
that machine. In JSON or noninteractive mode, the command exits nonzero, keeps
the workspace and writes setup's output to stderr.

The agent that `shoal start` or an app launches in the workspace waits in the
[agent queue](queue.md#launch-a-new-workspace) for its setup. It waits even
when the process that prepared the workspace ended before it could queue the
agent.
`shoal queue cancel` removes it. Once a retried or ignored setup gets past the
post-setup hook, from the prompt or from `shoal setup`, the queue starts the
agent with its mode, task and permissions. It links the PR that a start from a
PR names. A failed post-setup hook keeps the agent waiting.

Otherwise, choose with `shoal setup` or `shoal rm`. `shoal setup --skip`
accepts the failure and leaves what setup did. A scoped process skips as its
space's [`admin` permission](modes.md#permitted-actions) allows.

### Skip setup

`--skip-setup` on `shoal start` and `shoal create` creates a workspace without
setup. Use it for a workspace that needs only a checkout, such as one a CI job
or a reviewer reads. It runs none of setup's steps. It also skips
`hooks.pre_remove` and `hooks.post_remove`, since they tear down what setup
made. The workspace still clones the [seed](#seed-builds).

`shoal status` shows such a workspace with `Setup: skipped`, and `--json` with
`setup_skipped`. `shoal setup` runs the full setup later and clears the mark.
A removed workspace that skipped setup skips it again when it is restored.

## Hooks

[`[hooks]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hooks)
names executables that run at points of a space's life. A hook that runs while
the worktree exists uses it as its working directory. This section covers the
setup and removal hooks.
[Completion](completion.md) and [resources](resources.md#resource-hooks) cover
the others.

### Hook environment

Hooks run untracked, as your own processes, so whatever they leave running,
such as a tmux server, is no Shoal [execution](../design/execution.md). Each
hook gets:

- `SHOAL_HOOK`: the hook's name, such as `post_setup`;
- `SHOAL_SPACE`: the space's ID;
- `SHOAL_STATE_DIR`;
- for a space with a worktree, `SHOAL_WORKSPACE`, its `{workspace}`, and
  `SHOAL_WORKSPACE_PATH`.

Hooks get no scope token, port variables or `SHOAL_OUTPUT`.

### Setup hooks

`hooks.pre_setup` runs in the daemon before `setup.command`, after the
ownership and execution checks. It has a 60-second limit. It runs without a
`setup.command` too. Lifecycle and permit changes are refused while it runs.
Its failure marks the workspace failed and skips the rest of setup;
`shoal setup` retries.

`hooks.post_setup` runs without a terminal, once `shoal start` or
`shoal setup` has a ready workspace and before any agent starts. A nonzero exit
keeps the workspace and fails the command, and the agent keeps waiting.

### Removal hooks

`hooks.pre_remove` runs in the daemon for `shoal rm`, `shoal repo rm` and
automatic cleanup. It runs after the removal checks pass and the managed
commands stop, without a terminal and with a 60-second limit. A nonzero exit or
a timeout keeps the workspace, with the hook's stderr as its error. The hook is
skipped when the worktree directory is already gone.

`hooks.post_remove` runs once removal succeeds, without a terminal and with a
60-second limit. Its working directory is the repository checkout, which must
hold its copy of the script. `SHOAL_WORKSPACE_PATH` still names the removed
worktree. Shoal selects the command before removal. Its failure cannot undo
the deletion: removal still succeeds, with a CLI warning, a `hook_error` in the
JSON and a `hook_failed` alert. It is skipped for a worktree that is already
missing, and a daemon restart does not replay it. Use `hooks.pre_remove` when
a failure must keep the workspace.

## Build tool presets

[`setup.presets`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#setup)
names the build tools a repository uses. Shoal sets each one up so that its
workspaces stop repeating the same work:

| Preset | Sets up |
|---|---|
| `cargo` | Two rustc wrappers, a [seed](#seed-builds) that keeps `target`, and `CARGO_INCREMENTAL=1` for [runner](ci.md#runner) jobs |
| `npm` | A seed running `npm ci`, keeping `node_modules` |
| `pnpm` | A seed running `pnpm install --frozen-lockfile`, keeping `node_modules` |
| `yarn` | A seed running `yarn install --frozen-lockfile`, keeping `node_modules` |
| `bun` | A seed running `bun install --frozen-lockfile`, keeping `node_modules` |
| `go` | `GOFLAGS=-trimpath`, and a seed running `go build ./...` |
| `xcode` | One compilation cache for every workspace's `xcodebuild`, shared Swift package checkouts, and Xcode's MCP server, `xcrun mcpbridge`, for agents |
| `zig` | The shared cache `preset.zig` as `ZIG_LOCAL_CACHE_DIR`, and a seed running `zig build` |
| `ccache` | ccache as the compiler launcher of CMake builds, with the workspace as its base directory |

### Go

The `go` seed fills Go's build cache from the default branch. Every checkout
shares that cache. With `-trimpath`, binaries, panics and `runtime.Caller`
report module paths instead of file system paths.

### Cargo

The `cargo` seed runs `cargo build --all-targets`, `cargo test --no-run`,
`cargo check --all-targets` and `cargo clippy --all-targets`.

The preset keeps its two wrappers in a [shared cache](#shared-caches) named
`preset.cargo`. It sets `RUSTC_WORKSPACE_WRAPPER` to the first, so Cargo runs
the workspace's own crates through it. For each crate, the wrapper:

- adds `-Zremap-cwd-prefix=.`, so rustc's options are the same in every
  directory;
- passes `--out-dir` relative to the workspace root when the directory lies
  inside it.

A rustc that refuses `-Zremap-cwd-prefix`, such as a stable one, gets the
crate added to
`RUSTC_BOOTSTRAP`, which lets it take the flag for that crate alone. The
wrapper then also passes `-Zallow-features=`, so the crate still uses no
unstable feature. It leaves rustc's arguments unchanged under
`RUSTC_BOOTSTRAP=-1` or when rustc no longer takes the flag. Debug info of the
workspace's crates records `.` as their working directory.

Cargo builds dependencies without the wrapper, and the wrapper passes Cargo's
target queries through unchanged, so these build as the toolchain builds them.
A `RUSTC_WORKSPACE_WRAPPER` in `[env]` replaces the wrapper.

`cargo clippy` replaces `RUSTC_WORKSPACE_WRAPPER` with `clippy-driver`. So the
preset also sets `RUSTC_WRAPPER` to the second wrapper. It applies the first
one's mapping to clippy's builds of the workspace's crates, and passes every
other rustc run through unchanged. The preset's `RUSTC_WRAPPER` replaces one
set in the inherited environment or as Cargo's `build.rustc-wrapper`, such as
sccache. One set in `[env]` replaces the preset's, and clippy then builds
without the mapping.

The seed's clippy results serve only clippy runs with the same lint arguments,
and the seed passes none after `--`. To fail on warnings, use Cargo's
`build.warnings` instead, as in
`CARGO_BUILD_WARNINGS=deny cargo clippy --all-targets`. When the clippy step
fails, such as without clippy installed or on a lint that denies by default,
the seed keeps the outputs of the other steps and notifies. New workspaces
then start without clippy's outputs. The seed builds again only once the
default branch or its settings change.

The seed first runs `cargo clean --workspace` when any of these holds:

- its toolchain, as `rustc -vV` reports it in the seed worktree, or its
  settings changed since the snapshot's build;
- the snapshot is missing, or its commit is gone;
- the new commit changes `Cargo.lock`, a `Cargo.toml`, `rust-toolchain`,
  `rust-toolchain.toml` or `.cargo/config.toml`.

`cargo clean --workspace` deletes the workspace crates' outputs and keeps the
third-party ones. Other builds start from the outputs the seed has.

Cargo keeps separate builds of the workspace's crates for each wrapper. So a
Cargo command run without the preset's environment builds them from scratch,
such as one in a shell that `shoal internal env` did not set up.

`CARGO_INCREMENTAL=1` turns incremental compilation on in every profile of a
runner job, `--release` included. `CARGO_INCREMENTAL` in the workflow's `env`
or in `[env]` replaces it, so `"0"` there turns it off.

### ccache

The `ccache` preset needs ccache and CMake 3.17 or later. It applies when a
build directory is first configured. ccache takes the workspace as its base
directory and leaves its working directory out of the hash. So a compile that
another workspace ran hits the cache, and debug info records that workspace's
directory.

### Xcode

The `xcode` preset keeps an xcconfig and an `xcodebuild` wrapper in a
[shared cache](#shared-caches) named `preset.xcode-files`. It keeps Xcode's
compilation cache and Swift package checkouts in one named `preset.xcode`. It
sets `XCODE_XCCONFIG_FILE` to the xcconfig. It puts the wrapper first on the
`PATH` of every process Shoal starts in a workspace, and of
`shoal internal env`.

The xcconfig turns compilation caching on. It maps the workspace and its
DerivedData folder out of cache keys, so a workspace reuses what another
compiled. The mapping replaces the workspace path in `#filePath` and debug info
with `/^workspace`. Each workspace still keeps its own DerivedData, since Xcode
copies cached results into it.

The wrapper passes `-clonedSourcePackagesDirPath`, keyed by the content of the
`Package.resolved` of the project it builds, unless the command passes its
own. Workspaces with the same pins share one package checkout.

These do not use the preset:

- builds from the Xcode app;
- root-level Swift packages, whose package name is the folder name;
- a configured `XCODE_XCCONFIG_FILE`.

Removing a workspace deletes the DerivedData folder of every project inside
it. Shoal finds each folder by the `WorkspacePath` that its `info.plist`
records. It looks in Xcode's custom DerivedData location when that is absolute,
and in `~/Library/Developer/Xcode/DerivedData` otherwise. Removal also deletes
the package checkouts that no build used for a day and nothing pins: no
`Package.resolved` in the registered checkout, an existing worktree or the
default branch's tip.

### MCP servers

A preset's MCP server reaches every Claude Code and Codex agent of the
repository's workspaces and chats, as
[`[mcp.NAME]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#mcp-servers)
says. Claude Code receives the servers as one `--mcp-config`. Codex receives
one `-c mcp_servers.NAME` table per server, which replaces a server of the
same name in its own config whole.

## Seed builds

A seed is a build of the repository's default branch that new workspaces start
from. A branch then rebuilds only what it changed, and shares the rest on disk.
[`[setup.seed]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#seed-builds)
and the [presets](#build-tool-presets) name its build commands and the outputs
it keeps.

### The seed build

The daemon keeps a locked, detached worktree at the local default branch's tip,
in the repository directory's `.seed`. It builds there when that branch or the
seed's settings change. The settings come from the repository's local config
and its registered checkout's file.

The daemon checks every minute, one repository at a time. It waits while memory
or free disk space is past its `[machine.overload.warning]` threshold. It
retries a failed build only after such a change or a daemon restart.

The seed's command runs as `setup.command` does. Preset commands run in the
worktree's root. The build gets the repository's `[env]`, with placeholders
rendered for the seed worktree. It gets no inherited `SHOAL_*` variables and
no agent secrets. It has a two-hour limit. It writes each command's output to
`.seed/build.log`, after a `$` line that names the command. When a command
exits, Shoal stops anything it left running in its process group.

### Snapshots

A successful build freezes the outputs as a copy-on-write snapshot: APFS clones
on macOS, reflinks on Linux, never a full copy.

On btrfs each output becomes a subvolume in the seed worktree, once a check
shows that Shoal can remove the subvolumes it makes there. Freezes, spares and
new worktrees then take a snapshot of it, which takes the same time for any
number of files. Where that check or a snapshot fails, Shoal clones the output
with reflinks.

### New workspaces

A new worktree whose own settings match the snapshot's clones the snapshot's
paths before setup. It keeps any path that already exists. The daemon keeps one
spare clone in `.seed/snapshot`, which a new worktree without any of the paths
takes instead of cloning. The daemon clones the next spare in the background
while memory and disk space are clear of their warning thresholds. A
[copy](create.md#copies) of a workspace's current files takes that workspace's
outputs instead.

When the worktree clones at least one path, Shoal gives the seed's modification
time to every tracked file its commit left unchanged. It does the same for
every directory that holds tracked files in which it changed no path. So tools
that compare modification times, such as Cargo and make, rebuild only changed
files.

Cloned files share disk blocks with the snapshot until rewritten. An output
that records an absolute path, such as the path a test binary was compiled
with, names the seed worktree until it is rebuilt. The seed worktree moves on
with the default branch, so read such paths at run time.

A failed build or clone records a `seed_failed` alert, and the workspace starts
without the seed.

With the `cargo` preset, a new workspace's first build reuses the seed's
dependency builds as they are and the incremental state of the workspace's own
crates. It recompiles only the code its branch changed and what depends on that
code. Without the preset's [wrapper](#cargo), the workspace's own crates build
from scratch.

### Outputs outside the worktree

Point the outputs that a tool keeps outside the worktree at a path inside it,
such as `xcodebuild -derivedDataPath build/DerivedData`. Workspace removal then
deletes them too.

### Remove a seed

Removing the repository or its `[setup.seed]` stops a running build. It deletes
the seed worktree, snapshot and log, and keeps anything else in `.seed`. Shoal
marks `.seed` and the seed worktree's Git metadata with the repository. It
neither builds in nor deletes a seed whose marks are missing.

## Shared caches

Each
[`[caches.NAME]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#shared-caches)
is a directory in the repository directory's `.caches/shared`, which every
workspace of the repository shares. `{cache.NAME}` names it in `[env]`, so
every process Shoal starts in a workspace uses it, and so does the seed build.

The daemon creates the directory when it starts a process or exports the
environment. It marks `.caches` with the repository. It refuses to start a
process while an unmarked `.caches` is in the way.

`shoal cleanup caches` and [disk space](load.md#disk-cleanup) cleanup empty a
repository's caches, configured or not. They do so only while none of its
tracked executions and no seed build of it runs. They keep `preset.cargo` and
`preset.xcode-files`, which hold only the [presets'](#build-tool-presets) own
files. Disk space cleanup records a `caches_emptied` alert. Processes started
with an exported environment are not tracked, so they do not keep a cache from
being emptied.

Removing the repository deletes `.caches`.

## Temporary directories

Every workspace and chat has its own directory for temporary files, in the
repository directory's `.tmp`, named by the workspace's or chat's ID. Each of
these gets it as `TMPDIR`, so their scratch files stay out of the shared
`/tmp`:

- every process Shoal starts in a workspace;
- its hooks, other than `hooks.post_remove`;
- `shoal internal env`;
- a chat's agent.

A `TMPDIR` in `[env]` replaces it.

The daemon creates the directory when it starts a process in the workspace,
exports its environment or records the chat. Only you can read it. For a
registration without a repository directory, the daemon reserves one. It marks
`.tmp` with the repository, and refuses to start the process or chat while an
unmarked `.tmp` is in the way.

A chat without a repository has its directory in the `.tmp` of the `root_dir`
it started in. It stays there when `root_dir` changes, until
[`repo move`](repo.md#move-repositories-to-a-new-root) takes it along. The
daemon marks that `.tmp` with its state directory. A daemon with another state
directory that shares `root_dir` neither uses nor deletes it, and starts no
chat without a repository there.

Shoal keeps nothing in it. Removing the workspace or chat deletes the directory
without a checkpoint. Cleanup deletes the directories that belong to no
workspace or chat of the repository, or to no chat without a repository.
Removing the repository deletes `.tmp`.
