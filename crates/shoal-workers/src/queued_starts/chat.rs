//! An admitted chat launch: its chat starts in a background place where the
//! terminal that queued it would have opened one, and the chat's agent takes
//! the launch's slot.
use std::ffi::OsString;

use anyhow::{Context as _, Result};

use crate::{client::connection::CommandPaths, model::Repository, protocol::ConfigTarget};
use shoal_core::start_plan::{ChatPlan, ChatPlanStart};
use shoal_ids::{queue_entry_ids::QueueEntryId, repository_ids::RepositoryId};

/// Start the chat of the admitted launch `entry` that `plan` describes, its
/// agent taking the launch's slot.
pub(super) async fn start(
    paths: &CommandPaths,
    entry: &QueueEntryId,
    plan: ChatPlan,
) -> Result<()> {
    crate::env::launch_queue_entry(entry);
    match plan.chat_start {
        ChatPlanStart::Relaunch {
            args,
            repository,
            home,
            name,
        } => {
            let repository = registered(paths, repository.as_ref()).await?;
            let target = match &repository {
                Some(repository) => ConfigTarget::Repository(repository.id.clone().into()),
                None => ConfigTarget::Global,
            };
            let settings = crate::settings::of(paths, target).await?;
            let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
            let argv = crate::terminal::shoal_command(paths, &args)?;
            let repository = repository.as_ref();
            crate::places::queued::start_chat(
                paths, &settings, repository, &home, &name, argv, false,
            )
            .await
            .map(drop)
        }
        ChatPlanStart::Resolved(start) => crate::chat_starts::start_queued(paths, *start).await,
    }
}

/// The registered repository `id` names; none without one.
async fn registered(paths: &CommandPaths, id: Option<&RepositoryId>) -> Result<Option<Repository>> {
    let Some(id) = id else {
        return Ok(None);
    };
    let repositories = crate::client::repositories::list(paths).await?;
    let repository = (repositories.into_iter()).find(|repository| repository.id == *id);
    repository
        .context("the chat's repository is no longer registered")
        .map(Some)
}
