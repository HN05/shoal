# Runner and triggers

This file covers `shoal-runner`: the CI jobs this machine runs and the triggers
that start agents.

A trigger is config like every other setting, without commands that create or
change it, except that a contribution's tracked file defines none, since its
repository's maintainers are not the user who runs it; each source is a module
behind one interface whose firings carry a JSON input document, which filters,
templates and keys read without knowing the source, and every firing launches
through the operation `shoal start` uses, as the user, so the queue,
`max_heavy_agents` and cleanup apply.

Build acceleration and the CI runner belong in core: Shoal runs the CI jobs of
the repositories registered with it, so a job starts from the seed and rebuilds
only what its commit changed, and the machine's agents and jobs share one
admission instead of competing for memory. The forge's own runner runs the
steps on the host, `forgejo-runner one-job --handle` or a GitHub just-in-time
`actions/runner`, which keeps the workflow semantics, actions and log
streaming, while Shoal decides which job runs when: it lists the jobs waiting
for its labels as agent queue entries, admits them with agents, and claims a
job only once admitted, so until then any other runner may take it. GitHub
has no handle and no label that keeps a runner to one job, so its runner may
take another job its labels match, and it writes its credentials and work
into its installation, so each job runs a copy of it; a job-started hook holds a job of another
run until its workspace is at that run's commit, so a check never reports on
another commit. Polling finds jobs, which works without the webhook listener
and the tunnel it needs; it runs every ten seconds while a job could start and every minute
while none can, so the queue still shows new jobs and drops those other
runners took, and a claimed job's runner that another runner beat to the job,
or that a cancel left nothing, stops instead of holding its slot and the
memory its job is expected to use, and conditional reads keep unchanged answers off the rate
limit. Each job runs as a tracked execution in
a disposable workspace at its commit, which goes once the forge's runner
exits, at low CPU priority so that a build takes only the cores agents and the
user leave idle. A job outlives the daemon, which only decides when it starts:
a crash, a failed start, a restart and a signal, which the daemon cannot tell
from a service stop, leave it running for the next daemon to take its wrapper
back, and its wrapper runs apart from the daemon's service, as every detached
worker does, so the service manager's stop of the service leaves it running
too. A job ends only on the forge's word, so stopping it, as `shoal daemon
stop` does, cancels its run there, and disk protection leaves it running.
Load protection, and what cuts a job off on any machine, the machine's
announced sleep, the stop before a system shutdown and a logout's or a
shutdown's signal to its wrapper, which asks the daemon since the daemon
keeps the forge's login, end a job without
cancelling its run, which would end the run's other jobs too: before it
signals anything, Shoal reports that job alone cancelled through the forge's
runner protocol, as the runner it registered there, then stops the
job's processes and its runner, whose report of the stopped step changes
nothing, since the forge keeps a finished job's result. The job reruns once the
run has finished, so the PR's check shows the job cancelled until then; a sleep
or shutdown hands the jobs it cut off to the hub the machine joined, which
reruns each once its run has finished, through its own machine or else the
first connected one with the user's login for the run's forge, and the machine
forgets those the hub keeps, so it never reruns one again once it is back. A
machine in no hub, the hub's own and one that cannot reach the hub keep them and
also try at once, as the run whose last running job it ended has finished, and
the user hears of each job either cut off once it reruns. A
machine that drops without announcing it leaves its job to the forge's
timeout. On a
forge whose runner protocol Shoal does not speak, GitHub's, the forge fails
the job once the runner reports the stopped step. Once the forge has finished the run,
a runner that outlives it by a grace period, held open by a process a step
left, is Shoal's to end with every process of the job's workspace. Every space that links a run's PR, and the workspace that owns its branch, lists the job in its status, and a
waiting one with what holds it back, but
none of them owns it, lends it a build nor reaches it with its scope. The runner is
registered per repository with the user's own login, never for a user,
organisation or instance, or on GitHub one runner per job, and what the forge
does its own way is a `Runners` operation, so another forge adds an
implementation, not daemon code. Container jobs and forges other than Forgejo
and GitHub remain proposals.

## The crate's boundary

The `Runner` service fires the triggers from its timer loop and webhook
listener. It launches their agents through the crate's interface, as
`shoal start` does. It registers each repository's runner, and queues, claims
and ends its jobs. The jobs' workspaces, permits and executions stay with the
daemon, which the interface reaches.
