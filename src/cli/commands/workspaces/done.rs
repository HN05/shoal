//! Recording a workspace's assignment as done, and withdrawing that.
use anyhow::Result;

use crate::client::output::workspaces::Withdrawn;
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        ui::{self, Fallback},
    },
    model::{Completion, PresentSpace},
    protocol::{AssignmentMethod, ChatMethod, DemoMethod},
};

/// The space `space` names, else the current chat or workspace, else a
/// picked one: a running chat's ID, or else a workspace.
async fn selected(ctx: &Context, space: Option<String>) -> Result<Selected> {
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(match client::present_space(&ctx.paths, &space).await? {
        Some(PresentSpace::Chat(chat)) => Selected::Chat(chat.id),
        _ => Selected::Workspace(space),
    })
}

enum Selected {
    Chat(String),
    Workspace(String),
}

pub(in crate::cli::commands) async fn undone(ctx: &Context, space: Option<String>) -> Result<i32> {
    let withdrawn: Completion = match selected(ctx, space).await? {
        Selected::Chat(chat) => request(&ctx.paths, ChatMethod::ChatUndone { chat }).await?,
        Selected::Workspace(workspace) => {
            request(&ctx.paths, AssignmentMethod::WorkspaceUndone { workspace }).await?
        }
    };
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
    pub results: Vec<crate::daemon::workspace_files::NewFile>,
}

/// Stop the workspace's demos unless `keep_demos`, then record completion;
/// a chat only records it, since it has no cleanup, branch or demos.
pub(in crate::cli::commands) async fn done(
    ctx: &Context,
    space: Option<String>,
    options: Done,
) -> Result<i32> {
    let workspace = match selected(ctx, space).await? {
        Selected::Chat(chat) => {
            anyhow::ensure!(
                !options.cleanup && !options.integrated && !options.keep_demos,
                "a chat has no cleanup, branch or demos; --cleanup, --integrated and --keep-demos apply to workspaces"
            );
            let method = ChatMethod::ChatDone {
                chat,
                results: options.results,
                sender_chat: crate::env::chat(),
            };
            let completion: Completion = request(&ctx.paths, method).await?;
            ctx.emit("Chat marked done; its agent keeps running.", &completion)?;
            return Ok(0);
        }
        Selected::Workspace(workspace) => workspace,
    };
    // Before completion, whose cleanup may remove the workspace at once.
    if !options.keep_demos {
        request::<()>(
            &ctx.paths,
            DemoMethod::StopDemos {
                workspace: workspace.clone(),
                name: None,
            },
        )
        .await?;
    }
    let completion: Completion = request(
        &ctx.paths,
        AssignmentMethod::WorkspaceDone {
            workspace,
            cleanup: options.cleanup,
            integrated: options.integrated,
            results: options.results,
        },
    )
    .await?;
    ctx.emit(&done_message(&completion), &completion)?;
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
