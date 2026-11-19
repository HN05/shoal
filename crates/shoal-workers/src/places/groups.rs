//! The group a new place joins: the caller's own, otherwise the one a host's
//! config names for the repository, whose name it looks up from the daemon.
use std::path::Path;

use anyhow::{Context as _, Result};

use crate::{
    client::connection::CommandPaths,
    model::Repository,
    terminal::{
        self, Configured, Places,
        groups::{Group, fallbacks},
    },
};
use shoal_core::{config::Effective, sessions::Origin};

/// The repository whose name a configured group's name takes, looked up
/// only once a name is needed, or for a chat without one, the `root_dir`
/// it runs in, whose directory name the group takes.
pub enum Repo<'a> {
    Selected(crate::repository::Selector),
    Id(&'a str),
    Known(&'a Repository),
    Root(&'a Path),
}

impl<'a> Repo<'a> {
    /// The repository of a chat, or the `root_dir` of a chat without one.
    pub fn of_chat(repository: Option<&'a Repository>, root: &'a Path) -> Self {
        match repository {
            Some(repository) => Self::Known(repository),
            None => Self::Root(root),
        }
    }

    async fn name(&self, paths: &CommandPaths) -> Result<String> {
        let repos = match self {
            Self::Known(repo) => return Ok(repo.display_name().to_owned()),
            Self::Root(root) => {
                let name = root.file_name().unwrap_or(root.as_os_str());
                return Ok(name.to_string_lossy().into_owned());
            }
            Self::Selected(_) | Self::Id(_) => crate::client::repositories::list(paths).await?,
        };
        let repo = match self {
            Self::Selected(selector) => crate::repository::select(&repos, selector.clone()).await?,
            _ => repos
                .iter()
                .find(|repo| matches!(self, Self::Id(id) if repo.id == *id))
                .context("repository is no longer registered")?,
        };
        Ok(repo.display_name().to_owned())
    }
}

/// The group `host`'s config names for `repo`, on the server of `near` or
/// this machine's; `None` for a host without configured groups.
async fn configured(
    paths: &CommandPaths,
    host: &Configured,
    near: Option<&Origin>,
    repo: &Repo<'_>,
    cwd: &Path,
) -> Result<Option<Origin>> {
    let Some(template) = &host.options.group_template else {
        return Ok(None);
    };
    let label =
        crate::config::placeholders::render(template, &[("{repo}", &repo.name(paths).await?)]);
    terminal::labelled_group(&paths.state, host, near, &label, cwd).await
}

/// The place a start beside this terminal joins in `host`: the one `host`
/// serves, otherwise its configured group.
pub async fn beside(
    paths: &CommandPaths,
    host: &Configured,
    repo: &Repo<'_>,
    cwd: &Path,
) -> Result<Option<Origin>> {
    match host.host.places().and_then(Places::origin) {
        Some(origin) => Ok(Some(origin)),
        None => configured(paths, host, None, repo, cwd).await,
    }
}

/// Run `start` in the first host that opens a queued start from `origin`,
/// trying the next one while they fail or `start` passes them over with
/// `None`. Returns the last failure when none started it, and `None` when
/// every host was passed over.
pub async fn first_queued<T>(
    paths: &CommandPaths,
    settings: &Effective,
    origin: Option<&Origin>,
    repo: &Repo<'_>,
    cwd: &Path,
    mut start: impl AsyncFnMut(&Configured, Option<&Origin>) -> Result<Option<T>>,
) -> Result<Option<T>> {
    let mut failure = None;
    let mut tried = Vec::new();
    for fallback in fallbacks(settings, origin) {
        let group = match &fallback.group {
            Group::Origin(origin) => Some((*origin).clone()),
            Group::Configured(near) => {
                match configured(paths, &fallback.host, *near, repo, cwd).await {
                    Ok(group) => group,
                    Err(error) => {
                        failure = Some(error);
                        continue;
                    }
                }
            }
        };
        let key = (
            fallback.host.host.name(),
            group.as_ref().and_then(|origin| origin.group.clone()),
        );
        if tried.contains(&key) {
            continue;
        }
        tried.push(key);
        match start(&fallback.host, group.as_ref()).await {
            Ok(Some(started)) => return Ok(Some(started)),
            Ok(None) => {}
            Err(error) => failure = Some(error),
        }
    }
    failure.map_or(Ok(None), Err)
}
