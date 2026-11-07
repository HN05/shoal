//! `shoal start` launches under `max_agents`: admission, the queued start
//! workers, and the hold that keeps a starting launch's slot.

use anyhow::Result;

use super::listing::ListFilter;
use crate::client::space_names::SpaceSelector;
use crate::client::{queue, space_names::SpaceName};
use crate::{
    agent_queue::AddPlan,
    cli::{context::Context, output::Palette, ui},
    model::{Dependency, QueueEntry, QueueRequest, QueuedPlan, Workspace},
    state::{QueueKind, QueueState},
    terminal,
};

pub(super) enum Admission {
    /// Start now; the entry, when one tracks the slot, and the branch of a
    /// done dependency to build on.
    Start {
        entry: Option<String>,
        base: Option<String>,
    },
    /// Wait in the queue behind this many entries in total.
    Queued(Box<QueueEntry>, usize),
}

/// An agent launch to admit.
pub(super) struct Launch {
    pub agent: String,
    /// The workspace the launch opens.
    pub name: SpaceName,
    pub issue_title: Option<String>,
    pub now: bool,
    pub after: Vec<Dependency>,
}

/// Ask the daemon for a slot for `launch`, which `plan` starts.
pub(super) async fn admit(ctx: &Context, plan: &AddPlan, launch: Launch) -> Result<Admission> {
    let request = QueueRequest {
        repository: plan.repository.clone(),
        name: launch.name,
        agent: launch.agent,
        mode: plan.mode.clone(),
        issue_title: launch.issue_title,
        plan: serde_json::to_string(plan)?,
        env: terminal::start_environment(),
        now: launch.now,
        workspace: None,
        after: launch.after,
    };
    let admission = queue::add(&ctx.paths, request).await?;
    Ok(match (admission.start, admission.entry) {
        (true, entry) => Admission::Start {
            base: entry.as_ref().and_then(|entry| entry.base.clone()),
            entry: entry.map(|entry| entry.id),
        },
        (false, Some(entry)) => Admission::Queued(Box::new(entry), admission.waiting),
        (false, None) => anyhow::bail!("the daemon queued the agent without an entry"),
    })
}

pub(super) fn report_queued(ctx: &Context, entry: &QueueEntry, waiting: usize) -> Result<i32> {
    let start = starts_when(entry);
    ctx.emit(
        &format!(
            "Queued {} (position {} of {waiting}); it starts {start}",
            entry.name,
            entry.position.unwrap_or(waiting)
        ),
        entry,
    )?;
    Ok(0)
}

/// When the queued `entry` starts: once the work it waits for is complete
/// and an agent slot is free.
pub(super) fn starts_when(entry: &QueueEntry) -> String {
    match entry.waiting_for.as_slice() {
        [] => "when an agent slot is free".to_owned(),
        [work] => format!("once {work} is complete and an agent slot is free"),
        work => format!(
            "once {} are complete and an agent slot is free",
            work.join(", ")
        ),
    }
}

/// Record the workspace a starting entry opened, or the existing one a
/// waiting launch opens.
pub(super) async fn attach(ctx: &Context, entry: &str, workspace: &Workspace) -> Result<()> {
    queue::attach(
        &ctx.paths,
        entry.to_owned(),
        SpaceSelector::id(&workspace.id),
    )
    .await
}

/// Free a starting entry's slot after its launch failed; the daemon notifies
/// the user with `error`.
pub(super) async fn fail(ctx: &Context, entry: &str, error: &anyhow::Error) {
    let failed = queue::fail(&ctx.paths, entry.to_owned(), format!("{error:#}")).await;
    if let Err(error) = failed {
        eprintln!("warning: cannot release the agent queue slot: {error:#}");
    }
}

pub(super) use crate::client::queue::Hold;

async fn plan(ctx: &Context, entry: &str) -> Result<(QueuedPlan, AddPlan)> {
    let queued = queue::plan(&ctx.paths, entry.to_owned()).await?;
    let mut plan: AddPlan = serde_json::from_str(&queued.plan)?;
    plan.queue_entry = Some(entry.to_owned());
    Ok((queued, plan))
}

