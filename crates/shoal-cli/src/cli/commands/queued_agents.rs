//! `shoal start` launches under `max_agents`: admission, the queued start
//! workers, and the hold that keeps a starting launch's slot.

use anyhow::Result;

use super::listing::ListFilter;
use crate::client::space_ids::SpaceId;
use crate::client::space_names::SpaceSelector;
use crate::client::{
    queue::{self, QueueEntryId},
    space_names::SpaceName,
};
use crate::{
    cli::{context::Context, output::Palette, ui},
    model::{Blocker, QueueEntry, QueueRequest, QueuedPlan, Workspace},
    start_plan::AddPlan,
    state::{QueueKind, QueueState},
    terminal,
};

pub(super) enum Admission {
    /// Start now; the entry, when one tracks the slot, and the branch of a
    /// done blocker to build on.
    Start {
        entry: Option<QueueEntryId>,
        base: Option<String>,
    },
    /// Wait in the queue behind this many entries in total.
    Queued(Box<QueueEntry>, usize),
}

/// An agent launch to admit.
pub(super) struct Launch {
    pub agent: crate::agent::Agent,
    /// The workspace the launch opens.
    pub name: SpaceName,
    pub issue_title: Option<String>,
    /// The labels of its issue, which may give it a priority.
    pub issue_labels: Vec<String>,
    pub now: bool,
    pub blockers: Vec<Blocker>,
    /// The existing workspace the launch reopens, whose agent it becomes.
    pub opens: Option<SpaceId>,
    /// The caller starts the launch once admitted, even in the existing
    /// workspace it opens, which the daemon otherwise starts.
    pub caller_starts: bool,
}

/// Ask the daemon for a slot for `launch`, which `plan` starts.
pub(super) async fn admit(ctx: &Context, plan: &AddPlan, launch: Launch) -> Result<Admission> {
    let request = QueueRequest {
        repository: plan.repository.clone(),
        name: launch.name,
        agent: launch.agent,
        mode: plan.mode.clone(),
        narrowing: plan.narrowing,
        issue_title: launch.issue_title,
        issue_labels: launch.issue_labels,
        plan: serde_json::to_string(plan)?,
        env: terminal::start_environment(),
        now: launch.now,
        workspace: launch.opens.map(SpaceSelector::Id),
        starts_in_caller: launch.caller_starts,
        blockers: launch.blockers,
        priority: plan.creation.options.priority,
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

/// Report the launch `entry`, which waits behind `waiting` entries in
/// total, or which the daemon already starts in the background.
pub(super) fn report_queued(ctx: &Context, entry: &QueueEntry, waiting: usize) -> Result<i32> {
    let text = match entry.state {
        QueueState::Queued => format!(
            "Queued {} (position {} of {waiting}); it starts {}",
            entry.name,
            entry.position.unwrap_or(waiting),
            starts_when(entry)
        ),
        _ => format!("Starting {} in the background", entry.name),
    };
    ctx.emit(&text, entry)?;
    Ok(0)
}

/// When the queued `entry` starts: once nothing blocks it and an agent slot
/// is free.
pub(super) fn starts_when(entry: &QueueEntry) -> String {
    match entry.blockers.as_slice() {
        [] => "when an agent slot is free".to_owned(),
        blockers => format!(
            "once it is no longer blocked by {} and an agent slot is free",
            blockers.join(", ")
        ),
    }
}

/// Record the workspace a starting entry opened, or the existing one a
/// waiting launch opens.
pub(super) async fn attach(
    ctx: &Context,
    entry: &QueueEntryId,
    workspace: &Workspace,
) -> Result<()> {
    queue::attach(&ctx.paths, entry.clone(), SpaceSelector::id(&workspace.id)).await
}

/// Free a starting entry's slot after its launch failed; the daemon notifies
/// the user with `error`.
pub(super) async fn fail(ctx: &Context, entry: &QueueEntryId, error: &anyhow::Error) {
    let failed = queue::fail(&ctx.paths, entry.clone(), format!("{error:#}")).await;
    if let Err(error) = failed {
        eprintln!("warning: cannot release the agent queue slot: {error:#}");
    }
}

pub(super) use crate::client::queue::Hold;

async fn plan(ctx: &Context, entry: &QueueEntryId) -> Result<(QueuedPlan, AddPlan)> {
    let queued = queue::plan(&ctx.paths, entry.clone()).await?;
    let mut plan: AddPlan = serde_json::from_str(&queued.plan)?;
    plan.queue_entry = Some(entry.clone());
    Ok((queued, plan))
}

/// The daemon's worker for an admitted entry: open its tab or session, or
/// restore its stopped agent.
pub(super) async fn start(ctx: &Context, entry: QueueEntryId) -> Result<i32> {
    let started = async {
        let queued = entries(ctx)
            .await?
            .into_iter()
            .find(|queued| queued.id == entry)
            .ok_or_else(|| anyhow::anyhow!("queue entry no longer exists"))?;
        match queued.kind {
            QueueKind::Launch => start_launch(ctx, &entry).await,
            QueueKind::Restore => super::resume::queued::start_queued(ctx, &queued).await,
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
async fn start_launch(ctx: &Context, entry: &QueueEntryId) -> Result<()> {
    let (queued, plan) = launch_plan(ctx, entry).await?;
    let opens = queued.entry.opens.as_ref();
    super::workspaces::start_queued(ctx, plan, opens, &queued.entry.name).await
}

/// Run the launch `entry`, which this process marked starting for itself,
/// here, holding its slot until its agent takes it over.
pub(super) async fn run_here(ctx: &Context, entry: &QueueEntryId) -> Result<i32> {
    let (_, plan) = launch_plan(ctx, entry).await?;
    super::workspaces::execute_add(ctx, plan).await
}

/// The plan of the starting launch `entry`, building on the branch of the
/// done blocker it waited for, refusing one that no longer opens the
/// existing workspace it was queued for.
async fn launch_plan(ctx: &Context, entry: &QueueEntryId) -> Result<(QueuedPlan, AddPlan)> {
    let (queued, mut plan) = plan(ctx, entry).await?;
    if let Some(workspace) = &queued.entry.opens {
        let workspace =
            crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(workspace))
                .await?
                .workspace;
        anyhow::ensure!(
            plan.opens(&workspace),
            "the queued launch does not open workspace {}",
            workspace.name
        );
    }
    plan.creation.build_on(queued.entry.base.clone());
    Ok((queued, plan))
}

pub(super) async fn entries(ctx: &Context) -> Result<Vec<QueueEntry>> {
    queue::entries(&ctx.paths).await
}

/// The entries `shoal list` and `shoal queue` show under `filter`.
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

/// Remove `selector` from the queue when it is a waiting launch's ID; `None`
/// leaves it to workspace removal, which a starting entry belongs to.
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
    let found = entries(ctx).await?.into_iter().any(|entry| {
        entry.kind == QueueKind::Launch
            && entry.state == QueueState::Queued
            && entry.id == selector
            && takes(&entry)
    });
    if !found {
        return Ok(None);
    }
    let entry = queue::remove(&ctx.paths, selector.to_owned()).await?;
    ctx.emit(
        &format!("Removed {} from the agent queue", entry.name),
        &entry,
    )?;
    Ok(Some(0))
}
