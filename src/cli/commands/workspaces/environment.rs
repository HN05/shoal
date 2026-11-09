//! A workspace's `[env]` variables, as shown and as tracked commands and agents receive them.
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::{Context as _, Result};

use crate::client::output::workspaces::Revoked;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::Style,
        ui::{self, Fallback},
    },
    model::Workspace,
    protocol::ConfigTarget,
    repository,
};

pub(in crate::cli::commands) async fn environment(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    revoke: Option<crate::client::scope_tokens::ScopeToken>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    if let Some(token) = revoke {
        crate::client::workspaces::revoke_environment(&ctx.paths, workspace.clone(), token).await?;
        ctx.emit_styled(
            Style::Success,
            "Workspace environment revoked",
            Revoked { revoked: true },
        )?;
    } else {
        let values = crate::client::workspaces::environment(&ctx.paths, workspace.clone()).await?;
        let values = crate::env::export_configured(values, std::env::var_os("PATH"))?;
        ctx.show(&values, |values| {
            for (name, value) in values {
                outln!("{name}={value}");
            }
        })?;
    }
    Ok(0)
}

/// The variables `[env]` and the workspace's temporary directory add to its
/// tracked executions and hooks.
pub(in crate::cli) async fn configured_environment(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<BTreeMap<String, OsString>> {
    let settings = crate::client::workspaces::settings(
        &ctx.paths,
        ConfigTarget::Workspace(SpaceSelector::id(&workspace.id)),
    )
    .await?;
    let repo = crate::client::repositories::list(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == workspace.repository_id)
        .context("workspace repository is missing")?;
    Ok(crate::env::render_configured_environment(
        &settings.env,
        workspace,
        repository::name(&repo),
        &crate::client::workspaces::inspect(&ctx.paths, SpaceSelector::id(&workspace.id))
            .await?
            .ports,
        // Only the daemon creates them; tools create a missing cache, and
        // the daemon gives out hooks with the temporary directory created.
        &crate::caches::locate(&repo, &settings.caches),
        crate::tmp_dirs::locate(&repo, &workspace.id).as_deref(),
    ))
}

/// The workspace's [`configured_environment`] for finding its agents' files,
/// or none with a warning when it cannot be resolved.
pub(in crate::cli) async fn agent_environment(
    ctx: &Context,
    workspace: &Workspace,
) -> BTreeMap<String, OsString> {
    configured_environment(ctx, workspace)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "warning: cannot resolve the [env] of {} for its agents: {error:#}",
                workspace.name
            );
            Default::default()
        })
}
