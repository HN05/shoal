//! An admitted launch: its agent starts in the recorded space the launch
//! names, a chat's in a place of its own, and the add worker that starts a
//! workspace's runs in a background place where the terminal that
//! queued it would have opened one, or, for an agent that runs its own way,
//! such as Happy's, which needs no terminal host, in a process of its own.
use std::process::Stdio;

use anyhow::{Context as _, Result};

use crate::client::queue;
use crate::{
    client::connection::CommandPaths,
    config::Effective,
    env,
    model::{QueuedPlan, Workspace},
    protocol::ConfigTarget,
    start_plan::{AddPlan, QueuedStart},
    workers::{Worker, internal_command},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

/// Start the admitted launch `entry` in the space it names, whatever its
/// kind: a chat's agent in its place, a workspace's through the add worker.
pub(super) async fn start(paths: &CommandPaths, entry: &QueueEntryId) -> Result<()> {
    let queued = queue::plan(paths, entry.clone()).await?;
    let start = QueuedStart::of(&queued.plan)?;
    let space = queued_space(paths, &queued).await?;
    match space.into_workspace() {
        Ok(workspace) => {
            let plan = AddPlan::queued(&workspace, start, entry.clone())?;
            start_admitted(paths, plan, &workspace).await
        }
        Err(space) => {
            let chat = space.into_chat().ok().context("the space is no chat")?;
            super::chat::start(paths, entry, &chat, start.launch).await
        }
    }
}

/// The recorded space whose agent the launch `queued` starts.
async fn queued_space(paths: &CommandPaths, queued: &QueuedPlan) -> Result<crate::model::Space> {
    let space = (queued.entry.space()).context("the queued launch names no space")?;
    let spaces = crate::client::spaces::list(paths).await?;
    (spaces.find(&SpaceSelector::id(space)))
        .with_context(|| format!("the space {space} of the queued launch is gone"))
}

/// Start the add worker for an admitted launch's `plan` in `workspace`, in
/// the background where the terminal that queued it would have, or in no
/// place for an agent that runs its own way, as the workspace's settings,
/// its worktree's file included, decide.
async fn start_admitted(
    paths: &CommandPaths,
    mut plan: AddPlan,
    workspace: &Workspace,
) -> Result<()> {
    let target = ConfigTarget::Space(SpaceSelector::id(&workspace.id));
    let settings = crate::settings::of(paths, target).await?;
    if runs_its_own_way(&settings, &plan) {
        return run_add_worker(paths, &plan).await;
    }
    crate::places::queued::start_add(paths, &settings, &mut plan, workspace).await
}

/// The plan of the starting launch `entry`, which starts its agent in a
/// workspace.
pub async fn plan(paths: &CommandPaths, entry: &QueueEntryId) -> Result<(QueuedPlan, AddPlan)> {
    let queued = queue::plan(paths, entry.clone()).await?;
    let start = QueuedStart::of(&queued.plan)?;
    let workspace = (queued_space(paths, &queued).await?.into_workspace())
        .ok()
        .context("the queued launch starts no workspace's agent")?;
    let plan = AddPlan::queued(&workspace, start, entry.clone())?;
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
