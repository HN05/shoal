//! The group a new place joins: the caller's own, otherwise the one a host's
//! config names for the repository. A queued start that cannot open its
//! place falls back from its origin's group to its host's configured group
//! on the same server, then to the default host's configured group, then to
//! zmx.
use std::path::Path;

use anyhow::{Context as _, Result};

use super::{Configured, Origin};
use crate::{
    cli::{client, context::Context},
    config::Effective,
    model::Repository,
    sessions::HostName,
};

/// The repository whose name a configured group's name takes, looked up
/// only once a name is needed.
pub(super) enum Repo<'a> {
    Selected(crate::repository::Selector),
    Id(&'a str),
    Known(&'a Repository),
}

impl Repo<'_> {
    async fn name(&self, ctx: &Context) -> Result<String> {
        let repos = match self {
            Self::Known(repo) => return Ok(crate::repository::name(repo).to_owned()),
            Self::Selected(_) | Self::Id(_) => client::repositories(&ctx.paths).await?,
        };
        let repo = match self {
            Self::Selected(selector) => crate::repository::select(&repos, selector.clone()).await?,
            _ => repos
                .iter()
                .find(|repo| matches!(self, Self::Id(id) if repo.id == *id))
                .context("repository is no longer registered")?,
        };
        Ok(crate::repository::name(repo).to_owned())
    }
}

/// The group `host`'s config names for `repo`, on the server of `near` or
/// this machine's; `None` for a host without configured groups.
async fn configured(
    ctx: &Context,
    host: &Configured,
    near: Option<&Origin>,
    repo: &Repo<'_>,
    cwd: &Path,
) -> Result<Option<Origin>> {
    let Some(template) = &host.options.group_template else {
        return Ok(None);
    };
    let label =
        crate::config::placeholders::render(template, &[("{repo}", &repo.name(ctx).await?)]);
    host.host.group(near, &label, cwd).await
}

/// The place a start beside this terminal joins in `host`: the one `host`
/// serves, otherwise its configured group.
pub(super) async fn beside(
    ctx: &Context,
    host: &Configured,
    repo: &Repo<'_>,
    cwd: &Path,
) -> Result<Option<Origin>> {
    match host.host.origin() {
        Some(origin) => Ok(Some(origin)),
        None => configured(ctx, host, None, repo, cwd).await,
    }
}

/// Run `start` in the first host that opens a queued start from `origin`,
/// trying the next one while they fail or `start` passes them over with
/// `None`. Returns the last failure when none started it, and `None` when
/// every host was passed over.
pub(super) async fn first_queued<T>(
    ctx: &Context,
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
                match configured(ctx, &fallback.host, *near, repo, cwd).await {
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

/// A host a queued start may run in, and the group it joins there.
struct Fallback<'a> {
    host: Configured,
    group: Group<'a>,
}

enum Group<'a> {
    /// The origin itself.
    Origin(&'a Origin),
    /// The host's configured group, on the server of the origin when given.
    Configured(Option<&'a Origin>),
}

/// Where a queued start from `origin` may run, in the order to try: its own
/// group, its host's configured group on the same server, the default
/// host's configured group, then zmx.
fn fallbacks<'a>(settings: &Effective, origin: Option<&'a Origin>) -> Vec<Fallback<'a>> {
    let mut fallbacks = Vec::new();
    if let Some(origin) = origin
        && let Some(host) = Configured::all(settings)
            .find(|host| host.host.name() == origin.host && host.takes_queued(Some(origin)))
    {
        let keeps_groups = host.options.group_template.is_some();
        fallbacks.push(Fallback {
            host,
            group: Group::Origin(origin),
        });
        if keeps_groups {
            fallbacks.push(Fallback {
                host: Configured::named(settings, origin.host),
                group: Group::Configured(Some(origin)),
            });
        }
    }
    for host in [
        Configured::default_host(settings),
        Configured::named(settings, HostName::Zmx),
    ] {
        // Config that keeps a host from opening places keeps queued starts out.
        if !host.options.opens_places {
            continue;
        }
        fallbacks.push(Fallback {
            host,
            group: Group::Configured(None),
        });
    }
    fallbacks
}

#[cfg(test)]
mod tests;
