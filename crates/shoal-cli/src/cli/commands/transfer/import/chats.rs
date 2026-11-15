//! The chats a package carries with its space, which the import opens again
//! after it, under their IDs, stopped or with their agents' restores queued.
use anyhow::{Context as _, Result, bail};

use crate::cli::commands::restore::reopen::{ReopenedExt, reopened_chat};
use crate::client::agent::AgentSessionId;
use crate::{
    cli::{
        agents::{self, Existing, SessionHome},
        context::Context,
        ui,
    },
    model::{RemovedSpace, RestoredSpace},
    transfer::{CarriedChatExt, CarriedSessionFile, Manifest, ManifestExt},
};

/// A chat a package carries, as it opens here: the repository it goes to,
/// none for a chat without one, and its removal record, with the sessions
/// its agent keeps here.
pub(super) struct ChatImport {
    repository: Option<crate::repository::Selector>,
    chat: RemovedSpace,
}

impl ChatImport {
    /// The sessions its agent continues here.
    pub(super) fn sessions(&self) -> impl Iterator<Item = &AgentSessionId> {
        let stopped = self.chat.agents.iter().map(|agent| &agent.record.session);
        let last = self.chat.last_sessions.iter().map(|last| &last.session);
        stopped.chain(last).flatten()
    }
}

/// The chats `manifest` carries, as they open here, each in the repository
/// [`destination`] names, keeping their sessions as `sessions` says. One
/// that has no destination stays out, with a warning; the exporting machine
/// keeps its removal record.
pub(super) fn chats_here(
    manifest: &Manifest,
    repository: Option<&crate::repository::Selector>,
    sessions: Option<bool>,
) -> Vec<ChatImport> {
    let machine = crate::process::machine::id();
    let mut chats = Vec::new();
    for carried in &manifest.chats {
        let repository = match destination(carried, repository) {
            Ok(repository) => repository,
            Err(error) => {
                eprintln!(
                    "warning: chat {} stays on the machine that exported it: {error:#}",
                    carried.chat.name
                );
                continue;
            }
        };
        let kept = manifest.chat_sessions_on(carried, machine.as_deref(), sessions);
        chats.push(ChatImport {
            repository,
            chat: carried.chat_with(kept),
        });
    }
    chats
}

/// The repository the `carried` chat goes to: none for a chat without one,
/// else `repository`, where its package goes, or below a chat without one
/// the repository with its own origin.
fn destination(
    carried: &crate::transfer::CarriedChat,
    repository: Option<&crate::repository::Selector>,
) -> Result<Option<crate::repository::Selector>> {
    if carried.chat.repository_id.is_none() {
        return Ok(None);
    }
    if let Some(repository) = repository {
        return Ok(Some(repository.clone()));
    }
    let origin = (carried.remote_url.clone()).context("its repository has no origin")?;
    Ok(Some(ui::repository_selector(origin)?))
}

/// Open each of `chats` again, with the session files among `files` its
/// agent keeps, queueing their agents' restores when `restore`; a chat that
/// cannot open is a warning, since its space opened.
pub(super) async fn open(
    ctx: &Context,
    chats: &[ChatImport],
    files: &[CarriedSessionFile],
    restore: bool,
) {
    for import in chats {
        if let Err(error) = open_one(ctx, import, files, restore).await {
            eprintln!(
                "warning: cannot open chat {} again: {error:#}",
                import.chat.name
            );
        }
    }
}

/// Open `import` again as [`open`] does, reporting it.
async fn open_one(
    ctx: &Context,
    import: &ChatImport,
    files: &[CarriedSessionFile],
    restore: bool,
) -> Result<()> {
    let (repository, chat) = (import.repository.clone(), import.chat.clone());
    let restored = crate::client::transfer::import_chat(&ctx.paths, repository, chat).await?;
    let RestoredSpace::Chat(restored) = restored else {
        bail!("the import opened a workspace");
    };
    let kept: Vec<_> = import.sessions().collect();
    let own: Vec<CarriedSessionFile> = (files.iter())
        .filter(|file| kept.contains(&&file.file.session))
        .cloned()
        .collect();
    let home = match &import.repository {
        Some(repository) => SessionHome::Repository(repository.clone()),
        None => SessionHome::Root,
    };
    agents::install_session_files(ctx, home, &own, Existing::Continued).await;
    let mut reopened = reopened_chat(ctx, *restored).await?;
    if restore {
        reopened.queue_agents(ctx).await;
    }
    if !ctx.json {
        outln!("{}", reopened.message(ctx, "Imported"));
    }
    Ok(())
}
