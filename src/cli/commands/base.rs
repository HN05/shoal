//! CLI access to a stacked workspace's base workspace.
use anyhow::{Context as _, Result};
use serde_json::json;

use crate::{
    cli::{
        BaseCommand, client,
        context::Context,
        ui::{self, Fallback},
    },
    model::{Workspace, WorkspaceRef},
    protocol::Method,
};

pub(super) async fn run(
    ctx: &Context,
    command: Option<BaseCommand>,
    workspace: Option<String>,
) -> Result<i32> {
    let (workspace, base) = match command {
        Some(BaseCommand::Set { base, workspace }) => (workspace, Some(Some(base))),
        Some(BaseCommand::Clear { workspace }) => (workspace, Some(None)),
        None => (workspace, None),
    };
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = match base {
        Some(base) => {
            client::request::<Workspace>(&ctx.paths, Method::SetBaseWorkspace { workspace, base })
                .await?
        }
        None => client::workspaces(&ctx.paths)
            .await?
            .into_iter()
            .find(|w| w.id == workspace || w.name == workspace)
            .with_context(|| format!("unknown workspace: {workspace}"))?,
    };
    let value = json!({
        "base_workspace": workspace.base_workspace,
        "stacked_workspaces": workspace.stacked_workspaces,
    });
    ctx.show(&value, |_| {
        if workspace.base_workspace.is_none() {
            println!("No base workspace");
        }
        render(&workspace);
    })?;
    Ok(0)
}

/// The base and stacked workspaces, when the workspace has any.
pub(super) fn render(workspace: &Workspace) {
    if let Some(base) = &workspace.base_workspace {
        println!("Base workspace: {}", describe(base));
    }
    if !workspace.stacked_workspaces.is_empty() {
        let stacked: Vec<_> = workspace.stacked_workspaces.iter().map(describe).collect();
        println!("Stacked workspaces: {}", stacked.join(", "));
    }
}

fn describe(workspace: &WorkspaceRef) -> String {
    if workspace.name == workspace.branch {
        workspace.name.clone()
    } else {
        format!("{} ({})", workspace.name, workspace.branch)
    }
}
