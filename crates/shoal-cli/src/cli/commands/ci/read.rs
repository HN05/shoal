//! `shoal view ci` and `shoal ci ls`: read CI runs with their jobs and the
//! logs of failed ones.
use anyhow::{Context as _, Result};

use crate::{
    cli::{
        args::items::ItemArgs,
        commands::{items::select_item_target, view as items_view},
        context::Context,
    },
    client::views,
    forge::{
        ci::{
            JobState,
            check_state::CheckState,
            run_action::Run,
            run_view::{JobRef, RunFilter, RunView},
        },
        items::reference::ItemInput,
    },
    model::Relation,
    protocol::RepoTarget,
};
use shoal_forge::ci::run_view::validate_filter;
use shoal_protocol::space_names::SpaceSelector;

/// Show the run `items` selects after `ci` with the log tails of its failed
/// jobs, only the log of `job`, or open it in the browser.
pub(in crate::cli::commands) async fn view(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
    job: Option<JobRef>,
    web: bool,
) -> Result<i32> {
    let run = (items.selection()?.input).context("view ci needs a run's number or URL")?;
    let target = select_item_target(ctx, items.space, repo, Some(&run)).await?;
    if web {
        return open(ctx, target, run).await;
    }
    let target = &target;
    if let Some(job) = job {
        let log = views::job_log(&ctx.paths, target, &run, job).await?;
        ctx.show(&log, items_view::render_log)?;
        return Ok(0);
    }
    let view = ctx
        .progress("Reading run", views::read_run(&ctx.paths, target, &run))
        .await?;
    ctx.show(&view, |view| render_run(view, &[]))?;
    Ok(0)
}

/// Open the run in the browser, by its URL the forge reports.
async fn open(ctx: &Context, target: RepoTarget, run: ItemInput) -> Result<i32> {
    items_view::may_open_browser(ctx)?;
    let run = crate::client::ci::run(&ctx.paths, target, run).await?;
    items_view::open_urls(ctx, vec![run.url])
}

/// The newest runs `filter` selects, or those on the head of the PR `pr`.
pub(super) async fn list(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: Option<String>,
    pr: Option<ItemInput>,
    filter: RunFilter,
) -> Result<i32> {
    validate_filter(&filter)?;
    let target = select_item_target(ctx, space, repo, pr.as_ref()).await?;
    let runs = crate::client::ci::runs(&ctx.paths, target, pr, filter).await?;
    ctx.show(&runs, render_runs)?;
    Ok(0)
}

pub(in crate::cli::commands) fn render_run(view: &RunView, relations: &[Relation]) {
    outln!("Run:      {}", view.run.url);
    items_view::render_relations(relations);
    outln!("State:    {}", view.run.state);
    if let Some(result) = view.run.result {
        outln!("Result:   {result}");
    }
    if let Some(branch) = &view.run.branch {
        outln!("Branch:   {branch}");
    }
    if let Some(workflow) = &view.run.workflow {
        outln!("Workflow: {workflow}");
    }
    outln!("Jobs:");
    for job in &view.jobs {
        let state = shown_state(job.state, job.result);
        match &job.url {
            Some(url) => outln!("  {state:<9}  {}  (ID {})  {url}", job.name, job.id),
            None => outln!("  {state:<9}  {}  (ID {})", job.name, job.id),
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

/// One line per run: its state or result, workflow, branch and URL.
fn render_runs(runs: &Vec<Run>) {
    if runs.is_empty() {
        outln!("No runs");
    }
    for run in runs {
        let workflow = run.workflow.as_deref().unwrap_or("-");
        let branch = run.branch.as_deref().unwrap_or("-");
        let state = shown_state(run.state, run.result);
        outln!("{state:<9}  {workflow}  {branch}  {}", run.url);
    }
}

/// What a run or job shows in a line: its result once it finished, else its
/// state.
fn shown_state(state: JobState, result: Option<CheckState>) -> &'static str {
    result.map_or(state.as_str(), CheckState::as_str)
}
