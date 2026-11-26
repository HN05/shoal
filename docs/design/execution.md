# Execution

This file covers the execution wrapper and the processes Shoal starts: their
terminals, reattachment, systemd scopes, process identity and configured
commands.

## The execution wrapper

The execution wrapper runs a command for Shoal. It owns the command's terminal
I/O, environment, exit code and process group. The daemon never proxies a
terminal, so terminal I/O stays in the process that has the terminal.
[Process tracking](../reference/daemon.md#process-tracking) says how a stop
ends the command's process group.

A wrapper registers through one request that carries its execution kind. The
daemon prepares each kind, then registers every kind the same way. The wrapper
keeps the daemon's controls in a buffer across start, stop and recovery, so it
never loses a control that arrives right after another.

## The terminal

A command takes the terminal as its foreground job before it runs. It does not
wait for its registration, so its first read or terminal setting never stops
it.

The wrapper starts the command with the default stops for background terminal
use, whatever the wrapper inherited. A command that uses the terminal before it
has it then waits instead of failing.

After a command ends, the wrapper gives the caller back its foreground group
and terminal settings. When `TERM` is set and is not `dumb`, it also resets the
emulator's input modes for the shell. It writes that reset to the terminal
itself, even when output is redirected, and ignores a failure.

## Reattachment

A lost connection does not prove that an execution stopped or that its
resources are free. Wrappers therefore outlive the daemon. When the connection
drops, or a shutdown tells them to detach, they keep the command and its
terminal. They retry the socket with a capped backoff.

A wrapper reattaches with what identifies its execution: the execution ID, the
wrapper's and child's identities, the process group and the execution's scope
token. The daemon checks these against its record. It then restores the scope
and the agent's metadata and serves the connection as before. When it refuses
the execution, the wrapper stops the command as a manual stop does.
[Reattach to the next daemon](../reference/daemon.md#reattach-to-the-next-daemon)
says what the user sees meanwhile.

Reattachment is a permanent protocol method, which every version accepts, since
an upgrade leaves wrappers of the previous release running. A wrapper says at
registration that it can reattach. A restart or an update detaches those
wrappers and stops the others as a manual stop does, within the workspace stop
timeout.

Setup and merges hold daemon gates, so they never reattach. The daemon waits
for them before it restarts.

## Subprocesses Shoal waits on

Shoal runs the tools whose output it reads through one helper:

- An optional deadline covers the whole exchange. Its error names the command
  line and the directory, so it says what hung and where.
- Errors keep one bounded amount of the tool's stderr, which fits in a protocol
  frame.
- Cancelling the caller kills the tool.
- The helper drains the tool's output while it sends input, so a full pipe
  never blocks either side.

## Systemd scopes

On Linux, each process Shoal starts runs in a transient systemd scope of its
own. Who starts the process decides the slice the scope sits in.

### Processes the daemon starts

Every process the daemon starts in a session of its own, such as a worker, a
place or a CI job, runs in a scope in the service's slice. The managed daemon
finds the launcher for these scopes at startup. systemd's stop of the service,
after the daemon exited or failed to start, then leaves agents, places and CI
jobs running for the next daemon. A live daemon's SIGTERM still stops agents.

The service does not set `KillMode=process` instead. That would leave these
processes in the stopped service's control group. The next start would report
them as left over, and they would share its memory accounting and OOM kills.

Where the daemon's startup finds that no scope can start, it starts workers
directly. When `systemd-run` fails a start later, the daemon does not retry it
directly, since `systemd-run`'s exit status cannot be told from the worker's.

### Work commands

A wrapper starts the command it runs for work in a scope of its own in
`shoal-work.slice`, through `systemd-run --scope`, wherever the wrapper runs.
An agent in a terminal host's place thus keeps that terminal but leaves the
host's control group for one Shoal controls. The slice holds Shoal's work, and
only that, for the [memory limit](protection.md#memory-limit) and the
kernel's accounting.

`systemd-run --scope` runs the command in its own place. The command's PID,
process group, terminal and environment stay as a direct start leaves them.

The wrapper looks for the launcher once. Where no scope starts, it starts
commands directly. It names each scope for the space whose work it runs, so
what a command leaves there stays tied to the space after the command ends.
[Work slice](../reference/load.md#work-slice) lists where scopes cannot start.

### OOM kills

The service and every scope Shoal starts set `OOMPolicy=continue`. An OOM kill
then ends only the process the kernel chose, and only the daemon's own death
fails the service. A CI job's scope is the exception: the kernel kills all of
it with any one of its processes, as [OOM scores](protection.md#oom-scores)
explains.

## Process identity

Every process Shoal starts names its space in `SHOAL_SPACE`, whatever the
space's kind. That covers executions, hooks, exported environments and the
steps of CI jobs. Only a space with a worktree gives the worktree's variables,
so a chat in a workspace's worktree carries none of the workspace's.

Hooks run as untracked user processes. They carry the space's identity but no
scope token. A hook exists to start or stop what outlives it, such as a tmux
session, and what it starts must not become a survivor of an execution.

Nothing outlives the workspace. Removal stops every verified process that still
carries its identity, including what hooks and exported environments started,
and its leftovers.

## Leftovers

A leftover is a detached process that its space's worktree, temporary
directory, program, identity or, on Linux, work scope ties to the space.
[What belongs to a space](../reference/daemon.md#what-belongs-to-a-space)
defines it. The ties go beyond the identity because some processes cannot show
it: a platform binary hides its environment, and a program built in the
worktree may run elsewhere.

Live ancestry, not environment variables, decides what is never untracked: the
processes of tracked executions, the processes that started them, and terminal
session servers. `shoal stop` stops a space's untracked processes through the
same operation once its executions have stopped. `--processes` and
`--leftovers` stop them alone, so an agent stops what its commands left behind
without stopping itself.

### Reported, not stopped

Shoal reports leftovers instead of stopping them on sight. The agent knows
whether its own test servers should keep running.

The cleanup sweep finds every space's leftovers from one process listing and
keeps them in memory, so no tool call waits for a scan. The agent's hook names
each new leftover once. Leftovers raise no alert, and `shoal status` lists them
for the user.

The same holds once an agent exits, wherever it ran. What its scope keeps
stays the space's leftovers, since a restored agent may want its servers back.
They go with the space.

### Cleanup and CI jobs

Automatic cleanup counts a leftover that works in the worktree as use of the
workspace. It stops a program built there that runs elsewhere.

Shoal stops a CI job's leftovers as the job ends and names them in its
workspace's log. No agent is left to decide, and the workspace goes.

## Configured commands

A `[commands]` entry is an argument array. Shoal resolves it by name from the
configuration layers at launch. It runs through the tracked wrapper, with the
workspace's scope, terminal I/O and the caller's literal extra arguments.
[Configured commands](../reference/run.md) says what `shoal run` does with
them.

`shoal run <name>` is a configured command's only name, so it never competes
with a built-in command. Shoal has no `shoal <name>` shorthand. An unknown
top-level command prints clap's error and a line that points to
`shoal run <name>`, without reading configuration.

`shoal edit` runs through the same wrapper. The editor thus gets the
workspace's scope and port variables, and counts as activity.
