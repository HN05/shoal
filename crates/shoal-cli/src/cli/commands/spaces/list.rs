//! Listing spaces, children below their parent, with the agents queued to
//! start, or only the chats.
use anyhow::Result;

use crate::cli::{
    commands::{listing, queued_agents},
    context::Context,
    output::{self, Palette},
    ui,
};
use crate::model::{Chat, Space, Workspace};

/// `shoal list --print`: the spaces and queued agents `filter` keeps, or
/// with `chats` only the chats it keeps.
pub(in crate::cli::commands) async fn list(
    ctx: &Context,
    filter: &listing::ListFilter,
    chats: bool,
) -> Result<i32> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    let kept_chats = kept_chats(spaces.chats(), filter);
    let (workspaces, statuses) = match chats {
        true => (Vec::new(), ui::Statuses::default()),
        false => kept_workspaces(ctx, spaces.into_workspaces(), filter).await?,
    };
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let rows = ui::workspace_tree_rows(
        &workspaces,
        &kept_chats,
        &statuses,
        &ui::repository_choices(repositories).await,
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    let listed: Vec<Space> = (workspaces.into_iter().map(Space::from))
        .chain(kept_chats.into_iter().map(Space::from))
        .collect();
    ctx.show(&listed, |_| {
        for row in rows {
            outln!("{row}");
        }
    })?;
    Ok(0)
}

/// The chats among `chats`, running or stopped, that `filter` keeps.
fn kept_chats(mut chats: Vec<Chat>, filter: &listing::ListFilter) -> Vec<Chat> {
    chats.retain(|chat| filter.keeps_chat(chat));
    chats
}

/// The workspaces among `workspaces` that `filter` keeps by the statuses
/// their rows show, with those statuses and the queued agents it keeps.
async fn kept_workspaces(
    ctx: &Context,
    mut workspaces: Vec<Workspace>,
    filter: &listing::ListFilter,
) -> Result<(Vec<Workspace>, ui::Statuses)> {
    let mut statuses = ui::Statuses {
        stopped: ui::stopped_workspaces(&ctx.paths).await,
        queued: queued_agents::entries(ctx).await?,
    };
    workspaces.retain(|workspace| filter.keeps_workspace(workspace, &statuses.shown(workspace)));
    (statuses.queued).retain(|entry| filter.keeps_listed_entry(entry, &workspaces));
    Ok((workspaces, statuses))
}
