//! The daemon's check of a settled launch, before a picker asks for what it
//! leaves open.
use anyhow::Result;
use shoal_protocol::launches::Launch;
use shoal_protocol::space_names::SpaceSelector;

use super::super::StartOptions;
use crate::{
    cli::{commands::start::target_settings, context::Context},
    config::Effective,
    model::Workspace,
    protocol::ConfigTarget,
};

/// A start whose agent leaves this caller and that no terminal asks anything of
/// leaves its refusals to the launch itself.
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

pub(super) async fn workspace_settings(ctx: &Context, workspace: &Workspace) -> Result<Effective> {
    let target = ConfigTarget::Space(SpaceSelector::id(&workspace.id));
    target_settings(ctx, target).await
}

/// Whether the issue of the `checked` workspace the start goes into
/// prompts its agent where the launch names nothing to start on.
pub(super) fn issue_linked(checked: Option<&Workspace>) -> bool {
    checked.is_some_and(|workspace| workspace.links.issue().is_some())
}
