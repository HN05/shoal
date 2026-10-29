//! A workspace's status: what the daemon reports and the sessions the CLI finds.
mod render;

use anyhow::{Context as _, Result, ensure};

use self::render::render_status;
use crate::{
    cli::{
        client::{self, request},
        context::Context,
        terminal,
        ui::{self, Fallback},
    },
    forge::{IssueInput, items::link::Selection},
    model::{Chat, Workspace, WorkspaceStatus, WorkspaceTarget},
    protocol::{ConfigTarget, WorkspaceMethod},
};

pub(in crate::cli::commands) async fn status(
    ctx: &Context,
    workspace: Option<String>,
    item: Option<String>,
) -> Result<i32> {
    let Some(target) = status_target(workspace.as_deref(), item)? else {
        let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
        let status = StatusView::request(ctx, workspace).await?;
        ctx.show(&status, |status| render_status(status, ctx.json))?;
        return Ok(0);
    };
    let workspaces =
        request::<Vec<Workspace>>(&ctx.paths, WorkspaceMethod::FindWorkspaces { target }).await?;
    let mut statuses = Vec::new();
    for workspace in workspaces {
        statuses.push(StatusView::request(ctx, workspace.id).await?);
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

/// The daemon's workspace status with the agent sessions the CLI finds.
#[derive(serde::Serialize)]
struct StatusView {
    #[serde(flatten)]
    status: WorkspaceStatus,
    sessions: Vec<terminal::Session>,
    /// Why sessions could not be listed; status still reports the rest.
    #[serde(skip_serializing_if = "Option::is_none")]
    sessions_error: Option<String>,
    /// How text names the workspace's chat; JSON gives its ID.
    #[serde(skip)]
    chat: Option<String>,
}

impl StatusView {
    async fn request(ctx: &Context, workspace: String) -> Result<Self> {
        // Listing chats first ends finished ones, returning their workspaces to the user.
        let chats = client::chats(&ctx.paths).await?;
        let status =
            request::<WorkspaceStatus>(&ctx.paths, WorkspaceMethod::WorkspaceStatus { workspace })
                .await?;
        let chat = chat_name(&chats, &status.inspection.workspace);
        let places: Vec<_> = status
            .inspection
            .executions
            .iter()
            .filter_map(|execution| execution.origin.as_ref())
            .chain(&status.inspection.places)
            .collect();
        let (sessions, sessions_error) =
            match sessions(ctx, &status.inspection.workspace, &places).await {
                Ok(sessions) => (sessions, None),
                Err(error) => (Vec::new(), Some(format!("{error:#}"))),
            };
        Ok(Self {
            status,
            sessions,
            sessions_error,
            chat,
        })
    }
}

/// The agent sessions of `workspace`, in the hosts its settings enable and
/// at the `places` its executions were recorded running in.
async fn sessions(
    ctx: &Context,
    workspace: &Workspace,
    places: &[&crate::sessions::Origin],
) -> Result<Vec<terminal::Session>> {
    let target = ConfigTarget::Workspace(workspace.id.clone());
    let settings = client::settings(&ctx.paths, target).await?;
    terminal::sessions(&settings, places).await
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

/// Items and resources find the workspaces that link or hold them; anything
/// else selects a workspace.
fn status_target(first: Option<&str>, item: Option<String>) -> Result<Option<WorkspaceTarget>> {
    match first {
        Some("resource") => Ok(Some(WorkspaceTarget::Resource {
            name: item.context("resource needs a pool or member name")?,
        })),
        Some(first)
            if matches!(first, "pr" | "issue") || IssueInput::parse(first) == IssueInput::Url =>
        {
            let selection = Selection::parse(Some(first.to_owned()), item)?;
            Ok(Some(WorkspaceTarget::Item {
                kind: selection.kind.context("expected an issue or PR")?,
                input: selection
                    .input
                    .with_context(|| format!("{first} needs a number or URL"))?,
            }))
        }
        _ => {
            ensure!(
                item.is_none(),
                "put pr, issue or resource before a number, URL or name"
            );
            Ok(None)
        }
    }
}
