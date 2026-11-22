//! CLI access to a stacked workspace's base workspace.
use anyhow::{Result, bail, ensure};

use crate::client::output::workspaces::Bases;
use crate::{
    cli::{
        args::changes::BaseCommand,
        context::Context,
        ui::{self, Acting, Fallback},
    },
    model::{Located, RefPart, SpaceRef, Workspace, WorkspaceRef},
    refusal::{Reason, Refusal},
};
use shoal_protocol::space_names::SpaceSelector;

pub(super) async fn run(
    ctx: &Context,
    command: Option<BaseCommand>,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let (workspace, change) = match command {
        Some(BaseCommand::Set { base, workspace }) => (workspace, Some(Change::Set(base))),
        Some(BaseCommand::Clear { workspace }) => (workspace, Some(Change::Clear)),
        None => (workspace, None),
    };
    let acting = match &change {
        Some(Change::Set(_)) => Acting::request(|space| {
            let base = Some(String::new());
            crate::client::workspaces::set_base(space, base)
        }),
        Some(Change::Clear) => {
            Acting::request(|space| crate::client::workspaces::set_base(space, None))
        }
        None => Acting::request(crate::client::workspaces::inspect),
    };
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&acting));
    let workspace = workspace.await?;
    let workspace = match change {
        Some(change) => {
            let base = match change {
                Change::Set(Some(base)) => Some(base),
                Change::Set(None) => Some(pick(ctx, &workspace).await?),
                Change::Clear => None,
            };
            (crate::client::workspaces::set_base(workspace.clone(), base))
                .send(&ctx.paths)
                .await?
        }
        // Its machine reads it, wherever it runs.
        None => {
            (crate::client::workspaces::inspect(workspace))
                .send(&ctx.paths)
                .await?
                .workspace
        }
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

/// The workspace `selector` selects among this machine's `workspaces`,
/// which a picked base is one of.
async fn find<'a>(
    ctx: &Context,
    workspaces: &'a [Workspace],
    selector: &SpaceSelector,
) -> Result<&'a Workspace> {
    if let Some(workspace) = (workspaces.iter()).find(|w| selector.selects(&w.id, w.number)) {
        return Ok(workspace);
    }
    match crate::client::spaces::locate(&ctx.paths, selector.clone()).await? {
        Located::Elsewhere { .. } => {
            bail!("{selector} runs on another machine, so name its base workspace")
        }
        Located::Here(_) => {
            Err(Refusal::new(Reason::NotFound, format!("no workspace {selector}")).into())
        }
    }
}

/// Choose another workspace of the same repository as the base.
async fn pick(ctx: &Context, selector: &SpaceSelector) -> Result<String> {
    let workspaces = crate::client::spaces::list(&ctx.paths)
        .await?
        .into_workspaces();
    let workspace = find(ctx, &workspaces, selector).await?;
    let candidates: Vec<_> = workspaces
        .iter()
        .filter(|w| w.repository_id == workspace.repository_id && w.id != workspace.id)
        // A stack builds on its base's branch.
        .filter(|w| w.branch().is_some())
        .cloned()
        .collect();
    ensure!(
        !candidates.is_empty(),
        "no other workspace in this repository"
    );
    // No request names the base it builds on.
    let choosing = Acting::here(crate::client::workspaces::inspect);
    Ok(
        ui::pick_listed_workspace(ctx, candidates, "base", &choosing)
            .await?
            .into(),
    )
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
    match &space.part {
        RefPart::Chat => format!("{} (chat)", space.name),
        RefPart::Workspace {
            branch: Some(branch),
        } if branch.as_str() != space.name.as_str() => format!("{} ({branch})", space.name),
        RefPart::Workspace { .. } => space.name.to_string(),
    }
}

pub(super) fn describe(workspace: &WorkspaceRef) -> String {
    match &workspace.branch {
        Some(branch) if branch.as_str() != workspace.name.as_str() => {
            format!("{} ({branch})", workspace.name)
        }
        _ => workspace.name.to_string(),
    }
}
