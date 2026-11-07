//! What `shoal view` reads through the daemon: issues and PRs as the forge
//! reports them, with failed checks' logs, and CI runs with their jobs and
//! failed jobs' logs, in the order they were selected.
use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{
    connection::{self, Daemon},
    forge::{
        ci::{
            JobState,
            checks::CheckLog,
            run_view::{JobRef, RunJob, RunView},
        },
        items::link::{LinkKind, Selection},
        review::view::ItemView,
    },
    model::SelectedItem,
    output::view::{RunRead, Shown, ShownRun},
    protocol::{CiMethod, ItemContext, ItemMethod, ViewPiece},
};

/// The items a selection named, in order, and the views of the issues and
/// PRs among them, in the same order.
#[derive(Serialize, Deserialize)]
pub struct Viewed {
    pub items: Vec<SelectedItem>,
    pub views: Vec<ItemView>,
}

/// The items `selection` names in `context`, each as the forge reports it
/// now, in selection order: issues and PRs with their comments when
/// `comments` is set and the logs of a PR's failed checks, and runs with
/// their jobs and failed jobs' logs.
pub async fn read(
    daemon: &impl Daemon,
    context: ItemContext,
    selection: Selection,
    comments: bool,
) -> Result<Vec<Shown>> {
    let viewed = view_items(daemon, context.clone(), selection, comments).await?;
    let runs = read_runs(daemon, &context, &viewed.items).await;
    Ok(in_selection_order(&viewed.items, viewed.views, runs))
}

/// The selection and its issues' and PRs' views, reassembled from the
/// pieces of JSON the daemon sends them in.
async fn view_items(
    daemon: &impl Daemon,
    context: ItemContext,
    selection: Selection,
    comments: bool,
) -> Result<Viewed> {
    let method = ItemMethod::ViewItems {
        context,
        selection,
        comments,
    };
    let pieces = connection::pages::<ViewPiece>(daemon, method).await?;
    let json: String = pieces.into_iter().map(|piece| piece.0).collect();
    serde_json::from_str(&json).context("the daemon sent an invalid view")
}

/// Each selected run with its jobs and failed jobs' logs, read in the
/// context the items were selected in.
async fn read_runs(
    daemon: &impl Daemon,
    context: &ItemContext,
    items: &[SelectedItem],
) -> Vec<ShownRun> {
    let mut runs = Vec::new();
    for item in items {
        if item.kind != LinkKind::Ci {
            continue;
        }
        let read = read_run(daemon, Some(context), &item.url).await;
        let read = read
            .map(RunRead::Read)
            .unwrap_or_else(|error| RunRead::Failed {
                url: item.url.clone(),
                errors: vec![format!("{error:#}")],
            });
        runs.push(ShownRun {
            kind: LinkKind::Ci,
            read,
        });
    }
    runs
}

/// The issues and PRs, and the runs, each read in selection order, merged
/// back into that order.
fn in_selection_order(
    items: &[SelectedItem],
    views: Vec<ItemView>,
    runs: Vec<ShownRun>,
) -> Vec<Shown> {
    let (mut views, mut runs) = (views.into_iter(), runs.into_iter());
    items
        .iter()
        .filter_map(|item| match item.kind.item() {
            Some(_) => views.next().map(|view| Shown::Item(Box::new(view))),
            None => runs.next().map(Shown::Run),
        })
        .collect()
}

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
