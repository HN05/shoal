//! A workspace's `[env]` variables, as shown and as tracked commands and agents receive them.
use std::{collections::BTreeMap, ffi::OsString};

use anyhow::Result;

use crate::client::output::workspaces::Revoked;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        output::Style,
        ui::{self, Fallback},
    },
    model::Workspace,
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
        let inherited = std::env::var_os("PATH");
        let values =
            crate::client::workspaces::environment(&ctx.paths, workspace.clone(), inherited)
                .await?;
        ctx.show(&values, |values| {
            for (name, value) in values {
                outln!("{name}={value}");
            }
        })?;
    }
    Ok(0)
}

/// The workspace's [`crate::env::configured_environment`] for finding its agents' files,
/// or none with a warning when it cannot be resolved.
pub(in crate::cli) async fn agent_environment(
    ctx: &Context,
    workspace: &Workspace,
) -> BTreeMap<String, OsString> {
    crate::env::configured_environment(&ctx.paths, workspace)
        .await
        .unwrap_or_else(|error| {
            eprintln!(
                "warning: cannot resolve the [env] of {} for its agents: {error:#}",
                workspace.name
            );
            Default::default()
        })
}
