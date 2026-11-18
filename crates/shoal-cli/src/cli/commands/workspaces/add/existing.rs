//! `shoal start --in` in a mode that starts in a workspace: the agent of an
//! existing workspace, started as a new workspace's is, with no space
//! created. The workspace keeps its place in the tree, its name and links.
use std::ffi::OsString;

use anyhow::{Result, ensure};

use super::{
    AddOptions,
    agent::{AddSettings, choose_add_agent, resolve_add_agent},
    launch::{check_named_blockers, launch_worker},
    mode::choose_add_mode,
    target::AddTarget,
};
use crate::{agent::Agent, cli::context::Context};
use shoal_protocol::space_names::SpaceSelector;

/// Start `agent`, else on a terminal a picked one, as the worker of the
/// existing `workspace`, with `args` forwarded as any start forwards them,
/// else on the workspace's issue, through the daemon, here where it runs in
/// this caller. The daemon refuses it while the workspace has an agent of
/// its own. Without an agent the caller's shell enters the workspace.
pub(in crate::cli::commands) async fn start_worker(
    ctx: &Context,
    workspace: SpaceSelector,
    agent: Option<Agent>,
    args: Vec<OsString>,
    mut options: AddOptions,
) -> Result<i32> {
    // Without its token, the daemon would take a workspace process's start
    // for the user's.
    ensure!(
        crate::env::is_scoped() || !crate::env::inherits_scope(&ctx.paths.state),
        "workspace processes start agents only with their scope token"
    );
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace)
        .await?
        .workspace;
    let settings = AddSettings::workspace(ctx, &workspace.id);
    let target = AddTarget {
        selector: workspace.repository().into(),
        repositories: tokio::sync::OnceCell::new(),
    };
    let repository = Some(target.selector.clone());
    crate::cli::commands::permissions::ensure_held(ctx, repository, &options.narrowing).await?;
    check_named_blockers(ctx, &target, &options.blockers).await?;
    let mode = choose_add_mode(ctx, &settings, options.mode.take()).await?;
    let agent = choose_add_agent(&settings, agent).await?;
    let agent = resolve_add_agent(ctx, &settings, agent, options.model.clone()).await?;
    let picked = (mode, agent);
    launch_worker(ctx, &workspace, picked, args, (options, &settings)).await
}
