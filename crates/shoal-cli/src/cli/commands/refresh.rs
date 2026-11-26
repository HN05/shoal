//! `shoal refresh`.
use anyhow::{Result, bail};

use crate::{
    cli::{
        context::Context,
        output::Style,
        ui::{self, Acting, Fallback},
    },
    model::Refreshed,
};
use shoal_protocol::space_names::SpaceSelector;

pub(in crate::cli::commands) async fn refresh(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
) -> Result<i32> {
    // A chat in a workspace's worktree reads it and changes nothing there.
    if ui::current_chat(&workspace).is_some() {
        bail!("a chat has no worktree to refresh");
    }
    let refreshing = refreshing();
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory(&refreshing));
    let workspace = workspace.await?;
    let mut refreshed = (crate::client::workspaces::refresh(workspace))
        .send(&ctx.paths)
        .await?;
    if refreshed.setup {
        let Some(set_up) = shoal_workers::preparation::setup_workspace(
            &ctx.paths,
            ctx,
            &refreshed.workspace,
            true,
        )
        .await?
        else {
            return Ok(1);
        };
        refreshed.workspace = set_up.finish(&ctx.paths, ctx).await?;
    }
    ctx.emit_styled(Style::Success, &summary(&refreshed), &refreshed)?;
    Ok(0)
}

fn summary(refreshed: &Refreshed) -> String {
    let (name, target) = (&refreshed.workspace.name, &refreshed.target);
    if refreshed.up_to_date {
        return format!("{name} is up to date with {target}");
    }
    let (from, to) = (refreshed.from.abbreviated(7), refreshed.to.abbreviated(7));
    match from == to {
        true => format!("Refreshed {name} with the files of {target}"),
        false => format!("Refreshed {name} to {target} at {to}, from {from}"),
    }
}

pub(super) fn refreshing() -> Acting {
    Acting::request(crate::client::workspaces::refresh)
}
