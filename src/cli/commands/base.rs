//! CLI access to a stacked workspace's base workspace.
use anyhow::{Context as _, Result, ensure};
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
    let (workspace, change) = match command {
        Some(BaseCommand::Set { base, workspace }) => (workspace, Some(Change::Set(base))),
        Some(BaseCommand::Clear { workspace }) => (workspace, Some(Change::Clear)),
        None => (workspace, None),
    };
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = match change {
        Some(change) => {
            let base = match change {
                Change::Set(Some(base)) => Some(base),
                Change::Set(None) => Some(pick(ctx, &workspace).await?),
                Change::Clear => None,
            };
            client::request::<Workspace>(&ctx.paths, Method::SetBaseWorkspace { workspace, base })
                .await?
        }
        None => find(&client::workspaces(&ctx.paths).await?, &workspace)?.clone(),
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

enum Change {
    /// Record this base, or one chosen when omitted.
    Set(Option<String>),
    Clear,
}

fn find<'a>(workspaces: &'a [Workspace], selector: &str) -> Result<&'a Workspace> {
    workspaces
        .iter()
        .find(|w| w.id == selector || w.name == selector)
        .with_context(|| format!("unknown workspace: {selector}"))
}

/// Choose another workspace of the same repository as the base.
async fn pick(ctx: &Context, selector: &str) -> Result<String> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    let workspace = find(&workspaces, selector)?;
    let candidates: Vec<_> = workspaces
        .iter()
        .filter(|w| w.repository_id == workspace.repository_id && w.id != workspace.id)
        .cloned()
        .collect();
    ensure!(
        !candidates.is_empty(),
        "no other workspace in this repository"
    );
    ui::pick_workspace(ctx, candidates)
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

pub(super) fn describe(workspace: &WorkspaceRef) -> String {
    if workspace.name == workspace.branch {
        workspace.name.clone()
    } else {
        format!("{} ({})", workspace.name, workspace.branch)
    }
}
