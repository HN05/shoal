# Daemon

This file says what the `daemon` commands do, how the daemon restarts and
recovers, and how it treats untracked processes. [Daemon](../design/daemon.md)
and [execution](../design/execution.md) give the reasons.

```sh
cargo install --path .
shoal daemon install --dry-run     # Preview the OS service definition
shoal daemon install               # Register and start the per-user service
shoal daemon stop|start|restart
shoal status                       # Outside a space: the daemon, alerts, stalls, cleanup, memory limit
```

## The service

The daemon runs as a per-user service:

- macOS uses a launchd LaunchAgent in `~/Library/LaunchAgents`, which needs a
  GUI login session.
- Linux uses a systemd user service.

The service commands target the one registered service. Its state directory
must match theirs. To change the service's state directory, first stop it with
its existing `--state-dir`.

A scoped process runs the service commands and `shoal update` as its space's
`daemon` [permission](modes.md#permitted-actions) allows.

`shoal daemon start` without an installed service offers to run
`shoal daemon install` in an interactive terminal.

### Install the service

`shoal daemon install` registers and starts the service. The service starts
the executable by the path you invoked it with, keeping a symlink as it is. It
also captures the current `PATH`, so install the runtime dependencies and any
hook tools first.

Installing also creates the global config, `~/.config/shoal/config.toml` or
`$XDG_CONFIG_HOME/shoal/config.toml`, from
[default.toml](../../crates/shoal-core/configs/default.toml). That file shows
every setting commented out with its default and sets none, so each release's
defaults apply. Installing never touches an existing config.
[`shoal config`](config.md) edits, replaces and reloads it.

`--dry-run` prints the service definition and writes nothing.

Installing prints hints for what is missing:

- the shell initialization line, when neither `.bashrc` nor `.zshrc` (under
  `ZDOTDIR` when set) runs it;
- the default terminal host, when it is not installed, with one to install or
  an installed host to set as `terminal.default_host`;
- `shoal skill install`, when no AI tool has Shoal's skill.

A running daemon of a compatible release keeps running, with its commands,
until it restarts. Installing restarts a daemon of an incompatible release.

### Keep the service definition current

A daemon its service manager started checks the installed definition of its
state directory at startup. When its release renders the definition
differently, it rewrites it, keeping the executable and `PATH` it starts.
It restarts nothing:

- On Linux it reloads systemd, and the running service applies the change at
  once.
- On macOS launchd reads the rewritten plist at the service's next load.

A failed rewrite raises an `environment_problem` alert that names
`shoal daemon install`.

### Run in the foreground

Without a service manager, run the daemon in the foreground and give every
command the same state directory:

```sh
shoal --state-dir /tmp/shoal-dev daemon run
shoal --state-dir /tmp/shoal-dev status
```

`SHOAL_STATE_DIR` sets the state directory too. Stop a foreground daemon
yourself.

## State and logs

State lives in `~/.local/state/shoal` by default. The runtime state and the
Unix socket are private to the user.

The daemon starts each diagnostic line with its local time, and drops a line it
cannot write. On macOS the lines go to `daemon.log` in the state directory. On
Linux, read them with `journalctl --user -u shoal.service`.

## Daemon status

`shoal status` outside a space shows the daemon's PID, version and unread
alerts. It exits 1 when the daemon is offline.

It also lists each stalled directory. A directory stalls when its file access
times out, as under heavy memory pressure, or while macOS waits for an answer
to a privacy prompt, whichever process the prompt is for. A worktree check that
runs 30 seconds, or is interrupted, stalls the directory that holds the
worktree:

- An `access_stalled` alert names the directory once. On macOS it names a
  privacy prompt as a possible cause.
- Status lists the directory until a check there finishes.

Status reports automatic cleanup in `cleanup_error` when a step fails outside a
workspace, or when no pass has finished for 10 minutes.
[Workspace status](workspaces.md#status) reports a step that fails in a
workspace.

Status names the limit on work memory in effect, in `work_memory`, and
whether `load` or `memory_limit` sets it.

Status needs no request capacity, and neither do `daemon stop` and `restart`.
When a daemon with full request slots refuses to say which space the current
directory is in, `shoal status` warns and shows the daemon's status.

### A daemon that is starting

While the daemon starts, it answers under the `starting` error code. The
answer names the running step, a stalled directory, or a path whose check has
waited five seconds.

Commands that wait for a daemon to start, such as `daemon install`, keep
waiting while it answers so. They give up once it has not answered for 10
seconds.

### A daemon that does not answer

Every command reports, with its PID, a daemon that owns the state directory
but does not answer.

## Daemon restarts

How the daemon exits decides what becomes of tracked agents and commands.
`shoal daemon stop` and `restart` ask the daemon over its socket how to exit,
whatever its release. A daemon that does not answer, as while it starts, gets
SIGTERM instead, through the service manager or directly for a foreground
daemon.

### Exits that keep work running

These leave agents and commands running, to
[reattach](#reattach-to-the-next-daemon) to the next daemon:

- `shoal daemon restart`;
- `shoal daemon install` when it replaces an incompatible daemon;
- an [update](install.md#after-an-update);
- a crash or a failed start.

A restart names the `shoal resume` of each space whose work could not
reattach.

The service manager's stop of the service, once the daemon has exited, leaves
them running too. On macOS launchd ends only the daemon's own process group,
which every worker has left. On Linux each worker runs in a systemd scope of
its own, which needs systemd 254 or later and a user bus. Without one, a
worker runs in the service's control group, which systemd's stop ends.

### Stop

`shoal daemon stop` stops agents and commands as `shoal stop --all` does,
[chats'](chats.md) agents included, so they stay down until `shoal resume`.
Then it stops the daemon and names the `shoal resume` of the spaces it stopped.

Unlike `shoal stop`, it keeps the agents waiting in the
[agent queue](queue.md) to start in existing workspaces. The next daemon starts
them.

### Signals

SIGTERM or SIGINT, as a shutdown, a logout or a service manager's stop sends,
stops agents and commands as the daemon does before a
[system shutdown](load.md). Each wrapper saves its agent first. The restores
that need no user wait in the agent queue. CI jobs keep running for the next
daemon.

The service definition's `ExitTimeOut` (launchd) or `TimeoutStopSec` (systemd)
sets how long the stop may take before the service manager kills the daemon.

### Work the daemon runs itself

Agents and CI jobs outlive the daemon, but the work the daemon runs itself does
not:

- preparing, stopping, restoring or removing a workspace;
- an operation, such as a PR's push or a merge;
- a connected `shoal setup` or merge;
- a cleanup sweep or a checkout sync.

So `shoal daemon stop` and `restart` first wait until none of this runs. They
print what they wait for each time that changes, and `--json` lists it in
`waited_for`.

While they wait, the daemon starts no cleanup sweep, checkout sync, CI job or
queued launch. These wait for the next daemon. The daemon refuses requests
that would start such work, such as `shoal start`, `shoal rm` and
`shoal pr open`:

- During a restart, their commands print that they wait, and run on the next
  daemon.
- During a stop, which no daemon follows, they fail.

Restores of running agents and other requests go on as usual.

Flags change the wait:

- `--no-wait` refuses instead, exits 2 and lists the work in `blocked_by`. It
  still waits for requests and background passes, which end on their own.
- `--force` exits at once and cuts the work off. The next daemon marks each
  workspace it interrupted failed.

Interrupting the wait leaves the daemon running and releases what it held back.

### Reattach to the next daemon

While the daemon is away, each wrapper keeps its command and terminal and
writes nothing. It reconnects once a daemon listens again. Until then, the
command's own `shoal` calls fail.

The new daemon checks the execution record, the scope token and the process
identities. It then restores the agent's name and its automatic recovery
setting, so stopping, overload protection, exit alerts and
`hooks.post_agent_exit` work as before. When the daemon refuses an execution,
the wrapper stops the command and saves what `shoal stop` saves.

A command that exits while the daemon is away reports its exit code once
reattached. Its wrapper prints that it is waiting. It gives up after two
minutes, or on Ctrl-C, and leaves the execution for `shoal stop`.

Executions that wait to reattach stay active for cleanup and removal. An
agent's wait for its automatic restore after a protection stop continues the
same way.

Setup and merges cannot reattach. That is why stops and restarts wait for them.

## Untracked processes

An untracked process is a space's process that no tracked execution runs, so
nothing saves it when it stops.

```sh
shoal stop --processes [space...]   # Every untracked process of the spaces
shoal stop --leftovers [space...]   # Only their leftovers
```

Both act on the current workspace or chat by default, else on those a picker
selects. Neither takes the spaces below. Once its agents and commands have
stopped, `shoal stop` stops the untracked processes too, and counts them as
other processes.

A workspace process may stop the untracked processes and leftovers of its own
workspace and of those below it. A Claude Code agent runs both forms without a
permission prompt.

### What belongs to a space

A space's untracked processes are those that carry its identity
(`SHOAL_SPACE`), and its leftovers.

A leftover is a process whose parent exited, so that launchd or init adopted
it, or on Linux the subreaper that adopts the daemon's orphans, such as
`systemd --user`. A process that descends from one counts too. Such a process
is the space's leftover when it does one of these:

- works in the worktree or the space's temporary directory;
- runs a program inside the worktree;
- carries the space's identity;
- on Linux, runs in the scope of the space's [work](load.md#work-slice).

Every process a leftover started is a leftover too, wherever it runs. So what
an agent's command left in its scope stays the space's leftover after the agent
exits, until `shoal stop` or the space's removal stops it.

A process whose parent still runs elsewhere, such as a shell the user opened in
the worktree, is no leftover.

These are never untracked processes:

- the processes of tracked executions and chat agents, and those that started
  them;
- terminal session servers, such as zmx, tmux or Zellij;
- the process that asks.

### Find leftovers

The daemon looks for every space's leftovers in each cleanup sweep, from one
listing of the machine's processes. It keeps them in memory until the next
sweep, so the first sweep after a restart finds them again.

`shoal status` shows a workspace's leftover count and the PID and command of
the first ten. `--json` gives `leftover_count`, and a `leftovers` list of the
first ten, each with `pid`, `command`, `directory` and `started_at` in Unix
seconds.

The agent's hook names each new leftover once, up to ten per delivery, as
[delivery hooks](alerts.md#delivery-hooks) describes. Leftovers raise no alert.

### Stop them

Shoal checks each process against its PID and start time before every signal.
It sends SIGTERM, then SIGKILL after a grace period. A stop names the processes
that survive.

## Recovery

```sh
shoal inbox                         # Failed workspaces and untracked worktrees, with their fix
shoal alert list                    # Problems the daemon found when it started or reloaded
shoal stop 3                        # Stop survivors; asks before forgetting an unprovable execution
shoal setup 3 --skip                # Accept a failed setup
shoal branch rename 3 topic         # Finish an interrupted rename, then rename
shoal adopt <path>                  # Reclaim a worktree at its recorded path after checking it
```

Each problem shows where you look and names the command that resolves it.

### Environment checks

The daemon checks its environment when it starts and when `config reload`
rereads its config:

- that `git` is executable on its `PATH`;
- that the host the global `terminal.default_host` names is installed;
- that it can create a file in `root_dir`;
- which registered checkouts set `core.bare` although they have a working tree;
- whether a global rule repeats Shoal's guidance;
- which forge each recorded host serves, asking it again unless
  `[machine.forges]` sets its type. A host that cannot be asked keeps its
  answer.

Each problem becomes an `environment_problem` alert. A missing optional tool,
such as `fzf`, raises none.

The inbox lists the Git worktrees under registered repository roots that
Shoal does not track. A command that cannot reach a matching daemon says so
itself, and `shoal cd` says when shell integration is not loaded.

### Startup

Before it accepts requests or runs cleanup, the daemon marks, in one
transaction:

- unfinished simulator cleans interrupted;
- transient lifecycle operations failed;
- disconnected executions unknown, until their wrappers reattach.

It then audits the worktrees. The audit deletes no files and releases no
leases.

A workspace the audit failed, such as one on a volume that mounted late or
whose file access stalled, becomes ready again once a cleanup sweep verifies
its ownership. Other failures, such as a failed setup, wait for you.

### Repair

`shoal adopt` and `shoal setup --skip` request a repair. Repair restores
verified worktrees to ready and clears executions proven stopped. It keeps
work and leases.

A moved worktree must return to its recorded path. The next cleanup sweep or
`shoal rm` forgets a deleted directory, and keeps the branch.

A `shoal-workspace` marker in the worktree's Git admin directory proves
ownership, so ownership survives reboots, device renumbering and restores.
When the marker does not verify ownership, check the worktree at the recorded
path yourself and [adopt it](workspaces.md#adopting-a-worktree) again.

### Process tracking

An execution records its wrapper's and child's identities and a process group.
Descendants inherit `SHOAL_EXECUTION_ID`. Recovery finds survivors by that
marker and by live ancestry.

On macOS, the kernel's record of each process's parent ties a command's
descendants to it after it exits. A descendant whose readable environment lacks
the marker, such as a demo it started, is the exception. A descendant whose
environment is hidden and that launchd adopted stays the command's when the
daemon's process sampling saw it, or its exited parent, while the command's
processes lived.

Shoal rechecks identities before it signals, and never kills an unverified
candidate. A stop gives the command's whole process group one shared grace
period before it forces termination, even when the group's leader exits
first.

When an agent, or a command other than setup, exits, Shoal also stops its
verified processes outside its process group, such as background shell
commands. Run a process that should keep running as a [demo](demos.md) or from
a hook.

Detection is cooperative. Hidden environments, cleared markers and old records
can leave it uncertain.

### Survivors

A reported command exit keeps an unknown execution, and keeps setup from
becoming ready, while one of these remains:

- a survivor of the child or its process group;
- a live process whose environment Shoal cannot read and that works inside the
  workspace.

Exiting processes do not count. Shoal reads a process that is still
publishing its environment after exec again for up to two seconds. A settled
empty environment is readable and identifies no execution.

macOS hides the environments of its own binaries, such as shells. Such a
process counts as its nearest ancestor with a readable environment does. When
launchd adopted it after its parent exited, it counts as unreadable unless it
exits or shows its own environment within another two seconds.

A failed setup verification keeps the workspace. It reports the command's exit
status, the blocking PIDs or the inspection error, and the recovery commands.
It, and a blocked restore, name the `kill` command that ends the processes
Shoal cannot verify and does not stop itself.

`shoal stop` names unreadable PIDs without showing their arguments or
environments. It asks you to confirm that an execution without a launch record
ended. Visible live processes still block.
