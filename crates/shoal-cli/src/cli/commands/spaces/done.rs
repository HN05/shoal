//! Recording a space's work as done, and withdrawing that.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{assignments::Done, model::ByKind};

use crate::cli::{
    context::Context,
    ui::{self, Acting, Fallback},
};
use crate::client::assignments;
use crate::client::output::spaces::{self, Withdrawn};
use shoal_protocol::space_names::SpaceSelector;

/// The space `space` names, else the current chat or workspace, else a
/// picked one: a running chat's ID, or else a workspace.
async fn selected(ctx: &Context, space: Option<SpaceSelector>, done: &Done) -> Result<Selected> {
    let marking = {
        let done = done.clone();
        Acting::request(move |space| assignments::mark_done(space, done.clone()))
    };
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory, &marking).await?;
    Ok(
        match crate::client::spaces::present_space(&ctx.paths, &space)
            .await?
            .map(crate::model::Space::by_kind)
        {
            Some(ByKind::Chat(chat)) => Selected::Chat(chat.id),
            _ => Selected::Workspace(space),
        },
    )
}

enum Selected {
    Chat(SpaceId),
    Workspace(SpaceSelector),
}

pub(in crate::cli::commands) async fn undone(
    ctx: &Context,
    space: Option<SpaceSelector>,
) -> Result<i32> {
    let undoing = Acting::request(assignments::undo_done);
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory, &undoing).await?;
    let withdrawn = assignments::undo_done(space.clone())
        .send(&ctx.paths)
        .await?;
    ctx.emit(
        "Completion withdrawn; run shoal done when finished.",
        Withdrawn { withdrawn },
    )?;
    Ok(0)
}

/// Record `done`, which stops a workspace's demos unless it keeps them.
pub(in crate::cli::commands) async fn done(
    ctx: &Context,
    space: Option<SpaceSelector>,
    done: Done,
) -> Result<i32> {
    let selected = selected(ctx, space, &done).await?;
    let space = match &selected {
        Selected::Chat(chat) => SpaceSelector::id(chat),
        Selected::Workspace(workspace) => workspace.clone(),
    };
    let completion = assignments::mark_done(space, done).send(&ctx.paths).await?;
    let message = match selected {
        Selected::Chat(_) if completion.cleanup => {
            "Chat marked done; cleanup requested. Its agent may stop and the chat may be removed."
                .to_owned()
        }
        Selected::Chat(_) => "Chat marked done; no cleanup requested.".to_owned(),
        Selected::Workspace(_) => spaces::workspace_done(&completion),
    };
    ctx.emit(&message, &completion)?;
    Ok(0)
}
