//! What `shoal view` reads through the daemon: CI runs with their jobs and
//! failed jobs' logs.
use anyhow::Result;

use crate::{
    client::connection::{self, Daemon},
    forge::ci::{
        JobState,
        checks::CheckLog,
        run_view::{JobRef, RunJob, RunView},
    },
    protocol::{CiMethod, ItemContext},
};

/// The run `run` names with its jobs and the log tails of its failed jobs,
/// read in `context`, or without one by the run's URL.
pub async fn read_run(
    daemon: &impl Daemon,
    context: Option<&ItemContext>,
    run: &str,
) -> Result<RunView> {
    let mut view = run_with_jobs(daemon, context, run).await?;
    read_failed_job_logs(daemon, context, run, &mut view).await;
    Ok(view)
}

/// The run with its jobs, which arrive in pages of their own.
async fn run_with_jobs(
    daemon: &impl Daemon,
    context: Option<&ItemContext>,
    run: &str,
) -> Result<RunView> {
    let (context, input) = (context.cloned(), run.to_owned());
    let run = connection::request(
        daemon,
        CiMethod::ViewCiRun {
            context: context.clone(),
            run: input.clone(),
        },
    )
    .await?;
    let jobs = connection::pages::<RunJob>(
        daemon,
        CiMethod::CiRunJobs {
            context,
            run: input,
        },
    )
    .await?;
    Ok(RunView {
        run,
        jobs,
        logs: Vec::new(),
        errors: Vec::new(),
    })
}

/// Add the logs of the run's failed jobs, recording those it cannot read.
async fn read_failed_job_logs(
    daemon: &impl Daemon,
    context: Option<&ItemContext>,
    run: &str,
    view: &mut RunView,
) {
    let failed = view
        .jobs
        .iter()
        .filter(|job| job.state == JobState::Failed)
        .map(|job| (job.id, job.name.clone()))
        .collect::<Vec<_>>();
    for (id, name) in failed {
        match job_log(daemon, context, run, JobRef::Id(id)).await {
            Ok(log) => view.logs.push(log),
            Err(error) => view
                .errors
                .push(format!("{name}: log unavailable: {error:#}")),
        }
    }
}

/// The log of the run's `job`, read in `context`, or without one by the
/// run's URL.
pub async fn job_log(
    daemon: &impl Daemon,
    context: Option<&ItemContext>,
    run: &str,
    job: JobRef,
) -> Result<CheckLog> {
    connection::request(
        daemon,
        CiMethod::CiRunJobLog {
            context: context.cloned(),
            run: run.into(),
            job,
        },
    )
    .await
}
