//! Recording a space's work as done, and withdrawing that.
use anyhow::Result;

use crate::client::assignments;
use crate::client::output::workspaces::Withdrawn;
use crate::{
    cli::{
        context::Context,
        ui::{self, Fallback},
    },
    model::{Completion, PresentSpace},
};

/// The space `space` names, else the current chat or workspace, else a
/// picked one: a running chat's ID, or else a workspace.
async fn selected(ctx: &Context, space: Option<String>) -> Result<Selected> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(
        match crate::client::workspaces::present_space(&ctx.paths, &space).await? {
            Some(PresentSpace::Chat(chat)) => Selected::Chat(chat.id),
            _ => Selected::Workspace(space),
        },
    )
}

enum Selected {
    Chat(String),
    Workspace(String),
}

pub(in crate::cli::commands) async fn undone(ctx: &Context, space: Option<String>) -> Result<i32> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let withdrawn = assignments::undo_done(&ctx.paths, space).await?;
    ctx.emit(
        "Completion withdrawn; run shoal done when finished.",
        Withdrawn { withdrawn },
    )?;
    Ok(0)
}

/// How `shoal done` records the completion, and what it leaves running.
#[derive(Default)]
pub(in crate::cli::commands) struct Done {
    /// Request cleanup even when `[cleanup.done] enabled` is false.
    pub cleanup: bool,
    /// Another branch has HEAD's work under other commits.
    pub integrated: bool,
    pub keep_demos: bool,
    /// The files to hand back, checked before anything is picked or
    /// stopped.
    pub results: Vec<crate::client::files::NewFile>,
}

/// Stop the workspace's demos unless `keep_demos`, then record completion;
/// a chat only records it, since it has no branch or demos.
pub(in crate::cli::commands) async fn done(
    ctx: &Context,
    space: Option<String>,
    options: Done,
) -> Result<i32> {
    let selected = selected(ctx, space).await?;
    let space = match &selected {
        Selected::Chat(chat) => {
            anyhow::ensure!(
                !options.integrated && !options.keep_demos,
                "a chat has no branch or demos; --integrated and --keep-demos apply to workspaces"
            );
            chat.clone()
        }
        Selected::Workspace(workspace) => {
            // Before completion, whose cleanup may remove the workspace at once.
            if !options.keep_demos {
                stop_demos(ctx, workspace).await?;
            }
            workspace.clone()
        }
    };
    let completion = assignments::mark_done(
        &ctx.paths,
        space,
        options.cleanup,
        options.integrated,
        options.results,
        crate::env::chat(),
    )
    .await?;
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

async fn stop_demos(ctx: &Context, workspace: &str) -> Result<()> {
    crate::client::demos::stop(&ctx.paths, workspace.to_owned(), None).await
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
