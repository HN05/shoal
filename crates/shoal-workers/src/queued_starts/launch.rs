//! An admitted launch: the add worker that starts its agent in the space
//! the launch opens runs in a background place where the terminal that
//! queued it would have opened one, or, for an agent that runs its own way,
//! such as Happy's, which needs no terminal host, in a process of its own.
use std::process::Stdio;

use anyhow::{Context as _, Result};

use crate::client::queue;
use crate::{
    client::connection::CommandPaths,
    config::Effective,
    env,
    model::{QueueEntry, QueuedPlan},
    protocol::ConfigTarget,
    start_plan::AddPlan,
    workers::{Worker, internal_command},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// Start the admitted launch `entry`: a chat's in its place, else the add
/// worker of the workspace it opens.
pub(super) async fn start(paths: &CommandPaths, entry: &QueueEntryId) -> Result<()> {
    let queued = queue::plan(paths, entry.clone()).await?;
    if let Some(chat) = crate::start_plan::ChatPlan::of(&queued.plan) {
        return super::chat::start(paths, entry, chat).await;
    }
    let (queued, plan) = add_plan(paths, entry, queued).await?;
    let QueueEntry { opens, name, .. } = &queued.entry;
    let opens = opens
        .as_ref()
        .context("the queued launch opens no workspace")?;
    start_admitted(paths, plan, opens, name).await
}

/// Start the add worker for an admitted launch's `plan` in the workspace
/// `name` it `opens`, in the background where the terminal that queued it
/// would have, or in no place for an agent that runs its own way, as the
/// workspace's settings, its worktree's file included, decide.
async fn start_admitted(
    paths: &CommandPaths,
    mut plan: AddPlan,
    opens: &SpaceId,
    name: &str,
) -> Result<()> {
    let target = ConfigTarget::Space(SpaceSelector::id(opens));
    let settings = crate::settings::of(paths, target).await?;
    if runs_its_own_way(&settings, &plan) {
        return run_add_worker(paths, &plan).await;
    }
    crate::places::queued::start_add(paths, &settings, &mut plan, name).await
}

/// The plan of the starting launch `entry`, refusing one that no longer
/// opens the workspace it was queued for.
pub async fn plan(paths: &CommandPaths, entry: &QueueEntryId) -> Result<(QueuedPlan, AddPlan)> {
    let queued = queue::plan(paths, entry.clone()).await?;
    add_plan(paths, entry, queued).await
}

/// [`plan`] of the starting launch `entry` from the plan `queued` with it.
async fn add_plan(
    paths: &CommandPaths,
    entry: &QueueEntryId,
    queued: QueuedPlan,
) -> Result<(QueuedPlan, AddPlan)> {
    let mut plan: AddPlan = serde_json::from_str(&queued.plan)?;
    plan.queue_entry = Some(entry.clone());
    let opens = (queued.entry.opens.as_ref()).context("the queued launch opens no workspace")?;
    let workspace = crate::client::workspaces::inspect(paths, SpaceSelector::id(opens))
        .await?
        .workspace;
    anyhow::ensure!(
        plan.opens(&workspace),
        "the queued launch does not open workspace {}",
        workspace.name
    );
    Ok((queued, plan))
}

/// Whether `plan`'s agent runs its own way under `settings`, leaving the
/// process that launches it at once.
fn runs_its_own_way(settings: &Effective, plan: &AddPlan) -> bool {
    (plan.agent_name()).is_some_and(|agent| {
        crate::agents::agent_integration(settings, &agent)
            .detached()
            .is_some()
    })
}

/// Run the add worker for `plan` in no place, as a process of this one
/// that ends once its agent runs its own way; nothing reads its output.
/// The worker frees the entry's slot itself when the launch fails, as it
/// does in a place, and says why.
async fn run_add_worker(paths: &CommandPaths, plan: &AddPlan) -> Result<()> {
    let worker = Worker::Add {
        host: None,
        close_when_done: false,
        focused: false,
    };
    let argv = internal_command(paths, false, worker)?;
    let status = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env(env::ADD_PLAN, serde_json::to_string(plan)?)
        .stdin(Stdio::null())
        .status()
        .await
        .context("run the add worker")?;
    anyhow::ensure!(status.success(), "the workspace's start failed ({status})");
    Ok(())
}
