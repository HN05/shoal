# CI

This file says what the `ci` commands do and how a machine runs CI jobs.

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
shoal ci ls --workflow deps.yml        # the newest runs on the workspace's branch
shoal ci ls --pr 12                    # the runs on PR 12's head commit
shoal ci approve --pr 12                 # approve the runs waiting on PR 12's head
shoal ci rerun --pr 12 --repo shoal      # in a registered repository, without a workspace
shoal ci ls --space 5                  # a chat's linked PR's runs
```

`shoal ci run` starts a workflow run on the workspace's branch, or the one
`--branch` names, with each repeated `--input NAME=VALUE`. Every `ci` command
also runs outside a workspace as [issue and PR actions](issues.md#issue-and-pr-actions)
do, from a registered checkout or with `--repo`, where `ci run` and `ci ls`
take the repository's default branch. `--space` names the workspace or running
[chat](chats.md) to act from, which defaults to the current workspace, then the
chat this runs in, then the current checkout, then a picker. A chat acts in its repository's
checkout like a registered checkout, but with its links: `ci run` starts on
`--branch` or the default branch and links the run it starts, `ci ls` lists
the runs on its one linked PR's head or else on the default branch, and
`rerun`, `cancel` and `approve` act on its linked PR without `--pr`. It also takes a
workflow's web URL: its page, GitHub's `…/actions/workflows/<file>` or
Forgejo's `…/actions?workflow=<file>`, or the URL of its file on a branch,
`…/blob/<branch>/.github/workflows/<file>` or
`…/src/branch/<branch>/.forgejo/workflows/<file>`, whose branch it runs on
unless `--branch` names another. A URL of another repository starts the run
there, on its default branch unless a branch is named, and needs no workspace.
It is a new run, not a rerun of a PR's checks. On GitHub and GitLab, a run on the PR's head commit
reports checks there like any other run. Forgejo attaches a dispatched run to no
commit, so a failed check stays failed for `shoal view` and PR cleanup until
`ci rerun` reruns it on Forgejo 17 or later. GitHub,
Forgejo and Gitea dispatch the named workflow file, or a GitHub workflow ID,
which needs a `workflow_dispatch` trigger; GitLab runs the branch's pipeline,
taking no workflow and passing inputs to its `spec:inputs`; Bitbucket runs the named
custom pipeline, or the branch's own without one, with the inputs as its
variables. The daemon starts it as the [agent account](agents.md#agent-accounts), or in
another repository with the user's own logins. A scoped process, a
workspace's or a chat's agent, starts runs only on the default branch and on
the branches of the workspaces its scope reaches, in the repository it acts
in.
`--json` returns `workflow`, `branch` and the run's `url` where the forge
reports it, which GitHub and Gitea do not. Inside a workspace, a run with a URL
is [linked](links.md) as opened where Shoal reads runs, which
Bitbucket's it does not, so `shoal watch` reports when it finishes; `linked` says
whether it was. A chat links the runs it starts the same way.

`shoal ci rerun` and `shoal ci cancel` act on one run when given its web URL, as
`ci run` or a check links it, or the number in that URL. `rerun` reruns every
job of a finished run, or with `--failed` only its failed jobs; GitLab retries
only failed and cancelled jobs either way, Forgejo with `--failed` reruns each
failed job and the jobs that need it, also while the run is running, and
Forgejo before 17, Gitea and Bitbucket cannot rerun. `cancel` cancels a running run on every forge but Bitbucket.
A scoped process acts only on runs of the branches it starts them on, and on
runs of a PR in its repository whose head is one of those branches, on any
commit of the PR.
`--json` returns the run's `url` and the `action`.

A run's URL, or `--repo`, may name another repository than that of the
workspace or checkout, registered or not, and outside both it needs neither.
Anyone reads its runs with `view ci` and `ci ls`, which lists its default
branch's without `--branch`; `ci run`, `ci rerun`, `ci cancel`, `ci approve`
and `link ci` act there with the user's own logins, as merging does, and
processes inside a workspace link and act only on runs of its own repository.

Without a run, `shoal ci rerun` reruns failed jobs on the current head of the
linked PR or the one `--pr` names, and `shoal ci cancel` cancels running or
waiting ones:
every such CI job, leaving other providers' checks alone, or each repeated
`--check` by the name `shoal view` shows, which must be a CI job in that state.
The daemon uses the forge's job API as the agent account and records an
`item_changed` event once a request succeeds; a failed request stops the rest.
Outside a workspace, `rerun`, `cancel` and `approve` record no event, and
outside a chat too they need a run or `--pr`.
GitHub reruns a failed Actions job, or every failed job of a run when several of
its jobs are selected, because a rerun starts a new attempt of the run, refuses
a rerun while another job of the run is still running, and cancels the job's
run; GitLab retries or cancels each job; Forgejo 17 and later rerun a failed
job with the jobs that need it, or its run when several of the run's jobs are
selected, which waits until none of them runs, and Forgejo cancels each check's
run. A check whose run ran on an older commit than the head is cancelled with
it but not rerun, since only a new run on the head checks the head.
`--json` returns `url`, `action` and the `checks` acted on.

`shoal view ci` shows a run's state (`waiting` until a runner takes a job,
`running`, `failed`, `finished`, or `cancelled` when the only jobs that
stopped short were cancelled) and, once it finished, its result as the forge
reports a check's, such as `success`, `failure`, `cancelled` or `skipped`,
branch, workflow and jobs, each in one of those states or `cancelled` when
it was cancelled and with its result once it finished, and the log
tails of its failed jobs as `shoal view`
shows a PR's; `--job` shows only one job's whole log, in any state and up to
8 MiB, by its name or, as several jobs may share one, the ID the view lists
beside it. A number after `ci`
names a run; a number alone never does. `shoal ci ls` lists the newest runs,
ten unless `--limit` says up to 100, on the workspace's branch, the one
`--branch` names or the head commit of the PR `--pr` names, and of one workflow
file with `--workflow`, which GitLab does not take, reading the jobs of each
failed run for its state and result as `view ci` does. Both read any run of
the repository as the agent account, on every forge but Bitbucket. `--json`
returns each run's `url`, `state`, `result`, `branch` and `workflow`, and for
`view` its `jobs` with their `id`, `name`, `state`, `result` and `url`, the
`logs` it read and the `errors` of those it could not.

Forges hold the runs of PRs from forks and first-time contributors until a
maintainer approves them, and until then those runs report no checks. `shoal
view` and `shoal status` list an open PR's waiting runs in `waiting_runs`, each
with its `url` and `workflow`, and the text names how to approve them. GitHub
reports the runs it holds; Forgejo and Gitea mark them among the head commit's
runs; GitLab and Bitbucket hold none. `shoal ci approve` approves one run, which
must be waiting, or with `--pr`, or neither for the linked PR, every waiting run
on the PR's head, failing when none waits. It uses your login, or the agent
account for a scoped process, which approves as its space's
[`ci_approve` permission](modes.md#permitted-actions) allows, and records an
`item_changed` event. A proposed approval names the runs it approves, so a
run that starts waiting later, as for a later push, asks again. Only
GitHub approves through its API: Forgejo and Gitea approve in the web UI, so
there the command fails before any request. `--json` returns the `approved`
runs' URLs.

## Runner

While [`runner.enabled`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#runner) is on for a registered repository, the daemon runs its
CI jobs with your own login, the `user_token` of its [forge host](issues.md#forges), which
must be allowed to administer the repository's Actions. On Forgejo it registers
a runner for the repository and keeps the runner's UUID and token in its state;
turning the setting off, or removing the repository, deletes the runner on the
forge, and when the forge cannot be reached, removal goes ahead and a
`runner_failed` alert names the runner to delete by hand. On GitHub it
registers a single-use runner for each job instead, so turning the setting on
only checks that your login may. `[runner]` resolves like any repository
option; a change to the local config applies at once, others within a minute.
The runner takes jobs whose `runs-on` labels are among `labels` and
`shoal-<os>-<arch>`, such as `shoal-linux-x86_64` or `shoal-macos-arm64`. It
needs Forgejo 15 or later with `forgejo-runner` on the daemon's `PATH`, or
GitHub with the `bin` directory of an
[`actions/runner`](https://github.com/actions/runner) installation on it;
otherwise, or when the forge refuses the registration, a `runner_failed`
alert says why and nothing is claimed, and the daemon tries again five
minutes later or when the config changes. Other forges and Gitea report the
`run_jobs` capability as unsupported.

The jobs the forge holds for the runner's labels join the [agent
queue](queue.md) as `job` entries named `ci-<job>-<ID>`, whose workspaces
take the job's name in a directory of the entry's, ranked with the spaces
its run links, ahead of a queued agent of the same space. The daemon reads them every ten seconds
while a job could start and every minute while none can, so new jobs show in
the queue and the entries of jobs the forge no longer lists, which another
runner took or a cancel ended, leave it. `machine.max_jobs`
limits jobs as [`machine.max_heavy_agents`](../../crates/shoal-cli/src/cli/commands/configuration/reference.md#machine) limits agents, counting only jobs;
0 takes none. Under
`dynamic`, agents and jobs start through one admission, one at a time at least
10 seconds apart, while measured memory and disk leave room below the
`[machine.overload.warning]` thresholds: a job is expected to use the 95th
percentile of the resident memory its latest 30 whole runs reached, by
repository, workflow and job name, leaving out runs that load protection or
the user cut short, or what a starting agent counts at before it has run, and until its runner ends, however long its workspace remains, it reserves what it is expected to use beyond what its workspace used
at the latest usage sample, all of it before one. A job's recent start
counts against an agent's as an agent's counts against a job's. A job
beside running ones also waits while the machine's CPU, niced time
included, has been busy at or above `[machine.overload.warning]
cpu_used_percent` on average over its `cpu_sustained_seconds`, or has not
been read that long, while the first job starts whatever the CPU reads: a
job's own CPU use is never reserved, since builds and tests use every core
that is free, but jobs piled on a saturated CPU only make each other
slower. With
`runner.permit`, a job waits until its pool has a free permit and holds one
while it runs. Until a job is admitted, any other runner may take it. `queue
start` admits a job regardless of the limits, and `queue cancel` passes it
over, leaving it to other runners. A PR's job whose PR
has a newer head is passed over too, at the next poll of the forge's waiting
jobs while it waits, whatever holds it back, or when it is admitted; its run is
cancelled on the forge as the user unless it finished, so it no longer holds
the PR's checks; a cancel that fails sends a `runner_failed` alert.

An admitted job runs the forge's runner at nice 10, as a tracked execution in a
disposable workspace at the job's commit, fetched from its PR's or branch's ref, or by
commit. On Forgejo that is `forgejo-runner one-job --handle <handle>` with
each label on the host backend. On GitHub it is a just-in-time runner for the
labels, run from a copy of the installation in the job's directory under the
state directory, cloned where the filesystem clones files and copied
otherwise, since the runner writes its credentials and work into its
installation; keep the installation updated, or each copy updates itself
before its job. GitHub gives that runner any job its labels match: one of
another run than the admitted job's waits before its first step until the
daemon moves the workspace to that run's commit, and the job is then listed,
counted and cancelled as the one it took; a workspace that cannot move fails
the job. A runner GitHub gives no job is deleted once it has waited a minute
and no job waits for its labels, and its workspace goes. A `forgejo-runner`
whose job the forge no longer lists as waiting is stopped at the poll that
sees it when the job's log names another runner as the one that took it, or
the job ended before any runner took it, and its workspace goes. A runner
that ends without taking its job, however it ends, counts nothing towards the
job's expected memory: one GitHub registered whose hook reported no job, or a
`forgejo-runner` whose job's log names no runner of this machine, as one that
found its job taken already. The workspace skips setup
and starts from the [seed build](setup.md#seed-builds) like any workspace, so the job
rebuilds only what its commit changed. The job gets its workspace's
environment, scope token, `TMPDIR`, `[env]` and the variables its
[presets](setup.md#build-tool-presets) give jobs, but no agent secrets and no
forge login besides the token the forge gives the job. Once the execution ends,
however it ended, Shoal stops what its steps left running in the workspace,
naming them in a `job_ended` event in its log, and the workspace goes whatever
it holds, with its permit; after
a daemon restart, a job whose runner is gone goes the same way and the forge
times it out. The service manager's stop of the service after the daemon
exited leaves the job running for the next daemon, as it leaves
[agents](daemon.md#daemon-restarts).
Protection never stops a running job; the workflow's
`timeout-minutes` bounds it. `shoal stop` on the job's workspace, and `shoal ci
cancel`, cancel the job's run on the forge, which cancels each of its jobs; the
forge's runner then stops the job and its workspace goes. A runner still
running a minute after the forge reports the job's run finished, cancelled or
timed out, as when a process a step left behind holds its output open, is
ended by Shoal: it stops every process of the job's workspace, including
leftovers in other process groups or whose parent exited, then the runner,
and records a `job_ended` event in the workspace's log. `shoal stop` says
which run it asked the forge to cancel (`cancelled_run` with `--json`), and
`shoal rm` of a running job's workspace and `shoal daemon stop` cancel the run
before they stop the runner, while a signal to the daemon leaves the job
running. The machine going to sleep, the stop before a system shutdown and a
hangup or terminate signal to the job's wrapper, as a logout or a shutdown
sends, cut the job off instead, on every machine: Shoal ends it as
[load protection](load.md) does, cancelling that job alone, and
reruns it on its forge as soon as its run has finished, at once where it
already has, so any runner can take it, and a `jobs_rerun` alert names each
such job once it reruns. On a machine joined to a hub, a sleep or shutdown
hands the jobs it cut off to the hub, which reruns them while the machine is
away, through its own machine or another connected one with the user's login
for the run's forge, and the machine never reruns them itself once it is back.
A job its wrapper's signal cuts off is cancelled by
the daemon before the wrapper stops the runner, which may report its job
first; a wrapper the daemon does not answer within 15 seconds stops the runner
without the cancel and says why in the job's log. A job on a machine that
sleeps or drops without announcing it runs until the forge times it out. The
daemon's log records each stop of a job's runner with the job, its run, what
stopped it and whether the run or the job was cancelled. A job that cannot start is passed over with a
`runner_failed` alert, and its workspace goes, also one whose
checkout failed or a restart cut short; a removal that fails is tried again
until it succeeds.

`shoal list` shows a running job's workspace below the first workspace that
links its run's PR, or owns the branch it ran on, with the job and what its
processes use. The `shoal status` of every space that links the run's PR, and
of the workspace that owns its branch, lists the job under `CI jobs`, and each
job still waiting to start under `Waiting jobs` with what holds it back, as
`shoal queue` shows it. The job is not a child of any of them: it starts from
the seed rather than a workspace's build, and no agent's scope there reaches
it.

`forgejo-runner`'s host backend starts each step in a directory of its own,
not the job's workspace, so a job for this runner names the workspace as its
working directory and has no checkout step, since the workspace has the commit
already. `SHOAL_WORKSPACE_PATH` and `SHOAL_SPACE`, which Shoal gives
every job's steps and its `env` context, serve both, also in a job that other
runners take too. On GitHub the variables reach the steps from a job-started
hook, after job defaults are read, and the job's default working directory is
the workspace, so the same job works there:

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
