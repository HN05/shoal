//! An admitted launch: the place worker that runs the agent of the space
//! it names, a workspace or a chat, runs in a background place where the
//! terminal that queued it would have opened one, or, for an agent that
//! runs its own way, such as Happy's, which needs no terminal host, in a
//! process of its own.
use std::process::Stdio;

use anyhow::{Context as _, Result};

use crate::client::queue;
use crate::{
    client::connection::CommandPaths,
    config::Effective,
    env,
    protocol::ConfigTarget,
    start_plan::{PlacePlan, QueuedStart},
    workers::{Worker, internal_command},
};
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::space_names::SpaceSelector;

/// Start the place worker for the admitted launch `entry` in the space it
/// names, whatever its kind, in the background where the terminal that
/// queued it would have, or in no place for an agent that runs its own
/// way, as the space's settings, a worktree's file included, decide.
pub(super) async fn start(paths: &CommandPaths, entry: &QueueEntryId) -> Result<()> {
    let queued = queue::plan(paths, entry.clone()).await?;
    let start = QueuedStart::of(&queued.plan)?;
    let space = (queued.entry.space()).context("the queued launch names no space")?;
    let spaces = crate::client::spaces::list(paths).await?;
    let space = (spaces.find(&SpaceSelector::id(space)))
        .with_context(|| format!("the space {space} of the queued launch is gone"))?;
    let target = ConfigTarget::Space(SpaceSelector::id(&space.id));
    let settings = crate::settings::of(paths, target).await?;
    let mut plan = PlacePlan::new(&space.id, Some(entry));
    if runs_its_own_way(&settings, &start) {
        return run_place_worker(paths, &plan).await;
    }
    crate::places::queued::start_place(paths, &settings, &mut plan, &space).await
}

/// Whether the agent `start` launches runs its own way under `settings`,
/// leaving the process that launches it at once.
fn runs_its_own_way(settings: &Effective, start: &QueuedStart) -> bool {
    let integration = crate::agents::agent_integration(settings, &start.launch.agent);
    integration.detached().is_some()
}

/// Run the place worker for `plan` in no place, as a process of this one
/// that ends once its agent runs its own way; nothing reads its output.
/// The worker frees the entry's slot itself when the launch fails, as it
/// does in a place, and says why.
async fn run_place_worker(paths: &CommandPaths, plan: &PlacePlan) -> Result<()> {
    let worker = Worker::Place {
        host: None,
        close_when_done: false,
        focused: false,
    };
    let argv = internal_command(paths, false, worker)?;
    let status = tokio::process::Command::new(&argv[0])
        .args(&argv[1..])
        .env(env::PLACE_PLAN, serde_json::to_string(plan)?)
        .stdin(Stdio::null())
        .status()
        .await
        .context("run the place worker")?;
    anyhow::ensure!(status.success(), "the space's start failed ({status})");
    Ok(())
}
