//! An admitted launch: the add worker that carries out its plan runs in a
//! background place where the terminal that queued it would have opened
//! one, or, for an agent that runs its own way, such as Happy's, which
//! needs no terminal host, in a process of its own.
use std::process::Stdio;

use anyhow::{Context as _, Result};

use crate::client::queue::{self, QueueEntryId};
use crate::client::{space_ids::SpaceId, space_names::SpaceSelector};
use crate::{
    config::Effective,
    env,
    model::{QueueEntry, QueuedPlan},
    paths::CommandPaths,
    protocol::ConfigTarget,
    start_plan::AddPlan,
    workers::{Worker, internal_command},
};

/// Start the add worker for the admitted launch `entry`.
pub(super) async fn start(paths: &CommandPaths, entry: &QueueEntryId) -> Result<()> {
    let (queued, plan) = plan(paths, entry).await?;
    let QueueEntry { opens, name, .. } = &queued.entry;
    start_admitted(paths, plan, opens.as_ref(), name).await
}

/// Start the add worker for an admitted launch's `plan`, the planned
/// workspace `name`, in the background where the terminal that queued it
/// would have, or in no place for an agent that runs its own way. The
/// settings of the existing workspace the launch `opens`, its worktree's
/// file included, decide which, or else the repository's.
pub async fn start_admitted(
    paths: &CommandPaths,
    mut plan: AddPlan,
    opens: Option<&SpaceId>,
    name: &str,
) -> Result<()> {
    let target = match opens {
        Some(workspace) => ConfigTarget::Space(SpaceSelector::id(workspace)),
        None => ConfigTarget::Repository(plan.repository.clone()),
    };
    let settings = crate::config::settings(paths, target).await?;
    if runs_its_own_way(&settings, &plan) {
        return run_add_worker(paths, &plan).await;
    }
    crate::terminal::queued::start_add(paths, &settings, &mut plan, name).await
}

/// The plan of the starting launch `entry`, building on the branch of the
/// done blocker it waited for, refusing one that no longer opens the
/// existing workspace it was queued for.
pub async fn plan(paths: &CommandPaths, entry: &QueueEntryId) -> Result<(QueuedPlan, AddPlan)> {
    let queued = queue::plan(paths, entry.clone()).await?;
    let mut plan: AddPlan = serde_json::from_str(&queued.plan)?;
    plan.queue_entry = Some(entry.clone());
    if let Some(workspace) = &queued.entry.opens {
        let workspace = crate::client::workspaces::inspect(paths, SpaceSelector::id(workspace))
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

/// Whether `plan`'s agent runs its own way under `settings`, leaving the
/// process that launches it at once.
fn runs_its_own_way(settings: &Effective, plan: &AddPlan) -> bool {
    (plan.agent_name()).is_some_and(|agent| {
        crate::agents::agent_integration(settings, &agent)
            .detached()
            .is_some()
    })
}

/// Run the add worker for `plan` in no place, as a process of this one that
/// ends once its agent runs its own way. The worker frees the entry's slot
/// itself when the launch fails, as it does in a place.
async fn run_add_worker(paths: &CommandPaths, plan: &AddPlan) -> Result<()> {
    let worker = Worker::Add {
        host: None,
        close_when_done: false,
        focused: false,
    };
    let argv = internal_command(paths, false, worker)?;
    tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env(env::ADD_PLAN, serde_json::to_string(plan)?)
        .stdin(Stdio::null())
        .status()
        .await
        .context("run the add worker")?;
    Ok(())
}
