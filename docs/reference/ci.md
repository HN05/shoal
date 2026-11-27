# CI

This file says what the `ci` commands do and how this machine runs a
repository's CI jobs. [Links and watches](../design/links.md) and
[runner and triggers](../design/runner.md) give the reasons.

```sh
shoal ci run deps.yml                    # start a workflow run on the workspace's branch
shoal ci run deps.yml --branch main --input level=full
shoal ci run https://github.com/team/tool/actions/workflows/ci.yml   # a workflow by its link
shoal ci rerun                           # rerun every failed job on the linked PR's head
shoal ci cancel --pr 12 --check rust     # cancel one running or waiting check of PR 12
shoal ci rerun 2765 --failed             # rerun a run's failed jobs
shoal ci cancel https://forge.example/team/repo/actions/runs/2765
shoal view ci 2765                       # a run's state, its jobs and failed jobs' logs
shoal view ci 2765 --job rust            # the whole log of the run's job rust, in any state
shoal view ci https://github.com/team/tool/actions/runs/12   # a run of another repository
shoal ci ls --workflow deps.yml          # the newest runs on the workspace's branch
shoal ci ls --pr 12                      # the runs on PR 12's head commit
shoal ci approve --pr 12                 # approve the runs waiting on PR 12's head
shoal ci rerun --pr 12 --repo shoal      # in a registered repository, without a workspace
shoal ci ls --space 5                    # a chat's linked PR's runs
```

## Where the commands act

A `ci` command acts from the space `--space` names. Without it, it acts from
the current workspace, then the [chat](chats.md) it runs in, then the current
registered checkout, then a space a picker offers.

