//! `shoal view ci` and `shoal ci list`: read CI runs with their jobs and the
//! logs of failed ones.
use anyhow::{Context as _, Result};

use crate::{
    cli::{
        args::items::ItemArgs,
        client,
        commands::{items::select_item_context, view as items_view},
        context::Context,
    },
    forge::ci::{
        JobState,
        checks::CheckLog,
        run_action::{Run, validate_run_reference},
        run_view::{JobRef, RunFilter, RunJob, RunView, validate_filter},
    },
    protocol::{CiMethod, ItemContext},
};

/// Show the run `items` selects after `ci` with the log tails of its failed
/// jobs, only the log of `job`, or open it in the browser.
pub(in crate::cli::commands) async fn view(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
    job: Option<JobRef>,
    web: bool,
) -> Result<i32> {
    let run = items.item.context("view ci needs a run's number or URL")?;
    validate_run_reference(&run)?;
    let context = super::input_context(ctx, items.workspace, repo, Some(&run)).await?;
    if web {
        return open(ctx, context, run).await;
    }
    let context = context.as_ref();
    if let Some(job) = job {
        let log = job_log(ctx, context, &run, job).await?;
        ctx.show(&log, items_view::render_log)?;
        return Ok(0);
    }
    let view = ctx
        .progress("Reading run", read_view(ctx, context, &run))
        .await?;
    ctx.show(&view, render_run)?;
    Ok(0)
}

/// The run `run` names with its jobs and the log tails of its failed jobs,
/// read in `context`, or without one by the run's URL.
pub(in crate::cli::commands) async fn read_view(
    ctx: &Context,
    context: Option<&ItemContext>,
    run: &str,
) -> Result<RunView> {
    let mut view = read_run(ctx, context, run).await?;
    read_failed_logs(ctx, context, run, &mut view).await;
    Ok(view)
}

/// Open the run in the browser, by its URL the forge reports.
async fn open(ctx: &Context, context: Option<ItemContext>, run: String) -> Result<i32> {
    items_view::may_open_browser(ctx)?;
    let run: Run = client::request(&ctx.paths, CiMethod::ViewCiRun { context, run }).await?;
    items_view::open_urls(ctx, vec![run.url])
}

/// The run with its jobs, which arrive in pages of their own.
async fn read_run(ctx: &Context, context: Option<&ItemContext>, run: &str) -> Result<RunView> {
    let (context, input) = (context.cloned(), run.to_owned());
    let run = client::request(
        &ctx.paths,
        CiMethod::ViewCiRun {
            context: context.clone(),
            run: input.clone(),
        },
    )
    .await?;
    let jobs = client::pages::<RunJob>(
        &ctx.paths,
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
async fn read_failed_logs(
    ctx: &Context,
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
        match job_log(ctx, context, run, JobRef::Id(id)).await {
            Ok(log) => view.logs.push(log),
            Err(error) => view
                .errors
                .push(format!("{name}: log unavailable: {error:#}")),
        }
    }
}

async fn job_log(
    ctx: &Context,
    context: Option<&ItemContext>,
    run: &str,
    job: JobRef,
) -> Result<CheckLog> {
    client::request(
        &ctx.paths,
        CiMethod::CiRunJobLog {
            context: context.cloned(),
            run: run.into(),
            job,
        },
    )
    .await
}

/// The newest runs `filter` selects, or those on the head of the PR `pr`.
pub(super) async fn list(
    ctx: &Context,
    workspace: Option<String>,
    repo: Option<String>,
    pr: Option<String>,
    filter: RunFilter,
) -> Result<i32> {
    validate_filter(&filter)?;
    let context = select_item_context(ctx, workspace, repo, pr.as_deref()).await?;
    let method = CiMethod::ListCiRuns {
        context,
        pr,
        filter,
    };
    let runs = client::pages::<Run>(&ctx.paths, method).await?;
    ctx.show(&runs, render_runs)?;
    Ok(0)
}

pub(in crate::cli::commands) fn render_run(view: &RunView) {
    outln!("Run:      {}", view.run.url);
    outln!("State:    {}", view.run.state);
    if let Some(branch) = &view.run.branch {
        outln!("Branch:   {branch}");
    }
    if let Some(workflow) = &view.run.workflow {
        outln!("Workflow: {workflow}");
    }
    outln!("Jobs:");
    for job in &view.jobs {
        match &job.url {
            Some(url) => outln!(
                "  {:<8}  {}  (ID {})  {url}",
                job.state.as_str(),
                job.name,
                job.id
            ),
            None => outln!("  {:<8}  {}  (ID {})", job.state.as_str(), job.name, job.id),
        }
    }
    for log in &view.logs {
        outln!();
        items_view::render_log(log);
    }
    for error in &view.errors {
        outln!("{error}");
    }
}

/// One line per run: its state, workflow, branch and URL.
fn render_runs(runs: &Vec<Run>) {
    if runs.is_empty() {
        outln!("No runs");
    }
    for run in runs {
        let workflow = run.workflow.as_deref().unwrap_or("-");
        let branch = run.branch.as_deref().unwrap_or("-");
        outln!(
            "{:<8}  {workflow}  {branch}  {}",
            run.state.as_str(),
            run.url
        );
    }
}
