//! A detached start the daemon carries out: once this process has picked
//! what a terminal picks, it sends the start as one [`Launch`] and reports
//! what the daemon started.
use std::ffi::OsString;

use anyhow::Result;

use super::{
    AddOptions, agent::AddSettings, detached::ensure_detachable, finish::report_queued_in,
    plan::launch_args,
};
use crate::{cli::context::Context, model::Workspace, start_plan::ResolvedAddAgent};
use shoal_protocol::launches::{Launch, LaunchSource, Launched, QueuedWorker, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

/// Have the daemon start `agent` as the worker of `workspace`, in `mode`,
/// as `options` ask, with `args` as its task, and report it.
pub(super) async fn launch_worker(
    ctx: &Context,
    workspace: &Workspace,
    (mode, agent): (Option<crate::config::modes::ModeName>, ResolvedAddAgent),
    args: Vec<OsString>,
    (options, settings): (AddOptions, &AddSettings<'_>),
) -> Result<i32> {
    ensure_detachable(settings, agent.agent.as_ref(), &options).await?;
    let launch = Launch {
        in_workspace: Some(SpaceChoice::Space(SpaceSelector::id(&workspace.id))),
        mode,
        agent: agent.agent,
        model: agent.model,
        fork: options.fork,
        permissions: options.narrowing,
        blockers: options.blockers,
        now: options.now,
        task: launch_args(args)?,
        env: Some(crate::terminal::start_environment()),
        ..Default::default()
    };
    let launched = crate::client::launches::launch(&ctx.paths, launch, LaunchSource::Manual);
    let Launched::Worker(worker) = launched.await? else {
        anyhow::bail!("the daemon started the agent in another space than its workspace");
    };
    let QueuedWorker { workspace, entry } = *worker;
    ctx.show(&workspace, |_| ())?;
    match entry {
        Some(entry) => report_queued_in(ctx, &workspace, &entry),
        None => Ok(0),
    }
}