Outside a space, the commands act as
[issue and PR actions](issues.md#where-commands-act) do: in the registered
checkout they run in, or in the repository `--repo` names. There, `ci run` and
`ci ls` take the repository's default branch. `rerun`, `cancel` and `approve`
record no event and need a run or `--pr`.

A chat acts in its repository's checkout with its own links:

- `ci run` starts on `--branch` or the default branch, and links the run.
- `ci ls` lists the runs on its one linked PR's head, or else on the default
  branch.
- `rerun`, `cancel` and `approve` act on its linked PR without `--pr`.

### What an agent acts on

A scoped process, a workspace's or a chat's agent, acts only in the
repository it runs in. It reads any run there. It starts runs only on the default branch and
on the branches of the workspaces its scope reaches. It reruns and cancels
only runs on those branches, and runs of a PR whose head is one of them, on
any commit of the PR.

### Another repository

A run's URL, or `--repo`, may name another repository, registered or not.
Such a command needs no workspace or checkout. Anyone reads that repository's
runs with `view ci` and `ci ls`. `ci ls` there lists the default branch's runs
without `--branch`.

`ci run`, `ci rerun`, `ci cancel`, `ci approve` and [`link ci`](links.md#ci-runs) act
there with your own logins, as merging does.

## Start a run

`shoal ci run` starts a workflow run on the workspace's branch, or the one
`--branch` names. Each repeated `--input NAME=VALUE` passes an input. The
daemon starts it as the [agent account](agents.md#agent-accounts), or with
your own logins in another repository.

`ci run` names the workflow by its file name or by a web URL:

- its page: GitHub's `…/actions/workflows/<file>` or Forgejo's
  `…/actions?workflow=<file>`;
- its file on a branch: `…/blob/<branch>/.github/workflows/<file>` or
  `…/src/branch/<branch>/.forgejo/workflows/<file>`.

A file's URL runs on its branch unless `--branch` names another. A URL of
another repository starts the run there, on its default branch unless a branch
is named.

Each forge starts a run its own way:

- GitHub, Forgejo and Gitea dispatch the named workflow file, or a GitHub
  workflow ID. The workflow needs a `workflow_dispatch` trigger.
- GitLab runs the branch's pipeline. It takes no workflow and passes the
  inputs to the pipeline's `spec:inputs`.
- Bitbucket runs the named custom pipeline, or the branch's own without one.
  It passes the inputs as variables.

A started run is a new run, not a rerun of a PR's checks. On GitHub and
GitLab, a run on the PR's head commit reports checks there as any run does.
Forgejo attaches a dispatched run to no commit. A failed check there stays
failed for `shoal view` and PR cleanup until `ci rerun` reruns it, on Forgejo
17 or later.

Inside a workspace or chat, Shoal [links](links.md#ci-runs) the run as opened, so
`shoal watch` reports when it finishes. It links only a run whose URL the forge
reports, on a forge where Shoal reads runs, which Bitbucket is not.

`--json` returns `workflow`, `branch`, the run's `url` where the forge reports
it, which GitHub and Gitea do not, and `linked`, which says whether Shoal
linked the run.

## Rerun or cancel

`shoal ci rerun` and `shoal ci cancel` act on one run, or on the checks of a
PR's head.

The forges differ in what they support:

- Forgejo 17 and later, GitHub and GitLab rerun.
- Forgejo before 17, Gitea and Bitbucket cannot rerun.
- Every forge but Bitbucket cancels.

`shoal forge capabilities` says what the repository's forge supports.

### One run

Given a run's web URL, as `ci run` or a check links it, or the number in that
URL, the commands act on that run.

`rerun` reruns every job of a finished run. With `--failed` it reruns only the
failed jobs:

- GitLab retries only the failed and cancelled jobs, with or without
  `--failed`.
- Forgejo, with `--failed`, reruns each failed job and the jobs that need it,
  also while the run still runs.

`cancel` cancels a running run. `--json` returns the run's `url` and the
`action`.

### A PR's checks

Without a run, the commands act on the current head of the linked PR, or of
the PR `--pr` names. `rerun` reruns its failed CI jobs, and `cancel` cancels
its running or waiting ones. Each repeated `--check` instead selects one check
by the name `shoal view` shows, which must be a CI job in that state. Checks of
other providers stay as they are.

The daemon acts through the forge's job API as the agent account. It records
an `item_changed` event once a request succeeds. A failed request stops the
rest.

Each forge reruns and cancels its own way:

- GitHub reruns a failed Actions job, or every failed job of a run when
  several of its jobs are selected, since a rerun starts a new attempt of the
  run. It refuses a rerun while another job of the run still runs. It cancels
  a job by cancelling its run.
- GitLab retries or cancels each job.
- Forgejo 17 and later rerun a failed job with the jobs that need it. When
  several of a run's jobs are selected, they rerun the run, which waits until
  none of its jobs runs. Forgejo cancels each check's run.

A check whose run ran on an older commit than the head is cancelled with it,
but never rerun: only a new run on the head checks the head.

`--json` returns `url`, `action` and the `checks` acted on.

## View a run

`shoal view ci` shows a run's state, branch, workflow and jobs. A number after
`ci` names a run; a number alone never does.

A run's state is one of these:

- `waiting` until a runner takes a job;
- `running`;
- `failed`;
- `finished`;
- `cancelled` when the only jobs that stopped short were cancelled.

Once the run has finished, the view adds its result as the forge reports a
check's, such as `success`, `failure`, `cancelled` or `skipped`. Each job has
one of the same states, or `cancelled` when it was cancelled, and its result
once it has finished.

The view shows the log tails of the failed jobs as
[`shoal view`](links.md#failed-job-logs) shows a PR's. `--job` shows one job's whole log
instead, in any state, up to 8 MiB. It names the job by name, or by the ID the
view lists beside it, since several jobs may share a name.

`--json` returns the run's `url`, `state`, `result`, `branch` and `workflow`,
its `jobs` with their `id`, `name`, `state`, `result` and `url`, the `logs` it
read and the `errors` of those it could not.

## List runs

`shoal ci ls` lists the newest runs: ten, or up to 100 with `--limit`. It lists
the runs on the workspace's branch, on the branch `--branch` names, or on the
head commit of the PR `--pr` names. `--workflow` keeps one workflow file's
runs, except on GitLab, which does not take it. For each failed run it reads
the jobs, to give its state and result as `view ci` does.

`--json` returns each run's `url`, `state`, `result`, `branch` and `workflow`.

`view ci` and `ci ls` read any run of the repository as the agent account, on
every forge but Bitbucket.

## Runs that wait for approval

Forges hold the runs of PRs from forks and first-time contributors until a
maintainer approves them. Until then those runs report no checks.

`shoal view` and `shoal status` list an open PR's waiting runs in
`waiting_runs`, each with its `url` and `workflow`. The text names how to
approve them. GitHub reports the runs it holds. Forgejo and Gitea mark them
among the head commit's runs. GitLab and Bitbucket hold none.

`shoal ci approve` approves one run, which must be waiting. With `--pr`, or
with neither for the linked PR, it approves every waiting run on the PR's
head, and fails when none waits. It acts with your login. A scoped process
acts as the agent account, as its space's
[`ci_approve` permission](modes.md#permitted-actions) allows. A proposed
approval names the runs it approves, so a run that starts waiting later, as
after a push, asks again. The daemon records an `item_changed` event.

Only GitHub approves through its API. Forgejo and Gitea approve in the web UI,
so there the command fails before any request.

`--json` returns the `approved` runs' URLs.

## Runner

The daemon runs a registered repository's CI jobs on this machine while
[`runner.enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#runner)
is on for it.

### Turn the runner on

The runner acts with your own login, the `user_token` of the repository's
[forge host](issues.md#logins). That login must be allowed to administer the
repository's Actions.

On Forgejo, the daemon registers a runner for the repository and keeps the
runner's UUID and token in its state. Turning the setting off, or removing the
repository, deletes the runner on the forge. When the forge cannot be reached,
removal goes ahead, and a `runner_failed` alert names the runner to delete by
hand.

On GitHub, the daemon registers a single-use runner for each job instead.
Turning the setting on only checks that your login may.

The runner needs one of these:

- Forgejo 15 or later, with `forgejo-runner` on the daemon's `PATH`;
- GitHub, with the `bin` directory of an
  [`actions/runner`](https://github.com/actions/runner) installation on the
  daemon's `PATH`.

Otherwise, or when the forge refuses the registration, a `runner_failed` alert
says why and the runner takes no job. The daemon tries again five minutes
later, or when the config changes. Gitea and other forges report the
`run_jobs` capability as unsupported.

### Jobs in the queue

The jobs the forge holds for the runner's labels join the
[agent queue](queue.md) as `job` entries named `ci-<job>-<ID>`. A job ranks
with the spaces its run links, as [queue order](queue.md#queue-order) says.

The daemon reads the waiting jobs every ten seconds while a job could start,
and every minute while none can. New jobs then show in the queue. The entry
of a job the forge no longer lists leaves it, as when another runner took the
job or a cancel ended it.

Until the queue admits a job, any other runner may take it. `shoal queue
start` admits a job regardless of the limits. `shoal queue cancel` passes it
over and leaves it to other runners.

The queue also passes over a PR's job once the PR has a newer head: at the
next read of the waiting jobs, whatever holds it back, or when the queue
admits it. Shoal cancels its run on the forge as you, unless the run has
finished, so the run no longer holds the PR's checks. A cancel that fails
sends a `runner_failed` alert.

### Job admission

[`machine.max_jobs`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine)
limits the jobs that run at once. Under `dynamic`, jobs start through the
admission that starts [agents](queue.md). The spacing between starts counts a
job's start and an agent's alike.

A job is expected to use the 95th percentile of the resident memory its latest
30 whole runs reached, by repository, workflow and job name:

- A run that a stop cut short is not whole, such as a stop by load
  protection, `shoal stop`, a sleep or a shutdown. While a job has no whole
  run, the highest peak of its runs counts instead.
- A run whose process the kernel's OOM killer killed is whole, and counts at
  1.2 times its peak.
- A runner that ends without taking its job records no run: one GitHub
  registered whose hook reported no job, or a `forgejo-runner` whose job's log
  names no runner of this machine.
- Before a job has run, it counts at what a starting agent counts at.

Until its runner ends, a job reserves what it is expected to use beyond what
its workspace used at the latest usage sample. Before the first sample it
reserves all of it.

Shoal reserves no CPU for a job. Instead, a job beside running ones waits
while the machine's CPU has been busy at or above `[machine.overload.warning]
cpu_used_percent`, on average over its `cpu_sustained_seconds`, niced time
included. It also waits while the CPU has not been read that long. The first
job starts whatever the CPU reads.

A job also waits for a permit of the pool
[`runner.permit`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#runner)
names.

On a machine joined to a [hub](hub.md#placement), a job waits 15 seconds for
each machine its hub's placement ranks ahead of this one, since that machine
may claim it first. The queue shows it held by `N machines rank ahead`. The
best-ranked machine claims at once, an `intermittent` one with room before an
`always_on` one. A job waits for no machine when the hub cannot be reached,
under the `local` placement policy, and after a daemon restart.

### Run a job

An admitted job runs the forge's runner at nice 10, as a tracked execution. It
runs in a disposable workspace at the job's commit, fetched from its PR's or
branch's ref, or else by commit. The workspace's directory takes its queue
entry's name. It skips setup and starts from the
[seed build](setup.md#seed-builds) like any workspace, so the job rebuilds
only what its commit changed.

On Forgejo, the runner is `forgejo-runner one-job --handle <handle>`, with each
label on the host backend.

On GitHub, the runner is a just-in-time runner for the labels. It runs from a
copy of the installation in the job's directory under the state directory,
cloned where the filesystem clones files and copied otherwise. Keep the
installation updated, or each copy updates itself before its job.

GitHub gives that runner any job its labels match. A job of another run than
the admitted job's waits before its first step, until the daemon moves the
workspace to that run's commit. Shoal then lists, counts and cancels it as the
job the runner took. A workspace that cannot move fails the job.

A job's steps get its workspace's environment, its scope token, `TMPDIR`,
[`[env]`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#environment)
and the variables its [presets](setup.md#build-tool-presets) give jobs. They
get no agent secrets and no forge login besides the token the forge gives the
job.

### Write a job for the runner

`forgejo-runner`'s host backend starts each step in a directory of its own,
not in the job's workspace. A job for this runner therefore names the
workspace as its working directory, and has no checkout step, since the
workspace has the commit already.

Shoal gives every job's steps, and its `env` context, `SHOAL_WORKSPACE_PATH`
and `SHOAL_SPACE`. With them, one job serves this runner and other runners.
On GitHub, a job-started hook gives the steps these variables after the job's
defaults are read. The job's default working directory there is the workspace,
so the same job works:

```yaml
jobs:
  rust:
    runs-on: shoal
    defaults:
      run:
        working-directory: ${{ env.SHOAL_WORKSPACE_PATH }}
    steps:
      - uses: actions/checkout@v4
        if: ${{ !env.SHOAL_SPACE }}
      - run: cargo test --locked
```

### Where a job shows

`shoal list` shows a running job's workspace below the first workspace that
links its run's PR, or that owns the branch the run ran on. The row shows the
job and what its processes use.

`shoal status` lists the job under `CI jobs` for every space that links the
run's PR, and for the workspace that owns its branch. It lists each job still
waiting to start under `Waiting jobs`, with what holds it back, as
`shoal queue` shows it.

The job's workspace is a child of none of them, and no agent's scope there
reaches it.

### How a job ends

Once the job's execution ends, however it ended, Shoal stops what its steps
left running in the workspace. A `job_ended` event in the workspace's
[log](log.md) names them. The workspace then goes, whatever it holds, with its
permit. A removal that fails is tried again until it succeeds.

The runner can also outlive its job:

- When a runner is still running a minute after the forge reports the job's
  run finished, cancelled or timed out, Shoal ends it. This happens when a
  process a step left behind holds the runner's output open. Shoal stops every
  process of the job's workspace, including leftovers in other process groups
  or whose parent exited, then the runner, and records a `job_ended` event.
- Once a GitHub runner has waited a minute without a job, while no job waits
  for its labels, Shoal deletes it.
- When the forge no longer lists a `forgejo-runner`'s job as waiting, Shoal
  stops the runner at the read that sees it, if the job's log names another
  runner as the one that took it, or the job ended before any runner took it.

The workspace of such a runner goes as well.

A job that cannot start is passed over with a `runner_failed` alert, and its
workspace goes. So is a job whose checkout failed, or that a restart cut
short. After a daemon restart, a job whose runner is gone goes the same way,
and the forge times it out.

The daemon's log records each stop of a job's runner, with the job, its run,
what stopped it and whether the run or the job was cancelled.

### Stop a job

[Load protection](load.md#stopped-ci-jobs) may stop a running job. Disk protection never
does, and the workflow's `timeout-minutes` bounds the job.

`shoal stop` on the job's workspace, and `shoal ci cancel`, cancel the job's
run on the forge, which cancels each of its jobs. The forge's runner then stops
the job, and its workspace goes. `shoal stop` says which run it asked the
forge to cancel, in `cancelled_run` with `--json`.

`shoal rm` of a running job's workspace and `shoal daemon stop` cancel the run
before they stop the runner. A signal to the daemon, a
[restart](daemon.md#exits-that-keep-work-running) and the service manager's
stop of the service after the daemon exited leave the job running for the next
daemon.

### Cut off by sleep or shutdown

These cut a job off on every machine:

- the machine going to sleep;
- the stop before a system shutdown;
- a hangup or terminate signal to the job's wrapper, as a logout or a shutdown
  sends.

Shoal ends such a job as [load protection](load.md#stopped-ci-jobs) does: it
cancels that job alone, then reruns it on its forge as soon as its run has finished, so any
runner can take it. A `jobs_rerun` alert names each such job once it reruns.

On a machine joined to a [hub](hub.md), a sleep or shutdown hands the jobs it
cut off to the hub. The hub reruns them while the machine is away, through its
own machine or another connected one with your login for the run's forge. The
machine never reruns them itself once it is back.

On a signal to the job's wrapper, the daemon cancels the job before the
wrapper stops the runner. The runner may report its job first. When the daemon
does not answer within 15 seconds, the wrapper stops the runner without the
cancel and says why in the job's log.

A job on a machine that sleeps or drops without announcing it runs until the
forge times it out.
