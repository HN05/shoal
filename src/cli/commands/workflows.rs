//! `shoal forge ci`: start, read, rerun and cancel CI workflow runs of the
//! workspace's repository.
use anyhow::Result;

use crate::{
    cli::{ForgeCiCommand, RunArgs, RunTarget, client, context::Context, ui},
    forge::ci::{
        CiAction, JobState,
        checks::CheckLog,
        run::{StartedRun, validate_run},
        run_action::{Run, RunActed, validate_run_reference},
        run_view::{JobRef, RunFilter, RunJob, RunView, validate_filter},
    },
    protocol::Method,
};

pub(super) async fn ci(ctx: &Context, command: ForgeCiCommand) -> Result<i32> {
    match command {
        ForgeCiCommand::Run(args) => run(ctx, args).await,
        ForgeCiCommand::Rerun { target, failed } => act(ctx, target, CiAction::Rerun, failed).await,
        ForgeCiCommand::Cancel { target } => act(ctx, target, CiAction::Cancel, false).await,
        ForgeCiCommand::View {
            target,
            job,
            job_id,
        } => {
            let job = job.map(JobRef::Name).or(job_id.map(JobRef::Id));
            view(ctx, target, job).await
        }
        ForgeCiCommand::List {
            workspace,
            branch,
            workflow,
            limit,
        } => {
            let filter = RunFilter {
                branch,
                workflow,
                limit,
            };
            list(ctx, workspace, filter).await
        }
    }
}

async fn run(ctx: &Context, args: RunArgs) -> Result<i32> {
    // Refuse bad arguments before the workspace picker asks for anything.
    validate_run(args.workflow.as_deref(), args.branch.as_deref())?;
    let workspace =
        ui::select_workspace(ctx, args.workspace, ui::Fallback::CurrentDirectory).await?;
    let started: StartedRun = client::request(
        &ctx.paths,
        Method::StartCiRun {
            workspace,
            workflow: args.workflow,
            branch: args.branch,
            inputs: args.inputs.into_iter().collect(),
        },
    )
    .await?;
    ctx.emit(&started_text(&started), &started)?;
    Ok(0)
}

async fn act(ctx: &Context, target: RunTarget, action: CiAction, failed: bool) -> Result<i32> {
    validate_run_reference(&target.run)?;
    let workspace =
        ui::select_workspace(ctx, target.workspace, ui::Fallback::CurrentDirectory).await?;
    let acted: RunActed = client::request(
        &ctx.paths,
        Method::CiRunAction {
            workspace,
            run: target.run,
            action,
            failed,
        },
    )
    .await?;
    ctx.emit(&acted_text(&acted), &acted)?;
    Ok(0)
}

/// What the action did to the run.
fn acted_text(acted: &RunActed) -> String {
    let outcome = match acted.action {
        CiAction::Rerun => "Rerun started",
        CiAction::Cancel => "Cancelled",
    };
    format!("{outcome}: {}", acted.url)
}

/// Show the run with the log tails of its failed jobs, or only the log of
/// `job`.
async fn view(ctx: &Context, target: RunTarget, job: Option<JobRef>) -> Result<i32> {
    validate_run_reference(&target.run)?;
    let workspace =
        ui::select_workspace(ctx, target.workspace, ui::Fallback::CurrentDirectory).await?;
    if let Some(job) = job {
        let log = job_log(ctx, &workspace, &target.run, job).await?;
        ctx.show(&log, super::view::render_log)?;
        return Ok(0);
    }
    let mut view = ctx
        .progress("Reading run", read_run(ctx, &workspace, &target.run))
        .await?;
    read_failed_logs(ctx, &workspace, &target.run, &mut view).await;
    ctx.show(&view, render_run)?;
    Ok(0)
}

/// The run with its jobs, which arrive in pages of their own.
async fn read_run(ctx: &Context, workspace: &str, run: &str) -> Result<RunView> {
    let (workspace, input) = (workspace.to_owned(), run.to_owned());
    let run = client::request(
        &ctx.paths,
        Method::ViewCiRun {
            workspace: workspace.clone(),
            run: input.clone(),
        },
    )
    .await?;
    let jobs = client::pages::<RunJob>(
        &ctx.paths,
        Method::CiRunJobs {
            workspace,
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
async fn read_failed_logs(ctx: &Context, workspace: &str, run: &str, view: &mut RunView) {
    let failed = view
        .jobs
        .iter()
        .filter(|job| job.state == JobState::Failed)
        .map(|job| (job.id, job.name.clone()))
        .collect::<Vec<_>>();
    for (id, name) in failed {
        match job_log(ctx, workspace, run, JobRef::Id(id)).await {
            Ok(log) => view.logs.push(log),
            Err(error) => view
                .errors
                .push(format!("{name}: log unavailable: {error:#}")),
        }
    }
}

async fn job_log(ctx: &Context, workspace: &str, run: &str, job: JobRef) -> Result<CheckLog> {
    client::request(
        &ctx.paths,
        Method::CiRunJobLog {
            workspace: workspace.into(),
            run: run.into(),
            job,
        },
    )
    .await
}

async fn list(ctx: &Context, workspace: Option<String>, filter: RunFilter) -> Result<i32> {
    validate_filter(&filter)?;
    let workspace = ui::select_workspace(ctx, workspace, ui::Fallback::CurrentDirectory).await?;
    let runs = client::pages::<Run>(&ctx.paths, Method::ListCiRuns { workspace, filter }).await?;
    ctx.show(&runs, render_runs)?;
    Ok(0)
}

fn render_run(view: &RunView) {
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
        super::view::render_log(log);
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

/// The run that started and, where the forge reports one, its URL.
fn started_text(started: &StartedRun) -> String {
    let run = match &started.workflow {
        Some(workflow) => format!("{workflow} on {}", started.branch),
        None => started.branch.clone(),
    };
    match &started.url {
        Some(url) => format!("Run started for {run}: {url}"),
        None => format!("Run started for {run}"),
    }
}
