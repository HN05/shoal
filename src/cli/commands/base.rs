//! CLI access to a stacked workspace's base workspace.
use anyhow::{Result, ensure};

use crate::client::output::workspaces::Bases;
use crate::{
    cli::{
        args::changes::BaseCommand,
        client,
        context::Context,
        ui::{self, Fallback},
    },
    daemon::refusal::{Reason, Refusal},
    model::{SpaceKind, SpaceRef, Workspace, WorkspaceRef},
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
            crate::client::workspaces::set_base(&ctx.paths, workspace, base).await?
        }
        None => find(&client::workspaces(&ctx.paths).await?, &workspace)?.clone(),
    };
    let value = Bases {
        base_workspace: workspace.base_workspace.clone(),
        stacked_workspaces: workspace.stacked_workspaces.clone(),
    };
    ctx.show(&value, |_| {
        if workspace.base_workspace.is_none() {
            outln!("No base workspace");
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
        .ok_or_else(|| {
            Refusal::new(Reason::NotFound, format!("unknown workspace: {selector}")).into()
        })
}

/// Choose another workspace of the same repository as the base.
async fn pick(ctx: &Context, selector: &str) -> Result<String> {
    let workspaces = client::workspaces(&ctx.paths).await?;
    let workspace = find(&workspaces, selector)?;
    let candidates: Vec<_> = workspaces
        .iter()
        .filter(|w| w.repository_id == workspace.repository_id && w.id != workspace.id)
        // A stack builds on its base's branch.
        .filter(|w| w.branch.is_some())
        .cloned()
        .collect();
    ensure!(
        !candidates.is_empty(),
        "no other workspace in this repository"
    );
    ui::pick_workspace(ctx, candidates, "base").await
}

/// The base and stacked workspaces, when the workspace has any.
pub(super) fn render(workspace: &Workspace) {
    if let Some(base) = &workspace.base_workspace {
        outln!("Base workspace: {}", describe(base));
    }
    if !workspace.stacked_workspaces.is_empty() {
        let stacked: Vec<_> = workspace.stacked_workspaces.iter().map(describe).collect();
        outln!("Stacked workspaces: {}", stacked.join(", "));
    }
}

/// `space` by name, with a workspace's branch when it differs, or as a chat.
pub(super) fn describe_space(space: &SpaceRef) -> String {
    match (space.kind, &space.branch) {
        (SpaceKind::Chat, _) => format!("{} (chat)", space.name),
        (SpaceKind::Workspace, Some(branch)) if *branch != space.name => {
            format!("{} ({branch})", space.name)
        }
        (SpaceKind::Workspace, _) => space.name.clone(),
    }
}

pub(super) fn describe(workspace: &WorkspaceRef) -> String {
    match &workspace.branch {
        Some(branch) if *branch != workspace.name => format!("{} ({branch})", workspace.name),
        _ => workspace.name.clone(),
    }
}
