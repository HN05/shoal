//! Recording a space's work as done, and withdrawing that.
use crate::client::space_ids::SpaceId;
use anyhow::Result;

use crate::client::assignments;
use crate::client::output::workspaces::Withdrawn;
use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    model::{Completion, PresentSpace},
};

/// The space `space` names, else the current chat or workspace, else a
/// picked one: a running chat's ID, or else a workspace.
async fn selected(ctx: &Context, space: Option<SpaceSelector>) -> Result<Selected> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(
        match crate::client::workspaces::present_space(&ctx.paths, &space).await? {
            Some(PresentSpace::Chat(chat)) => Selected::Chat(chat.id),
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
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let withdrawn = assignments::undo_done(&ctx.paths, space.clone()).await?;
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
    done: assignments::Done,
) -> Result<i32> {
    let selected = selected(ctx, space).await?;
    let space = match &selected {
        Selected::Chat(chat) => SpaceSelector::id(chat),
        Selected::Workspace(workspace) => workspace.clone(),
    };
    let completion = assignments::mark_done(&ctx.paths, space, done, crate::env::chat()).await?;
    let message = match selected {
        Selected::Chat(_) if completion.cleanup => {
            "Chat marked done; cleanup requested. Its agent may stop and the chat may be removed."
                .to_owned()
        }
        Selected::Chat(_) => "Chat marked done; no cleanup requested.".to_owned(),
        Selected::Workspace(_) => done_message(&completion),
    };
    ctx.emit(&message, &completion)?;
    Ok(0)
}

fn done_message(completion: &Completion) -> String {
    let marked = if completion.integrated {
        "Assignment marked done; integrated elsewhere"
    } else {
        "Assignment marked done"
    };
    if completion.cleanup {
        format!(
            "{marked}; cleanup requested. Tracked commands may stop and the workspace may be removed."
        )
    } else {
        format!("{marked}; no cleanup requested.")
    }
}
