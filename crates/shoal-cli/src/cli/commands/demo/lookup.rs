//! Finding the workspace and the running demos a demo command acts on.
use anyhow::{Result, bail};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    demos::{self, Running},
    model::Workspace,
};

/// The running demo `name` of the workspace given or chosen.
pub(super) async fn named(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: &str,
) -> Result<Running> {
    let id = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = crate::client::workspaces::inspect(&ctx.paths, id.clone())
        .await?
        .workspace;
    find(
        running(ctx, Some(&workspace)).await?,
        name,
        Some(&workspace),
    )
}

pub(super) fn find(
    demos: Vec<Running>,
    name: &str,
    workspace: Option<&Workspace>,
) -> Result<Running> {
    let mut matching: Vec<_> = demos
        .into_iter()
        .filter(|demo| demo.launch.name == name)
        .collect();
    match (matching.len(), workspace) {
        (0, Some(workspace)) => bail!("{} has no running demo {name}", workspace.name),
        (0, None) => bail!("no demo {name} is running"),
        (1, _) => Ok(matching.remove(0)),
        _ => bail!("several workspaces run a demo {name}; pass --workspace"),
    }
}

/// The workspace whose demos to show: the one named, the current one, a
/// workspace process's own, or `None` for every workspace.
pub(super) async fn scope(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<Option<Workspace>> {
    let id = match workspace {
        Some(selector) => Some(ui::explicit_workspace(ctx, selector).await?),
        // A workspace process selects as start and stop do: the directory's
        // workspace, which may be a child of its own, otherwise its own.
        None if crate::env::is_scoped() => {
            Some(ui::select_workspace(ctx, None, Fallback::CurrentDirectoryOnly).await?)
        }
        None => {
            let workspaces = crate::client::workspaces::list(&ctx.paths).await?;
            ui::current_workspace(&workspaces)?.map(|workspace| SpaceSelector::id(&workspace.id))
        }
    };
    match id {
        Some(id) => Ok(Some(
            crate::client::workspaces::inspect(&ctx.paths, id)
                .await?
                .workspace,
        )),
        None => Ok(None),
    }
}

pub(super) async fn running(ctx: &Context, workspace: Option<&Workspace>) -> Result<Vec<Running>> {
    demos::running(
        &ctx.paths,
        workspace.map(|workspace| SpaceSelector::id(&workspace.id)),
    )
    .await
}
