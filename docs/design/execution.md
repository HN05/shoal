# Execution

This file covers the execution wrapper and the processes Shoal starts:
terminals, reattachment, systemd scopes, process identity and configured
commands.

The execution wrapper owns terminal I/O, environment delivery, exit codes, and
command process groups. It retains buffered daemon controls across start, stop,
and recovery transitions so adjacent frames cannot be lost. A command takes the
terminal as its foreground job before it runs, without waiting for its
registration, so its first read or terminal setting never stops it. The wrapper
starts it with the default stops for background terminal use, whatever the
wrapper inherited, so a command that uses the terminal before it has it waits
instead of failing. After commands it restores the caller's foreground group and
OS terminal settings. When `TERM` is nonempty and not `dumb`, it also resets
emulator input modes for the shell, writing best-effort cleanup directly to the
terminal even when output is redirected. It registers through one request
carrying its execution kind, and gives the entire command process group a shared
grace period on stop requests, even after its leader exits. The daemon prepares
each kind before shared registration. The daemon never proxies terminals, and a
lost connection is not proof that an execution stopped or that its resources are
free. Command wrappers outlive the daemon instead: on a lost connection or a
shutdown's detach control they keep the command and terminal, retry the socket
with capped backoff, and reattach with the execution ID, wrapper and child
identities, process group and the execution's scope token. The daemon verifies
these against the record, restores scope and agent metadata, and serves the
connection as before; a command that exits meanwhile reports once reattached. A
refusal stops the command as manual stop does. Reattachment is a permanent
protocol method accepted at every version, and wrappers advertise it when they
register: a restart or an update detaches those and stops the rest as manual
stop does, within the workspace stop timeout. Setup and merges hold daemon
gates, so they never reattach.

Invoke external tools with argument arrays. Captured subprocesses share optional
deadlines, whose error names the command line and directory, and a single
diagnostic limit, terminate on cancellation, and drain output while sending
input.

The unit and every scope Shoal starts set `OOMPolicy=continue`, so an OOM kill
ends only the process it chose and the daemon's own death alone fails the
service, except that a CI job's scope has the kernel kill all of it with any one
of its processes, as overload protection describes. Every process the daemon
starts in a session of its own runs in a transient systemd scope of its own in
the service's slice, which `workers::detached_command` adds through the launcher
the managed daemon finds at startup, so that systemd's stop of the service after
the daemon exited or failed to start leaves agents, places and CI jobs running
for the next daemon. The command a wrapper starts for work runs in a scope of
its own in `shoal-work.slice` instead, which the wrapper starts through
`systemd-run --scope` wherever it runs, so an agent in a terminal host's place
keeps that terminal yet leaves the host's control group for one Shoal controls:
the slice holds Shoal's work, and only it, for the memory limit and the kernel's
accounting. `systemd-run --scope` runs the command in its own place, so its PID,
process group, terminal and environment stay as a direct start leaves them. The
wrapper finds the launcher once, trying a scope; where none starts, it starts
commands directly. It names each scope for the space whose work it runs, so what
a command leaves there stays tied to the space after it ends. `KillMode=process`
is rejected: it would leave them in the stopped service's control group, which
the next start reports as left over, sharing its memory accounting and OOM
kills. A live daemon's SIGTERM still stops agents. Where the daemon's startup
finds that no scope can start, workers start directly; a start that
`systemd-run` fails later is not retried directly, since its exit status cannot
be told from the worker's own.

Every process Shoal starts, an execution, a hook, an export or a CI job's step,
names its space in `SHOAL_SPACE` whatever its kind, and only a space with a
worktree gets the worktree's variables, so a chat in a workspace's worktree
carries none of the workspace's. Hooks are deliberately untracked user processes
with the space identity but no scope token, because their purpose is to start or
stop things that outlive the hook (a tmux session, say) without becoming
execution survivors. Nothing outlives the workspace: removal stops every
verified process that still carries its identity, including those hooks and
exported environments started, and its leftovers. A leftover is a detached
process, one whose parent exited and left it to launchd, init or the subreaper
that takes the daemon's orphans (such as `systemd --user`), or a descendant of
one, that the space's worktree, temporary directory, program, identity or, on
Linux, the scope of its work ties to it, with everything it started: no live
process outside the space keeps it, and the ties cover platform binaries whose
hidden environment cannot show the identity and programs built in the worktree
that run elsewhere. Exclusion follows live ancestry, not environment variables:
the processes of tracked executions, those that started them, and terminal
session servers are never untracked. `shoal stop` takes the same processes
through the same operation after the executions, and `--processes` and
`--leftovers` take them alone, so an agent stops what its own commands left
behind without stopping itself. Automatic cleanup counts a leftover working in
the worktree as use instead of stopping it, but stops a program built there and
left running elsewhere. Leftovers are reported, not stopped on sight: the
cleanup sweep finds every space's from one process listing and keeps them in
memory, so no tool call waits for a scan, and the agent's hook names each new
one once, since the agent knows whether its own test servers should keep
running. They create no alert; `shoal status` lists them for the user. The same
holds once an agent exits, whatever place it ran in: what its scope keeps is
reported as the space's leftovers, since a restored agent may want its servers
back, and goes with the space. A CI job's are stopped as it ends, and named in
its workspace's log, since no agent is left to decide and the workspace goes.

Named `[commands]` are executable/argument arrays resolved per name from saved
repository config, worktree config, then global config and built-in defaults at
launch. They run through the tracked wrapper with workspace scope, terminal I/O,
and literal extra arguments. `shoal run <name>` is their only name, so a
configured command never competes with a built-in: there is no `shoal <name>`
shorthand, and an unknown top-level name prints clap's error, with its
suggestions of similar built-ins, followed by one fixed line pointing to `shoal
run <name>`, without reading configuration. `run` lists the resolved arrays with
their layers. Without a current or explicit workspace, repository-only commands
pick from the workspaces whose configuration defines them. An unknown name
reports a command error and never opens a picker. Workspace fields expand once
within individual arguments; a standalone `{args}` places the caller's literal
arguments. `{diff_base}` lazily uses `diff`'s daemon lookup. Built-in `edit`
runs the `edit` command, otherwise `$VISUAL` or `$EDITOR` with the workspace
path, through the same wrapper, so the editor gets scope and port variables and
counts as activity.
