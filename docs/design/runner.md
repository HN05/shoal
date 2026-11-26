# Runner and triggers

This file covers `shoal-runner`: the triggers that start agents and the CI jobs
this machine runs. [Triggers](../reference/triggers.md) and
[Runner](../reference/ci.md#runner) say what each does; this file gives the
reasons.

## Triggers

### A trigger is config

A trigger is config like every other setting. No command creates or changes
one; commands change only its runtime state, such as whether it is paused.

A contribution's tracked config defines no trigger. Its repository's
maintainers are not the user who runs it, and a trigger starts agents as that
user.

### One interface for every source

Each source, such as a timer or a webhook route, is a module behind one
interface. A firing carries a JSON input document. Filters, templates and keys
read that document without knowing the source. So a new source adds a module,
and every trigger option works with it unchanged.

### A firing launches as `shoal start` does

Every firing launches through the operation `shoal start` uses, as the user.
The [agent queue](queue.md), `max_heavy_agents` and cleanup thus apply to a
trigger's agent as to any other.

## Running CI jobs

### Shoal runs the jobs

Shoal runs the CI jobs of the repositories registered with it. A job then
starts from the [seed build](repositories.md) and rebuilds only what its commit
changed. The agent queue admits jobs with agents, as
[Each machine schedules its own work](overview.md#each-machine-schedules-its-own-work)
says, and [Agent queue](queue.md) says what a job is expected to use.

### The forge's runner runs the steps

The forge's own runner runs a job's steps on the host:
`forgejo-runner one-job --handle` on Forgejo, a just-in-time `actions/runner`
on GitHub. It keeps the workflow semantics, actions and log streaming.

Shoal decides only which job runs when. It lists the jobs waiting for its
labels as agent queue entries and claims a job only once the queue admits it.
Until then any other runner may take the job.

### One runner per repository

Shoal registers its runner per repository, with the user's own login. It never
registers one for a user, an organisation or an instance, so the runner takes
only the jobs of repositories registered with Shoal.

What one forge does its own way is a `Runners` operation in `shoal-forge`.
Another forge adds an implementation, not daemon code.

### GitHub's runner takes any job

GitHub has no handle and no label that keeps a runner to one job. Shoal
therefore registers one runner per job, and that runner may take another job
its labels match. A job-started hook holds a job of another run until its
workspace is at that run's commit. A check thus never reports on another
commit.

GitHub's runner writes its credentials and work into its installation. So each
job runs a copy of the installation.

### Polling finds jobs

Shoal finds waiting jobs by polling. Polling works without the webhook listener
and the tunnel it needs.

Shoal polls often while a job could start and seldom while none can. So the
queue shows new jobs and drops those other runners took. A claimed runner that
another runner beat to its job, or whose job a cancel ended, stops at the next
poll. It then holds neither its slot nor the memory its job is expected to use.
Conditional reads keep unchanged answers off the forge's rate limit.

### A job runs in a disposable workspace

Each job runs as a tracked execution in a disposable workspace at its commit.
The workspace goes once the forge's runner exits. The job runs at low CPU
priority, so a build takes only the cores agents and the user leave idle.

The spaces that link a run's PR, and the workspace that owns its branch, list
the job in their status. None of them owns the job, lends it a build or reaches
it with its scope.

## How a job ends

### Only the forge ends a job

A job outlives the daemon, which decides only when the job starts. A crash, a
failed start, a restart and a signal leave the job running for the next daemon,
which takes its wrapper back. The daemon cannot tell a signal from a service
stop. The wrapper runs apart from the daemon's service, as
[every detached worker does](execution.md#processes-the-daemon-starts), so a
service stop leaves it running too.

A job ends only on the forge's word. Stopping a job, as `shoal daemon stop`
does, therefore cancels its run on the forge. Disk protection leaves jobs
running.

### Cut-off jobs rerun

Some stops cut a job off on any machine: load protection, the machine's
announced sleep, the stop before a system shutdown, and a logout's or
shutdown's signal to the job's wrapper. These end the job without cancelling
its run, since that would end the run's other jobs too.

Before it signals anything, Shoal reports that job alone cancelled through the
forge's runner protocol, as the runner it registered there. Only the daemon
keeps the forge's login, so a signalled wrapper asks the daemon to do it.
Shoal then stops the job's processes and its runner. The runner's report of the
stopped step changes nothing, since the forge keeps a finished job's result.
On GitHub, whose runner protocol Shoal does not speak, the forge fails the job
once the runner reports the stopped step.

The job reruns once its run has finished. Until then the PR's check shows the
job stopped. The user hears of each cut-off job once it reruns.
[Stopped CI jobs](../reference/load.md#stopped-ci-jobs) says how protection's
stops rerun.

A machine that drops without announcing it leaves its job to the forge's
timeout.

### The hub reruns what a sleep or shutdown cut off

A sleeping or shut-down machine cannot rerun its jobs once their runs finish.
So it hands the jobs it cut off to the [hub](hub.md) it joined. The hub reruns
each once its run has finished, through the hub's own machine, or else the
first connected one with the user's login for the run's forge. The machine
forgets the jobs the hub keeps, so it never reruns one again once it is back.

A machine in no hub, the hub's own machine and one that cannot reach the hub
keep their jobs. Each also reruns at once every job whose run has already
finished.

### A runner that outlives its run

A process a step left can hold the forge's runner open after the forge has
finished the run. Once a grace period passes, Shoal ends such a runner with
every process of the job's workspace.

## Proposals

Container jobs, and forges other than Forgejo and GitHub, remain proposals.

## The crate's boundary

The `Runner` service fires the triggers from its timer loop and webhook
listener. It launches their agents through the crate's interface, as
`shoal start` does. It registers each repository's runner, and queues, claims
and ends its jobs. The jobs' workspaces, permits and executions stay with the
daemon, which the interface reaches.