/// The daemon's worker for an admitted entry: open its tab or session, or
/// restore its stopped agent.
pub(super) async fn start(ctx: &Context, entry: String) -> Result<i32> {
    let started = async {
        let queued = entries(ctx)
            .await?
            .into_iter()
            .find(|queued| queued.id == entry)
            .ok_or_else(|| anyhow::anyhow!("queue entry no longer exists"))?;
        match queued.kind {
            QueueKind::Launch => start_launch(ctx, &entry).await,
            QueueKind::Restore => super::resume::start_queued(ctx, &queued).await,
            QueueKind::Job => anyhow::bail!("the daemon starts a CI job itself"),
        }
    }
    .await;
    if let Err(error) = &started {
        fail(ctx, &entry, error).await;
    }
    started.map(|()| 0)
}

/// Open the tab or session of the admitted launch `entry`.
async fn start_launch(ctx: &Context, entry: &str) -> Result<()> {
    let (queued, plan) = plan(ctx, entry).await?;
    if let Some(workspace) = &queued.entry.opens {
        let workspace =
            crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::parse(workspace))
                .await?
                .workspace;
        anyhow::ensure!(
            plan.opens(&workspace),
            "the queued launch does not open workspace {}",
            workspace.name
        );
    }
    let mut plan = plan;
    plan.creation.build_on(queued.entry.base.clone());
    super::workspaces::start_queued(ctx, plan, &queued.entry.name).await
}

pub(super) async fn entries(ctx: &Context) -> Result<Vec<QueueEntry>> {
    queue::entries(&ctx.paths).await
}

/// The entries `shoal work` and `shoal queue` show under `filter`.
pub(super) async fn listed(ctx: &Context, filter: &ListFilter) -> Result<Vec<QueueEntry>> {
    let mut entries = entries(ctx).await?;
    entries.retain(|entry| filter.keeps_entry(entry));
    Ok(entries)
}

/// Text rows for `entries`; JSON output needs none.
pub(super) async fn rows(ctx: &Context, entries: &[QueueEntry]) -> Result<Vec<String>> {
    if ctx.json || entries.is_empty() {
        return Ok(Vec::new());
    }
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    Ok(ui::queue_rows(
        entries,
        &repositories,
        Palette::stdout(ctx.json),
    ))
}

/// Remove `selector` from the queue when it names a waiting launch; `None`
/// leaves it to workspace removal, which a starting entry belongs to. A name
/// shared with a workspace is refused rather than guessed.
pub(super) async fn remove(ctx: &Context, selector: &str) -> Result<Option<i32>> {
    remove_waiting(ctx, selector, |_| true).await
}

/// Remove `selector` from the queue as [`remove`] does when it names a
/// waiting launch of a new workspace; `None` leaves it to `shoal stop` of a
/// workspace, which cancels the launches waiting to open it.
pub(super) async fn remove_new(ctx: &Context, selector: &str) -> Result<Option<i32>> {
    remove_waiting(ctx, selector, |entry| entry.opens.is_none()).await
}

/// Remove `selector` from the queue as [`remove`] does when it names a
/// waiting launch that `takes` accepts.
async fn remove_waiting(
    ctx: &Context,
    selector: &str,
    takes: impl Fn(&QueueEntry) -> bool,
) -> Result<Option<i32>> {
    let Some(entry) = entries(ctx).await?.into_iter().find(|entry| {
        entry.kind == QueueKind::Launch
            && entry.state == QueueState::Queued
            && (entry.id == selector || entry.name == selector)
            && takes(entry)
    }) else {
        return Ok(None);
    };
    if entry.id != selector
        && let Some(workspace) = crate::client::workspaces::list(&ctx.paths)
            .await?
            .into_iter()
            .find(|workspace| workspace.name == selector)
    {
        anyhow::bail!(
            "{selector} names both a workspace and a queued agent; pass the workspace ID {} or the queued agent's ID {}",
            workspace.id,
            entry.id
        );
    }
    let entry = queue::remove(&ctx.paths, selector.to_owned()).await?;
    ctx.emit(
        &format!("Removed {} from the agent queue", entry.name),
        &entry,
    )?;
    Ok(Some(0))
}
