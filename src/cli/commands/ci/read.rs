//! `shoal view ci` and `shoal ci list`: read CI runs with their jobs and the
//! logs of failed ones.
use anyhow::{Context as _, Result};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        args::items::ItemArgs,
        commands::{items::select_space_item_context, view as items_view},
        context::Context,
    },
    client::views,
    forge::ci::{
        run_action::{Run, validate_run_reference},
        run_view::{JobRef, RunFilter, RunView, validate_filter},
    },
    model::Relation,
    protocol::ItemContext,
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
    let context = super::input_context(ctx, items.space, repo, Some(&run)).await?;
    if web {
        return open(ctx, context, run).await;
    }
    let context = context.as_ref();
    if let Some(job) = job {
        let log = views::job_log(&ctx.paths, context, &run, job).await?;
        ctx.show(&log, items_view::render_log)?;
        return Ok(0);
    }
    let view = ctx
        .progress("Reading run", views::read_run(&ctx.paths, context, &run))
        .await?;
    ctx.show(&view, |view| render_run(view, &[]))?;
    Ok(0)
}

/// Open the run in the browser, by its URL the forge reports.
async fn open(ctx: &Context, context: Option<ItemContext>, run: String) -> Result<i32> {
    items_view::may_open_browser(ctx)?;
    let run = crate::client::ci::run(&ctx.paths, context, run).await?;
    items_view::open_urls(ctx, vec![run.url])
}

/// The newest runs `filter` selects, or those on the head of the PR `pr`.
pub(super) async fn list(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: Option<String>,
    pr: Option<String>,
    filter: RunFilter,
) -> Result<i32> {
    validate_filter(&filter)?;
    let context = select_space_item_context(ctx, space, repo, pr.as_deref()).await?;
    let runs = crate::client::ci::runs(&ctx.paths, context, pr, filter).await?;
    ctx.show(&runs, render_runs)?;
    Ok(0)
}

pub(in crate::cli::commands) fn render_run(view: &RunView, relations: &[Relation]) {
    outln!("Run:      {}", view.run.url);
    items_view::render_relations(relations);
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
