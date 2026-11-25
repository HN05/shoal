# Daemon

This file says what the `daemon` commands do, how the daemon restarts and
recovers, and how it treats untracked processes.

```sh
cargo install --path .
shoal daemon install --dry-run     # Preview the OS service definition
shoal daemon install               # Register and start the per-user service
shoal daemon stop|start|restart
shoal status                       # Outside a space: the daemon, alerts, stalls, cleanup, memory limit
```

macOS uses a launchd LaunchAgent in `~/Library/LaunchAgents` (requires a GUI
login session); Linux uses a systemd user service, whose unit sets
`OOMPolicy=continue` so that the kernel's OOM killer ending one process in it,
other than the daemon, leaves the service and its other processes running. Without a service manager,
run `shoal --state-dir /tmp/shoal-dev daemon run` in the foreground and target
it with the same `--state-dir` (or `SHOAL_STATE_DIR`). State defaults to
`~/.local/state/shoal`; runtime state and the Unix socket are private to the
user. Service commands target the one registered per-user service, whose state
directory must match. At startup, a daemon its service manager started rewrites
the installed definition of its state directory when its release renders it
differently, keeping the executable and `PATH` it starts, and on Linux reloads systemd, which the running service applies at once;
it restarts nothing, launchd reads a rewritten plist at the service's next load,
and a failure is an `environment_problem` alert naming `shoal daemon install`. `shoal status` outside a space shows the daemon's PID,
version and unread alerts, exits 1 when it is offline, and names each
directory whose file access timed out, as under heavy memory pressure or while
macOS waits for an answer to a privacy prompt, whichever process the prompt is
for: a worktree check that runs 30 seconds or is interrupted reports the
directory holding the worktree once as an `access_stalled` alert, which on
macOS names a privacy prompt as a possible cause, and it stays listed until a check there
finishes. It also says, in `cleanup_error`, when a step of automatic cleanup
fails other than in a workspace or no pass has finished for 10 minutes, and
names the limit on work memory in effect, in `work_memory`, with whether
`load` or `memory_limit` sets it. While the daemon starts, its answer names the step, a stalled
directory, or a path whose check has waited five seconds, under the
`starting` error code; the commands that wait for a daemon to start, such as
`daemon install`, keep waiting while it answers so and give up once it has
not for 10 seconds. A daemon that
owns the state directory but does not answer is reported with its PID by every
command. The status request, `daemon stop` and `restart` need no request capacity;
when a daemon with full request slots refuses to say which space the current
directory is in, `shoal status` warns and shows the daemon's status.
`daemon stop` and `restart` send a daemon that does not answer SIGTERM, through
the service manager or directly for a foreground daemon, as while it starts.
Both first [wait for the work the daemon runs itself](#daemon-restarts).
A scoped process runs the service commands and `shoal update` as its space's
`daemon` [permission](modes.md#permitted-actions) allows.
`daemon start`
without an installed service offers to run `shoal daemon install` in an interactive terminal.

`shoal daemon install` preserves the invoked executable's symlink path and captures the
current `PATH` for the service, so install runtime dependencies and any hook
tools first. It also creates `~/.config/shoal/config.toml` (or
`$XDG_CONFIG_HOME/shoal/config.toml`) from
[default.toml](../../crates/shoal-core/configs/default.toml), which shows every
setting commented out with its default and sets none, so each release's
defaults apply, never touching an existing file; `--dry-run` writes
nothing. Installation prints
the shell initialization hint only when neither `.bashrc` nor `.zshrc` (under
`ZDOTDIR` when set) contains an active initialization command, and names the
default terminal host when it is not installed, suggesting one to install or an
installed host to set as `terminal.default_host`, and suggests `shoal skill
install` when no AI tool has Shoal's skill. [`shoal config`](config.md)
edits, replaces and reloads the file. Setup preserves compatible
daemons and commands until restart; incompatible daemons restart automatically.
Changing the service state directory requires stopping it with its existing
`--state-dir` first.
Stop foreground daemons manually. The daemon starts each diagnostic line with its
local time and drops lines it cannot write. macOS diagnostics go to `daemon.log` in the
state directory; Linux uses `journalctl --user -u shoal.service`.

## Daemon restarts

How the daemon exits decides what becomes of tracked agents and commands:

- `shoal daemon restart`, `shoal daemon install` when it replaces an
  incompatible daemon, an update, a crash and a failed start leave them
  running, and so does the service manager's stop of the service once the
  daemon exited. launchd ends only the daemon's own process group, which every
  worker the daemon starts in a session of its own has left. On Linux each
  such worker, such as the wrapper of an agent or a CI job, runs in a
  transient systemd scope of its own in the service's slice, with
  `OOMPolicy=continue`, where systemd 254 or later and a user bus start one
  when the daemon starts; otherwise it runs in the service's control group,
  which systemd's stop ends. What a wrapper starts runs in the
  [work slice](load.md#work-slice). Wrappers of releases before reattachment stop
  instead, and `shoal daemon restart` names the `shoal resume` of the spaces
  they ran in.
- `shoal daemon stop` stops them, [chats'](chats.md) agents included, as
  `shoal stop --all` does, so they stay down until `shoal resume`, then stops
  the daemon and names that `shoal resume` with the spaces it stopped. Unlike
  `shoal stop`, it keeps
  agents waiting in the [agent queue](queue.md) to start in existing
  workspaces, which the next daemon starts.
- SIGTERM or SIGINT, as a shutdown, a logout or a service manager's stop sends,
  stops them as the daemon does before a system shutdown: each wrapper saves its
  agent first, and restores that need no user wait in the agent queue. CI jobs
  keep running for the next daemon, as on a restart. The
  service definition's `ExitTimeOut` (launchd) or `TimeoutStopSec` (systemd)
  gives the stop time before the service manager kills the daemon.

Agents and CI jobs outlive the daemon, but the work the daemon runs itself does
not: preparing, stopping, restoring or removing a workspace, an operation such
as a PR's push or a merge, a connected `shoal setup` or merge, and a cleanup
sweep or checkout sync. `shoal daemon stop` and `restart` therefore wait until
none runs, printing what they wait for each time that changes; `--json` lists
it in `waited_for`. While they wait, the daemon starts no cleanup sweep,
checkout sync, CI job or queued launch, which wait for the next daemon, and
refuses requests that would start such work, such as
`shoal start`, `shoal rm` and `shoal pr open`: during a restart their
commands print that they wait and run on the next daemon, while a stop, which
no daemon follows, fails them. Restores of running agents and other requests
go on as usual. `--no-wait` refuses instead, exiting 2 and listing the work in
`blocked_by`, though it still waits for requests and background passes, which
end on their own; `--force` exits at once and cuts the work off, and the next
daemon marks each workspace it interrupted failed. Interrupting the wait
leaves the daemon running and releases what it held back.

The service commands ask the daemon how to exit over its socket, whatever its
release; one that does not answer gets SIGTERM. When work keeps running, its
wrappers keep the command and terminal, write nothing while it runs, and
reconnect once a daemon listens again; until then the command's own
`shoal` calls fail. The new daemon checks the execution record, scope token and
process identities, then restores the agent's name and automatic recovery
setting, so stop, overload protection, exit alerts and
`hooks.post_agent_exit` work as before. A command that exits while the daemon is
away reports its exit code once reattached; after it exits the wrapper prints
that it is waiting and gives up after two minutes or on Ctrl-C, leaving the
execution for `shoal stop`. When the daemon refuses an execution, the wrapper
stops the command and saves what `shoal stop` saves. Waiting executions stay
active for cleanup and removal. An agent's wait for its automatic restore after
a protection stop continues the same way. Setup, merges and wrappers from releases before
reattachment stop on a restart as `shoal stop` does, and `shoal daemon restart`
then suggests `shoal resume --all`.

## Untracked processes

Once its agents and commands have stopped, `shoal stop` stops the workspace's
untracked processes too, which nothing saves, and counts them as other
processes. `shoal stop --processes [space...]` stops only the untracked
processes of workspaces or chats, by default the current one, else those
picked several at once, and
`shoal stop --leftovers [space...]` only their leftovers; neither takes the
workspaces below. The daemon looks for every space's leftovers in each cleanup sweep, from one
listing of the machine's processes, and keeps them in memory until the next
one; the first sweep after a restart finds them again. `shoal status` shows a
workspace's count and the PID and command of the first ten, and with `--json`
`leftover_count` and a `leftovers` list of the first ten with `pid`, `command`,
`directory` and `started_at` in Unix seconds. Its agent's hook names each new
one once, up to ten per delivery, and they create no alert. A space's untracked processes carry its identity
(`SHOAL_SPACE`) or are its leftovers:
processes whose parent exited, so that launchd or init adopted them, or on
Linux the subreaper that adopts the daemon's orphans, such as `systemd --user`,
or that descend from such a process, working in the worktree or the space's temporary
directory, running a program inside the worktree, carrying its identity,
or on Linux running in the scope of the space's [work](load.md#work-slice), with
every process they started wherever it runs. So what an agent's command left
in its scope stays the space's leftovers after the agent exits, until
`shoal stop` or the space's removal stops them. Never among them are the
processes of tracked executions and chat agents, the processes that started
those, terminal session servers such as zmx, tmux or Zellij, and the process that asks.
A process whose parent still runs elsewhere, such as a shell the user opened in
the worktree, is not a leftover. Each process is checked against its PID and
start time before every signal and gets SIGTERM, then SIGKILL after a grace
period; a stop names the processes that survive. A workspace process may stop
the untracked processes and leftovers of its own workspace and those below it,
and a Claude Code agent runs both forms without a permission prompt.

## Recovery

```sh
shoal inbox                         # Failed workspaces and untracked worktrees, with their fix
shoal alert list                    # Problems the daemon found when it started or reloaded
shoal stop 3                        # Stop survivors; asks before forgetting an unprovable execution
shoal setup 3 --skip           # Accept a failed setup
shoal branch rename 3 topic             # Finish an interrupted rename, then rename
shoal adopt <path>             # Reclaim a worktree at its recorded path after checking it
```

Each problem shows where you look and names the command that resolves it. When
the daemon starts and when `config reload` rereads its config, it checks that
`git` is executable on its PATH, that the host the global
`terminal.default_host` names is installed, that it can create a
file in `root_dir`, which registered checkouts set `core.bare` although they have
a working tree, whether a global rule repeats Shoal's guidance, and which forge each
recorded host serves, asking it again unless `[machine.forges]` sets its type. Each
problem becomes an `environment_problem` alert; a missing optional tool such as
`fzf` raises none, and a host that cannot be asked keeps its answer. The inbox lists
Git worktrees under registered repository roots that Shoal does not track. A
command that cannot reach a matching daemon says so itself, and `shoal cd` says
when shell integration is not loaded.

Before accepting requests or
running cleanup, daemon startup atomically marks unfinished simulator cleans
interrupted, transient lifecycle operations failed, and disconnected executions
unknown until their wrappers reattach, then audits worktrees without deleting files or releasing leases.
A workspace the audit failed, such as one on a volume that mounted late or
whose file access stalled, becomes
ready again once the cleanup sweep finds its ownership verified; other failures,
such as a failed setup, wait for you. Repair, as `adopt` and
`setup --skip` request it, restores verified worktrees to ready and clears
executions proven stopped, preserving work and leases. Moved worktrees must return
to their recorded path, and a deleted directory is forgotten by the next
cleanup sweep or `shoal rm`, retaining the branch. Ownership is proven by a
`shoal-workspace` marker in the worktree's Git admin directory, so it survives
reboots, device renumbering and restores. When the marker does not verify
ownership, check the worktree at the recorded path yourself and
[adopt it](workspaces.md#adopting-a-worktree) again.

Executions record wrapper and child identities plus a process group. Descendants
inherit `SHOAL_EXECUTION_ID`, which recovery uses with live ancestry to find
survivors; on macOS, the kernel's record of each process's parent also ties a
command's descendants to it after it exits, short of a descendant whose readable
environment lacks the marker, such as a demo it started. A descendant whose hidden
environment launchd adopted stays the command's when the daemon's process sampling
saw it, or its exited parent, while the command's processes lived. Identities are rechecked before signaling, and unverified candidates are
never killed. Stopping gives the entire command process group a shared grace period
before forced termination, even if its leader exits first. When an agent or a
command other than setup exits, Shoal also stops its verified processes outside
its process group, such as background shell commands; run a process that should
keep running as a [demo](demos.md) or from a hook. Detection is cooperative:
hidden environments, cleared markers, and old records can leave it uncertain.
A reported command exit retains an unknown execution and prevents setup readiness
while child/group survivors or live unreadable environments remain; an
unreadable process counts only when it works inside the workspace. Exiting
processes do not count, and a process still publishing its environment after exec
is read again for up to two seconds; a settled empty environment is readable and
does not identify an execution. macOS hides the environments of its own
binaries, such as shells, which then count as their nearest ancestor with a
readable environment does, or as unreadable when launchd adopted them after
their parent exited and they neither exit nor show their own environment within
another two seconds. Setup verification failures retain the workspace
and report the command's exit status, blocking PIDs or inspection error, and
recovery commands; they and blocked restores name the `kill` command that ends
the processes Shoal cannot verify and does not stop itself. `shoal stop` names
unreadable PIDs without exposing their arguments or environments, and asks you
to confirm that an execution without a launch record ended; visible live
processes still block.
