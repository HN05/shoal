//! Moving the shell into a workspace.
use anyhow::{Result, ensure};

use crate::cli::{
    context::Context,
    recent, shell,
    ui::{self, Acting},
};
use crate::client::output::workspaces::Directory;
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn cd(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    let workspace = match workspace {
        Some(workspace) => ui::explicit_workspace(ctx, workspace).await?,
        // The shell goes on in the worktree on this machine, as a command
        // `shoal exec` runs there does, which the inspection it reads
        // the path with would reach on another.
        None => SpaceSelector::Id(
            ui::workspace_picker(
                ctx,
                &Acting::request(shoal_workers::execution::running_command),
            )
            .await?,
        ),
    };
    let inspection = crate::client::workspaces::inspect(workspace)
        .send(&ctx.paths)
        .await?;
    ensure!(
        inspection.workspace.path.is_dir(),
        "workspace directory is missing"
    );
    recent::record_move(ctx, &SpaceSelector::id(&inspection.workspace.id)).await;
    navigate(ctx, &inspection.workspace.path)?;
    Ok(0)
}

/// Report the destination and ask the shell wrapper to change directory.
fn navigate(ctx: &Context, path: &std::path::Path) -> Result<()> {
    let directory = Directory {
        path: path.to_owned(),
    };
    ctx.emit(&path.display().to_string(), directory)?;
    shell::navigate(ctx, path)
}
