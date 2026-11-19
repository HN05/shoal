//! The daemon's check of a settled launch, before a picker asks for what
//! it leaves open, and the existing workspace it answers with.
use anyhow::Result;
use shoal_protocol::launches::Launch;
use shoal_protocol::space_names::SpaceSelector;

use super::super::StartOptions;
use crate::{
    cli::{commands::start::DaemonReads, context::Context},
    config::Effective,
    launches::LaunchReads,
    model::Workspace,
    protocol::ConfigTarget,
};

/// Have the daemon refuse what it would refuse of `launch`, returning the
/// existing workspace the start goes into, if any. A start whose agent
/// leaves this caller and that no terminal asks anything of leaves its
/// refusals to the launch itself.
pub(super) async fn check(
    ctx: &Context,
    launch: &Launch,
    options: &StartOptions,
) -> Result<Option<Workspace>> {
    let leaves = super::super::leaves_caller(ctx, options.here) || launch.machine.is_some();
    if leaves && !ctx.interactive() {
        return Ok(None);
    }
    crate::client::launches::check(&ctx.paths, launch.clone()).await
}

/// The settings of `workspace`, which the start goes into.
pub(super) async fn workspace_settings(ctx: &Context, workspace: &Workspace) -> Result<Effective> {
    let target = ConfigTarget::Space(SpaceSelector::id(&workspace.id));
    DaemonReads(ctx).settings(target).await
}

/// Whether the issue of the `checked` workspace the start goes into
/// prompts its agent where the launch names nothing to start on.
pub(super) fn issue_linked(checked: Option<&Workspace>) -> bool {
    checked.is_some_and(|workspace| workspace.links.issue().is_some())
}
