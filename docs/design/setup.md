# Setup and hooks

This file covers workspace setup and the lifecycle hooks that repository config
names. [Setup and hooks](../reference/setup.md) says what each step and hook
does, and the
[configuration reference](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#hooks)
describes each option.

## Executables

Setup and each hook are one executable path. Shoal runs it without shell
parsing or `PATH` lookup, so arguments and shell logic stay in the script. A
hook that runs while the worktree exists uses it as its working directory.

Hooks run untracked, as the user's own processes, as
[Process identity](execution.md#process-identity) says.

## Setup

### Readiness

Setup runs through the tracked execution wrapper, with workspace scope. The
daemon owns the readiness and execution records. A workspace stays `preparing`
until setup exits 0 and leaves no process running. What should outlive setup
belongs in a hook or a demo, as
[Proof that processes ended](daemon.md#proof-that-processes-ended) says.

### The copy of ignored files

Setup starts by copying the repository checkout's ignored files. Local files,
such as `.env`, thus arrive without a setup script. The copy is a step of
setup, so its failure keeps the workspace from ready.

A `.worktreeinclude` selects what the copy takes. Other worktree tools read the
same file, so one file serves them all. Shoal copies with its own code and
needs no such tool.

### Failed setup

A failed setup keeps the workspace with its files, branch and leases, so the
user can see what went wrong. A setup that left a process running fails too,
with the command's exit status and the processes that blocked it.

Nothing retries a setup on its own. `shoal setup` reruns it. `shoal resume`
refuses the workspace and names `shoal setup`, since a setup is no agent to
resume.

### The agent that waits for setup

The agent that a start launches in a new workspace waits in the agent queue
for setup. It holds no slot meanwhile, since a setup can take minutes.

The daemon queues the agent with the workspace's record. For an agent that a
CLI runs itself, the preparation carries it instead. No process has to stay to
start the agent once setup ends.

A start whose preparation ended without queuing its agent, such as when its
worker died, goes back to the queue as a new entry. No hold on the old entry
keeps it back, and it opens the workspace when it starts.

The queued agent depends on a persisted record that only a preparation past
the post-setup hook releases. A retried setup thus starts the agent without
the user restating its mode, task or permissions. A failed post-setup hook
keeps it waiting.

### Registration

While the daemon registers setup's execution, it sends the wrapper keepalives.
The wrapper's time limit bounds the daemon's silence, not the whole wait. So
setups queued behind each other's Git gate never fail for being slow.

A registration whose wrapper left is forgotten, not recorded as a failed
setup, as [Executions that never started](agent-lifecycle.md#executions-that-never-started)
says.

## Setup hooks

### The pre-setup hook

`hooks.pre_setup` runs in the daemon, after the ownership and execution checks.
It runs with a time limit. Preparation refuses lifecycle and permit changes
while it runs. Its failure marks setup failed. It runs without a
`setup.command` too.

### The post-setup hook

`hooks.post_setup` runs in setup's operation, without a terminal. It runs once
the workspace is ready, after its PR is linked and before any agent starts. It
thus sees the PR and can change how the agent launches. Its failure keeps the
ready workspace and starts no agent.

## Removal hooks

### The pre-remove hook

`hooks.pre_remove` runs in the daemon on
[the removal path](cleanup.md#the-removal-path) that manual, repository and
automatic removal share. It runs after the checks pass and the commands stop,
with a time limit. Its failure keeps the workspace, so a teardown that must
finish belongs in this hook.

### The post-remove hook

`hooks.post_remove` runs after removal has released the workspace's ownership.
It runs from the repository checkout, with the checkout's copy of the
executable, since the worktree is gone. Its environment still names the old
worktree path. Shoal selects its command and environment before removal, while
the workspace's record and config still exist.

The hook is best-effort, since nothing can undo the deletion. Its failure is a
warning and an alert, never a failed removal or restored ownership. Shoal does
not queue or replay it across a daemon restart.

A worktree the user already deleted runs neither removal hook.
