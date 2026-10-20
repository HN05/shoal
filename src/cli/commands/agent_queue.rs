//! `shoal add` launches under `max_agents`: admission, the queued start
//! workers, and the hold that keeps a starting launch's slot.
use std::time::Duration;

use anyhow::Result;
use tokio::io::AsyncReadExt as _;

use super::{listing::ListFilter, workspaces::AddPlan};
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::Palette,
        terminal, ui,
    },
    model::{QueueAdmission, QueueEntry, QueueRequest, QueuedPlan, Workspace},
    paths::Paths,
    protocol::Method,
    state::{QueueKind, QueueState},
};

/// Reconnect a hold this soon after the daemon restarts.
const HOLD_RETRY: Duration = Duration::from_secs(1);

pub(super) enum Admission {
    /// Start now; the entry, when one tracks the slot.
    Start(Option<String>),
    /// Wait in the queue behind this many entries in total.
    Queued(QueueEntry, usize),
}

/// Ask the daemon for a slot for `agent`, which `plan` starts in workspace `name`.
pub(super) async fn admit(
    ctx: &Context,
    plan: &AddPlan,
    agent: String,
    name: String,
    issue_title: Option<String>,
    now: bool,
) -> Result<Admission> {
    let request = QueueRequest {
        repository: plan.repository.clone(),
        name,
        agent,
        issue_title,
        plan: serde_json::to_string(plan)?,
        env: terminal::environment(),
        now,
    };
    let admission =
        client::request::<QueueAdmission>(&ctx.paths, Method::QueueAdd { request }).await?;
    Ok(match (admission.start, admission.entry) {
        (true, entry) => Admission::Start(entry.map(|entry| entry.id)),
        (false, Some(entry)) => Admission::Queued(entry, admission.waiting),
        (false, None) => anyhow::bail!("the daemon queued the agent without an entry"),
    })
}

pub(super) fn report_queued(ctx: &Context, entry: &QueueEntry, waiting: usize) -> Result<i32> {
    ctx.emit(
        &format!(
            "Queued {} (position {} of {waiting}); it starts when an agent slot is free",
            entry.name,
            entry.position.unwrap_or(waiting)
        ),
        entry,
    )?;
    Ok(0)
}

/// Record the workspace a starting entry opened.
pub(super) async fn attach(ctx: &Context, entry: &str, workspace: &Workspace) -> Result<()> {
    request::<()>(
        &ctx.paths,
        Method::QueueAttach {
            entry: entry.to_owned(),
            workspace: workspace.id.clone(),
        },
    )
    .await
}

/// Free a starting entry's slot after its launch failed; the daemon notifies
/// the user with `error`.
pub(super) async fn fail(ctx: &Context, entry: &str, error: &anyhow::Error) {
    let failed = request::<()>(
        &ctx.paths,
        Method::QueueFail {
            entry: entry.to_owned(),
            error: format!("{error:#}"),
        },
    )
    .await;
    if let Err(error) = failed {
        eprintln!("warning: cannot release the agent queue slot: {error:#}");
    }
}

/// Keeps a starting entry's slot while this process runs, until its agent
/// registers. It reconnects after a daemon restart.
pub(super) struct Hold(tokio::task::JoinHandle<()>);

impl Hold {
    pub fn start(paths: &Paths, entry: &str) -> Self {
        let (paths, entry) = (paths.clone(), entry.to_owned());
        Self(tokio::spawn(async move {
            loop {
                if let Ok((mut stream, body)) = client::open(
                    &paths,
                    Method::QueueHold {
                        entry: entry.clone(),
                    },
                )
                .await
                {
                    // A refusal means the agent took the slot over or the entry ended.
                    if body.into_result().is_err() {
                        return;
                    }
                    let mut closed = [0u8; 1];
                    let _ = stream.read(&mut closed).await;
                }
                tokio::time::sleep(HOLD_RETRY).await;
            }
        }))
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn plan(ctx: &Context, entry: &str) -> Result<(QueuedPlan, AddPlan)> {
    let queued = request::<QueuedPlan>(
        &ctx.paths,
        Method::QueuedPlan {
            entry: entry.to_owned(),
        },
    )
    .await?;
    let mut plan: AddPlan = serde_json::from_str(&queued.plan)?;
    plan.queue_entry = Some(entry.to_owned());
    Ok((queued, plan))
}

/// The daemon's worker for an admitted entry: open its tab or session.
pub(super) async fn start(ctx: &Context, entry: String) -> Result<i32> {
    let started = async {
        let (queued, plan) = plan(ctx, &entry).await?;
        super::workspaces::start_queued(ctx, plan, &queued.entry.name).await
    }
    .await;
    if let Err(error) = &started {
        fail(ctx, &entry, error).await;
    }
    started.map(|()| 0)
}

pub(super) async fn entries(ctx: &Context) -> Result<Vec<QueueEntry>> {
    request(&ctx.paths, Method::ListQueue).await
}

/// The entries `shoal ls` shows under `filter`.
pub(super) async fn listed(ctx: &Context, filter: &ListFilter) -> Result<Vec<QueueEntry>> {
    let mut entries = entries(ctx).await?;
    entries.retain(|entry| filter.keeps_entry(entry));
    Ok(entries)
}

/// `shoal ls --queue`.
pub(super) async fn list(ctx: &Context, filter: &ListFilter) -> Result<i32> {
    let entries = listed(ctx, filter).await?;
    let rows = rows(ctx, &entries).await?;
    ctx.show(&entries, |_| {
        for row in rows {
            println!("{row}");
        }
    })?;
    Ok(0)
}

/// Text rows for `entries`; JSON output needs none.
pub(super) async fn rows(ctx: &Context, entries: &[QueueEntry]) -> Result<Vec<String>> {
    if ctx.json || entries.is_empty() {
        return Ok(Vec::new());
    }
    let repositories = client::repositories(&ctx.paths).await?;
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
    let Some(entry) = entries(ctx).await?.into_iter().find(|entry| {
        entry.kind == QueueKind::Launch
            && entry.state == QueueState::Queued
            && (entry.id == selector || entry.name == selector)
    }) else {
        return Ok(None);
    };
    if entry.id != selector
        && let Some(workspace) = client::workspaces(&ctx.paths)
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
    let entry = request::<QueueEntry>(
        &ctx.paths,
        Method::QueueRemove {
            entry: selector.to_owned(),
        },
    )
    .await?;
    ctx.emit(
        &format!("Removed {} from the agent queue", entry.name),
        &entry,
    )?;
    Ok(Some(0))
}
