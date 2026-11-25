# Setup and hooks

This file covers workspace setup and the lifecycle hooks repository config
names.

Repository config may name setup and lifecycle hook executables:
single executable paths run directly without shell parsing or PATH lookup.
Hooks that run with a worktree resolve paths against it and use it as their
working directory. Setup runs through
the tracked wrapper with workspace scope; the daemon owns readiness and execution
records, and `start` keeps the workspace preparing until setup exits cleanly with no
survivors; manual removal also takes a preparing workspace with no execution, whose
setup an interrupted command never started. Failures preserve files, branches, and leases; process-verification
failures retain the workspace and report the command's exit status and the
blocking evidence. Other failures let interactive callers choose retry, delete,
ignore (which repairs verified state first), or keep; a setup that never
started offers only retry or keep. JSON callers get a nonzero exit.
`setup` reruns setup explicitly, and `resume` refuses a workspace whose setup
failed, naming it, since a setup is not an agent; nothing retries
automatically.
The agent a start launches in a workspace whose setup fails waits in the
queue for that setup instead of holding its slot, queued with the workspace's
record for a start the daemon carries out, or by the preparation, which
carries the agent a CLI would run itself, so no follower is needed; one that
starts now starts once the setup releases it, regardless of the limits. A start whose preparation ended without queuing it, such
as when its worker died, goes back to the queue as a new entry, which no hold
on the old one keeps back, with its plan opening the workspace. A persisted
dependency that only a preparation getting past its post-setup hook releases
holds it, so a retried setup starts it without the user restating its mode,
task or permissions, and a failed hook keeps it waiting.
While the daemon registers an execution it sends the wrapper keepalives, and
the wrapper's limit bounds the daemon's silence rather than the whole wait, so
setups queued behind each other's Git gate never fail for being slow; a
registration whose wrapper left is forgotten rather than recorded as a failed
setup.
Tracked setup starts by copying the repository checkout's ignored files unless
`setup.copy_ignored = false`, so local files like `.env` arrive without a setup
script and failures gate readiness; `.worktreeinclude`, a convention other worktree
tools share, selects what to copy, and `setup.copy_ignored_exclude` leaves entries
out. The copy is Shoal's own, ported from Worktrunk's `wt step copy-ignored`.

They use global defaults below repository config. The pre-setup hook runs in the
daemon after ownership and execution checks, with a time limit, while
preparation excludes lifecycle and permit changes; failure marks setup failed.
It may be configured without a setup command. The post-setup hook runs in
setup's operation, without a terminal, once the workspace is ready, after its PR
is linked and before any agent; failure keeps the ready workspace. The
pre-remove hook runs in the daemon inside the single removal path for manual,
repository, and automatic removal, after checks pass and commands stop, bounded
in time; failure retains the workspace. Ownership and removal policy are
rechecked after all removal hooks, with branch deletion derived from the final
inspection. A manual removal answers its request once the workspace is reserved
and passes the checks removal makes before changing anything, and goes on in the
background, under the gate a daemon update waits for, unless the caller waits
for it; a later failure retains the workspace and reaches the requesting agent,
or else the user as an alert. Chat removal stays synchronous.

Post-remove runs after ownership is released, from the repository checkout with
its copy of the executable, retaining the old workspace path in the environment.
Its command is selected before removal. It is best-effort: failure is a warning
and alert, never a failed removal or restored ownership. Cleanup of deleted
worktrees tolerates missing ancestors and skips hooks; post-remove events are
not durably queued or replayed.
