//! A space's status: what the daemon reports and the sessions the CLI finds.
mod render;

use anyhow::{Context as _, Result, ensure};

use self::render::render_status;
use crate::client::output::status::StatusView;
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        terminal::{self, Recorded},
        ui::{self, Fallback},
    },
    forge::{IssueInput, items::link::Selection},
    model::{Chat, SpaceDetail, SpaceStatus, Workspace, WorkspaceTarget},
    protocol::{ConfigTarget, WorkspaceMethod},
};

pub(in crate::cli::commands) async fn status(
    ctx: &Context,
    space: Option<String>,
    item: Option<String>,
) -> Result<i32> {
    let Some(target) = status_target(space.as_deref(), item)? else {
        let space = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
        let status = status_view(ctx, space).await?;
        ctx.show(&status, |status| render_status(status, ctx.json))?;
        return Ok(0);
    };
    let workspaces =
        request::<Vec<Workspace>>(&ctx.paths, WorkspaceMethod::FindWorkspaces { target }).await?;
    let mut statuses = Vec::new();
    for workspace in workspaces {
        statuses.push(status_view(ctx, workspace.id).await?);
    }
    ctx.show(&statuses, |statuses| {
        for (index, status) in statuses.iter().enumerate() {
            if index > 0 {
                outln!();
            }
            render_status(status, ctx.json);
        }
    })?;
    Ok(0)
}

/// The status of `space`, with the sessions its terminal hosts keep.
async fn status_view(ctx: &Context, space: String) -> Result<StatusView> {
    // Listing chats first ends finished ones, returning their workspaces to the user.
    let chats = client::recorded_chats(&ctx.paths).await?;
    let status =
        *request::<Box<SpaceStatus>>(&ctx.paths, WorkspaceMethod::SpaceStatus { space }).await?;
    let chat = status
        .space
        .worktree()
        .and_then(|worktree| chat_name(&chats, &worktree.workspace));
    let (sessions, sessions_error) = match sessions(ctx, &status.space).await {
        Ok(sessions) => (sessions, None),
        Err(error) => (Vec::new(), Some(format!("{error:#}"))),
    };
    Ok(StatusView {
        status,
        sessions: sessions
            .into_iter()
            .map(terminal::Session::output)
            .collect(),
        sessions_error,
        chat,
    })
}

/// The agent sessions of `space`, in the hosts its settings enable and at
/// the places the daemon recorded it running in.
async fn sessions(ctx: &Context, space: &SpaceDetail) -> Result<Vec<terminal::Session>> {
    let (target, places) = match space {
        SpaceDetail::Workspace(status) => (
            ConfigTarget::Workspace(status.workspace.id.clone()),
            Recorded::workspace(&status.executions, &status.places),
        ),
        SpaceDetail::Chat { chat } => (
            ConfigTarget::Repository(chat.repository_id.clone().into()),
            Recorded::chat(chat),
        ),
    };
    let settings = client::settings(&ctx.paths, target).await?;
    terminal::sessions(&settings, &places).await
}

/// The listed chat that opened `workspace` with its agent, else its ID, which
/// scoped callers see without the chat.
fn chat_name(chats: &[Chat], workspace: &Workspace) -> Option<String> {
    let id = workspace.chat.as_ref()?;
    Some(match chats.iter().find(|chat| chat.id == *id) {
        Some(chat) => format!("{} ({})", chat.name, chat.agent),
        None => id.clone(),
    })
}

/// Linked items and resources find the workspaces that link or hold them;
/// anything else selects a workspace.
fn status_target(first: Option<&str>, item: Option<String>) -> Result<Option<WorkspaceTarget>> {
    match first {
        Some("resource") => Ok(Some(WorkspaceTarget::Resource {
            name: item.context("resource needs a pool or member name")?,
        })),
        Some(first)
            if matches!(first, "pr" | "issue" | "ci")
                || IssueInput::parse(first) == IssueInput::Url =>
        {
            let selection = Selection::parse(Some(first.to_owned()), item)?;
            Ok(Some(WorkspaceTarget::Item {
                kind: selection.kind.context("expected an issue, PR or CI run")?,
                input: selection
                    .input
                    .with_context(|| format!("{first} needs a number or URL"))?,
            }))
        }
        _ => {
            ensure!(
                item.is_none(),
                "put pr, issue, ci or resource before a number, URL or name"
            );
            Ok(None)
        }
    }
}
