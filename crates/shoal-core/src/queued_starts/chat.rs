//! An admitted chat launch: its chat starts in a background place where the
//! terminal that queued it would have opened one, and the chat's agent takes
//! the launch's slot.
use std::{ffi::OsString, path::PathBuf};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::client::{
    queue::QueueEntryId,
    repository_ids::RepositoryId,
    space_names::{SpaceName, SpaceSelector},
};
use crate::{model::Repository, paths::CommandPaths, protocol::ChatStart, protocol::ConfigTarget};

/// What the queue's start of a chat launch runs, which a launch of a new
/// workspace's plan tells apart by its `chat_start`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatPlan {
    /// The space the chat goes below, which the queue ranks and scopes the
    /// launch by until the chat exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<SpaceSelector>,
    pub chat_start: ChatPlanStart,
}

/// How a queued chat starts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatPlanStart {
    /// The `shoal` arguments that start the chat `name` here, as the
    /// terminal that queued it built them, run in `home`, its checkout,
    /// worktree or `root_dir`, of `repository`, or of none.
    Relaunch {
        args: Vec<String>,
        repository: Option<RepositoryId>,
        home: PathBuf,
        name: SpaceName,
    },
    /// The start the daemon resolved for a client, whose chat it recorded
    /// when the launch had to wait.
    Resolved(Box<ChatStart>),
}

impl ChatPlan {
    /// The chat plan `plan` holds, if it is one.
    pub fn of(plan: &str) -> Option<Self> {
        serde_json::from_str(plan).ok()
    }
}

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
            let settings = crate::config::settings(paths, target).await?;
            let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
            let argv = crate::terminal::shoal_command(paths, &args)?;
            let repository = repository.as_ref();
            crate::terminal::queued::start_chat(
                paths, &settings, repository, &home, &name, argv, false,
            )
            .await
            .map(drop)
        }
        ChatPlanStart::Resolved(start) => crate::chat_starts::start_queued(paths, &start).await,
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
