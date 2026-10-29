//! Persistent agent sessions run in terminal hosts; attaching hands this
//! terminal to one of a workspace's or a running chat's.
use anyhow::{Context as _, Result, bail, ensure};

use crate::{
    cli::{
        agents, client,
        context::Context,
        recent,
        terminal::{self, Owner, OwnerKind},
        ui::{self, Fallback, NoCurrentWorkspace},
    },
    model::{Chat, Workspace},
    sessions::Origin,
};

pub(super) async fn attach(
    ctx: &Context,
    target: Option<String>,
    session: Option<String>,
) -> Result<i32> {
    ensure!(
        !crate::env::inherits_scope(&ctx.paths.state),
        "attach to sessions outside scoped executions"
    );
    ensure!(ctx.interactive(), "attach requires a terminal");
    let target = Target::resolve(ctx, target).await?;
    let sessions = terminal::sessions(target.owner(), &target.places()).await?;
    let session = match (session, sessions.as_slice()) {
        (Some(name), _) => sessions
            .iter()
            .find(|session| session.name == name)
            .with_context(|| format!("{} has no session {name}", target.name()))?,
        (None, []) => bail!("{}", target.without_sessions()),
        (None, [only]) => only,
        (None, several) => {
            let labels: Vec<_> = several.iter().map(terminal::Session::label).collect();
            let choices: Vec<_> = several
                .iter()
                .zip(&labels)
                .map(|(session, label)| (session, label.as_str()))
                .collect();
            ui::pick_choice(ctx, "Attach session> ", &choices)?
        }
    };
    terminal::attach(target.owner(), target.name(), session).await
}

/// What `shoal attach` returns to.
enum Target {
    /// A workspace with the places its executions were recorded running in.
    Workspace(Box<Workspace>, Vec<Origin>),
    Chat(Chat),
}

impl Target {
    /// The running chat or workspace `name` selects; without one, the
    /// current workspace, else the current checkout's only running chat,
    /// else a picker of the workspaces and chats with sessions.
    async fn resolve(ctx: &Context, name: Option<String>) -> Result<Self> {
        let chats = client::chats(&ctx.paths).await?;
        if let Some(name) = name {
            return match chats.into_iter().find(|chat| chat.name == name) {
                Some(chat) => Ok(Self::Chat(chat)),
                None => Self::workspace(ctx, Some(name), Fallback::CurrentDirectory).await,
            };
        }
        match Self::workspace(ctx, None, Fallback::CurrentDirectoryOnly).await {
            Err(error) if error.is::<NoCurrentWorkspace>() => {}
            current => return current,
        }
        if let Some(chat) = checkout_chat(ctx, &chats).await? {
            return Ok(Self::Chat(chat));
        }
        Self::picked(ctx, chats).await
    }

    async fn workspace(ctx: &Context, name: Option<String>, fallback: Fallback) -> Result<Self> {
        let id = ui::select_workspace(ctx, name, fallback).await?;
        Self::inspected(ctx, id).await
    }

    async fn inspected(ctx: &Context, id: String) -> Result<Self> {
        let inspection = client::inspect(&ctx.paths, id).await?;
        let places = inspection
            .executions
            .into_iter()
            .filter_map(|execution| execution.origin)
            .collect();
        Ok(Self::Workspace(Box::new(inspection.workspace), places))
    }

    /// A workspace or one of `chats`, each with an agent session, from a picker.
    async fn picked(ctx: &Context, chats: Vec<Chat>) -> Result<Self> {
        let attachable = terminal::session_owners(OwnerKind::Chat, &chat_places(&chats)).await?;
        let chats: Vec<_> = chats
            .into_iter()
            .filter(|chat| attachable.contains(&chat.id))
            .collect();
        let workspaces = client::workspaces(&ctx.paths).await?;
        let id = ui::pick_workspace_or_chat(ctx, workspaces, &chats).await?;
        if let Some(chat) = chats.into_iter().find(|chat| chat.id == id) {
            return Ok(Self::Chat(chat));
        }
        recent::record(ctx, &id).await;
        Self::inspected(ctx, id).await
    }

    fn owner(&self) -> Owner<'_> {
        match self {
            Self::Workspace(workspace, _) => Owner::Workspace(&workspace.id),
            Self::Chat(chat) => Owner::Chat(&chat.id),
        }
    }

    fn name(&self) -> &str {
        match self {
            Self::Workspace(workspace, _) => &workspace.name,
            Self::Chat(chat) => &chat.name,
        }
    }

    /// Where the daemon recorded the target running.
    fn places(&self) -> Vec<&Origin> {
        match self {
            Self::Workspace(_, places) => places.iter().collect(),
            Self::Chat(chat) => chat.origin.iter().collect(),
        }
    }

    /// Why there is nothing to attach to, and for a chat where it runs instead.
    fn without_sessions(&self) -> String {
        match self {
            Self::Workspace(workspace, _) => format!("{} has no agent sessions", workspace.name),
            Self::Chat(chat) => match agents::chat_place(&chat.agent) {
                Some(place) => {
                    format!("{} runs in {place}; it has no session to attach", chat.name)
                }
                None => format!(
                    "{} has no session to attach; it runs in the terminal that started it",
                    chat.name
                ),
            },
        }
    }
}

/// Where the daemon recorded `chats` running, by chat ID.
pub(in crate::cli) fn chat_places(chats: &[Chat]) -> Vec<(&str, &Origin)> {
    chats
        .iter()
        .filter_map(|chat| Some((chat.id.as_str(), chat.origin.as_ref()?)))
        .collect()
}

/// The only running chat of the registered checkout the current directory
/// is in.
async fn checkout_chat(ctx: &Context, chats: &[Chat]) -> Result<Option<Chat>> {
    let repos = client::repositories(&ctx.paths).await?;
    let Some(repo) = super::issues::current_checkout(&repos).await? else {
        return Ok(None);
    };
    let mut running = chats.iter().filter(|chat| chat.repository_id == repo.id);
    Ok(match (running.next(), running.next()) {
        (Some(only), None) => Some(only.clone()),
        _ => None,
    })
}
