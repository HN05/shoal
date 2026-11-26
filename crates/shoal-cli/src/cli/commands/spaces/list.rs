//! `shoal list --print`.
use anyhow::Result;
use shoal_protocol::hub::directory::{HubView, Listed};

use crate::cli::{
    commands::{listing, queued_agents},
    context::Context,
    everywhere,
    output::{self, Palette},
    ui,
};
use crate::client::output::spaces::ListedSpace;
use crate::model::{Chat, SpaceKind, Workspace};

pub(in crate::cli::commands) async fn list(
    ctx: &Context,
    filter: &listing::ListFilter,
    chats: bool,
) -> Result<i32> {
    let everywhere = everywhere::spaces(ctx).await?;
    let (spaces, view) = (everywhere.here, everywhere.elsewhere);
    let kept_chats = kept_chats(spaces.chats(), filter);
    let (workspaces, statuses) = match chats {
        true => (Vec::new(), ui::Statuses::default()),
        false => kept_workspaces(ctx, spaces.into_workspaces(), filter).await?,
    };
    let elsewhere = kept_elsewhere(view.as_ref(), filter, chats);
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let listing = ui::Listing {
        workspaces: &workspaces,
        chats: &kept_chats,
        elsewhere: (view.as_ref()).map(|view| ui::Elsewhere {
            spaces: elsewhere.clone(),
            machines: ui::Machines::new(view),
        }),
    };
    let rows = ui::space_tree_rows(
        listing,
        &statuses,
        &ui::repository_choices(repositories).await,
        Palette::stdout(ctx.json),
        output::stdout_width(ctx.json),
    );
    let listed: Vec<ListedSpace> = (workspaces.into_iter())
        .map(|workspace| ListedSpace::Here(workspace.into()))
        .chain(
            kept_chats
                .into_iter()
                .map(|chat| ListedSpace::Here(chat.into())),
        )
        .chain(elsewhere.into_iter().cloned().map(ListedSpace::Elsewhere))
        .collect();
    ctx.show(&listed, |_| {
        for row in rows {
            outln!("{row}");
        }
    })?;
    Ok(0)
}

fn kept_chats(mut chats: Vec<Chat>, filter: &listing::ListFilter) -> Vec<Chat> {
    chats.retain(|chat| filter.keeps_chat(chat));
    chats
}

fn kept_elsewhere<'a>(
    view: Option<&'a HubView>,
    filter: &listing::ListFilter,
    chats: bool,
) -> Vec<&'a Listed> {
    let spaces = view.into_iter().flat_map(|view| &view.directory.spaces);
    spaces
        .filter(|remote| !chats || remote.space.kind == SpaceKind::Chat)
        .filter(|remote| filter.keeps_remote(remote))
        .collect()
}

async fn kept_workspaces(
    ctx: &Context,
    mut workspaces: Vec<Workspace>,
    filter: &listing::ListFilter,
) -> Result<(Vec<Workspace>, ui::Statuses)> {
    let mut statuses = ui::Statuses {
        stopped: ui::stopped_spaces(&ctx.paths).await,
        queued: queued_agents::entries(ctx).await?,
    };
    workspaces.retain(|workspace| filter.keeps_workspace(workspace, &statuses.shown(workspace)));
    (statuses.queued).retain(|entry| filter.keeps_listed_entry(entry, &workspaces));
    Ok((workspaces, statuses))
}
