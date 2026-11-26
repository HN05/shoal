//! `shoal view ci` and `shoal ci ls`.
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
use shoal_protocol::protocol::RunsOf;
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn view(
    ctx: &Context,
    items: ItemArgs,
    repo: Option<String>,
    job: Option<JobRef>,
    web: bool,
) -> Result<i32> {
    let run = (items.selection()?.input).context("view ci needs a run's number or URL")?;
    let reading = {
        let run = run.clone();
        crate::cli::commands::items::in_context(move |target| {
            crate::client::ci::run(target, run.clone())
        })
    };
    let target = select_item_target(ctx, items.space, repo, Some(&run), &reading).await?;
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

async fn open(ctx: &Context, target: RepoTarget, run: ItemInput) -> Result<i32> {
    items_view::may_open_browser(ctx)?;
    let run = crate::client::ci::run(target, run).send(&ctx.paths).await?;
    items_view::open_urls(ctx, vec![run.url])
}

pub(super) async fn list(
    ctx: &Context,
    space: Option<SpaceSelector>,
    repo: Option<String>,
    pr: Option<ItemInput>,
    filter: RunFilter,
) -> Result<i32> {
    validate_filter(&filter)?;
    let of = pr.clone().map_or(RunsOf::Branch, RunsOf::Pull);
    let listing = {
        let (of, filter) = (of.clone(), filter.clone());
        crate::cli::commands::items::in_context(move |target| {
            crate::client::ci::runs(target, of.clone(), filter.clone())
        })
    };
    let target = select_item_target(ctx, space, repo, pr.as_ref(), &listing).await?;
    let runs = crate::client::ci::runs(target, of, filter)
        .send(&ctx.paths)
        .await?;
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
