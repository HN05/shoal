//! A workspace's `[env]` variables, as shown and as tracked commands and agents receive them.
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::{Context as _, Result};
use serde_json::json;

use crate::{
    cli::{
        client::{self, request},
        context::Context,
        output::Style,
        ui::{self, Fallback},
    },
    model::Workspace,
    protocol::{ConfigTarget, WorkspaceMethod},
    repository,
};

pub(in crate::cli::commands) async fn environment(
    ctx: &Context,
    workspace: Option<String>,
    revoke: Option<String>,
) -> Result<i32> {
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    if let Some(token) = revoke {
        request::<()>(
            &ctx.paths,
            WorkspaceMethod::RevokeWorkspaceEnv { workspace, token },
        )
        .await?;
        ctx.emit_styled(
            Style::Success,
            "Workspace environment revoked",
            json!({"revoked": true}),
        )?;
    } else {
        let values = request::<std::collections::BTreeMap<String, String>>(
            &ctx.paths,
            WorkspaceMethod::WorkspaceEnv { workspace },
        )
        .await?;
        let values = crate::env::export_configured(values, std::env::var_os("PATH"))?;
        ctx.show(&values, |values| {
            for (name, value) in values {
                outln!("{name}={value}");
            }
        })?;
    }
    Ok(0)
}

/// The variables `[env]` adds to the workspace's tracked executions and hooks.
pub(in crate::cli) async fn configured_environment(
    ctx: &Context,
    workspace: &Workspace,
) -> Result<BTreeMap<String, OsString>> {
    let settings =
        client::settings(&ctx.paths, ConfigTarget::Workspace(workspace.id.clone())).await?;
    let repo = client::repositories(&ctx.paths)
        .await?
        .into_iter()
        .find(|repo| repo.id == workspace.repository_id)
        .context("workspace repository is missing")?;
    Ok(crate::env::render_configured_environment(
        &settings.env,
        workspace,
        repository::name(&repo),
        &client::inspect(&ctx.paths, workspace.id.clone())
            .await?
            .ports,
        // Only the daemon creates them; tools create a missing one.
        &crate::caches::locate(&repo, &settings.caches),
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
