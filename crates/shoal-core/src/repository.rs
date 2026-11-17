//! Resolving repository selectors against the registered repositories, and
//! the remote identity of a checkout, which Git reads.
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use crate::{
    forge::{
        ForgeRepo,
        items::{link::LinkKind, reference::ItemRef},
        repo::remote_url::RemoteUrl,
    },
    git,
    model::Repository,
    refusal::{Reason, Refusal},
};
pub use shoal_protocol::repository::*;

/// Local checkouts use origin; clones retain their original source URL even
/// when their checkout is temporarily unavailable. No network access is needed.
pub async fn identity(source: &str) -> Result<Option<String>> {
    Origins::default().identity(source).await
}

/// Resolve a source that may be an existing checkout or a clone URL.
pub async fn remote_url_from_source(source: &str) -> Result<Option<String>> {
    Origins::default().source_url(source).await
}

/// The origin URLs of local checkouts, each read from Git when a lookup
/// first needs it and kept for the rest of one operation, which may look
/// up several sources among the same registered repositories.
#[derive(Default)]
pub struct Origins {
    read: Mutex<HashMap<PathBuf, Option<String>>>,
}

impl Origins {
    /// The remote identity of `source`, as [`identity`] reads it.
    async fn identity(&self, source: &str) -> Result<Option<String>> {
        Ok(self.source_url(source).await?.map(|url| url_key(&url)))
    }

    /// The URL of `source`, as [`remote_url_from_source`] reads it.
    async fn source_url(&self, source: &str) -> Result<Option<String>> {
        let path = Path::new(source);
        if path.exists() {
            self.of(path).await
        } else {
            Ok(Some(source.to_owned()))
        }
    }

    /// The origin URL of the checkout at `path`, as
    /// [`remote_url_from_path`] reads it.
    async fn of(&self, path: &Path) -> Result<Option<String>> {
        if let Some(origin) = self.lock().get(path) {
            return Ok(origin.clone());
        }
        let origin = remote_url_from_path(path).await?;
        self.lock().insert(path.to_owned(), origin.clone());
        Ok(origin)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, Option<String>>> {
        self.read.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Read origin from a known checkout without interpreting its path as a URL.
pub async fn remote_url_from_path(path: &Path) -> Result<Option<String>> {
    git::configured_remote_url(path, "origin").await
}

/// What is wrong with a checkout whose Git config sets `core.bare` although it
/// has a working tree, and how to fix it; `None` when Git can use the working tree.
pub async fn bare_flag_problem(path: &Path) -> Result<Option<String>> {
    if !git::is_bare_with_work_tree(path).await? {
        return Ok(None);
    }
    let fix = git::clear_bare_flag_command(path);
    Ok(Some(format!(
        "{} has a working tree, but its Git config sets core.bare=true; run {fix}",
        path.display()
    )))
}

/// Resolve the same repository selectors in the CLI and daemon.
pub async fn select(
    repositories: &[Repository],
    selector: impl Into<Selector>,
) -> Result<&Repository> {
    select_with(repositories, selector, &Origins::default()).await
}

/// [`select`], reading the checkouts' origins through `origins`, which the
/// caller's operation shares with its other lookups.
pub async fn select_with<'a>(
    repositories: &'a [Repository],
    selector: impl Into<Selector>,
    origins: &Origins,
) -> Result<&'a Repository> {
    let selector = selector.into();
    if let Selector::Id(id) = &selector {
        return repositories
            .iter()
            .find(|repo| repo.id == *id)
            .ok_or_else(|| {
                let message =
                    format!("no registered repository has the ID {id}; see shoal repo ls");
                Refusal::new(Reason::NotFound, message).into()
            });
    }
    let value = selector.value();
    if let Some(repo) = repositories
        .iter()
        .find(|repo| repo.id == value || repo.source == value)
    {
        return Ok(repo);
    }
    let canonical = selector.path();
    let exact = repositories
        .iter()
        .find(|repo| repo.name.as_deref() == Some(value));
    let mut matches: Vec<_> = repositories
        .iter()
        .filter(|repo| {
            exact.is_some_and(|exact| exact.id == repo.id)
                || (exact.is_none() && name(repo) == value)
                || repo.path.to_str() == Some(value)
                || canonical.as_ref() == Some(&repo.path)
        })
        .collect();
    ensure!(
        matches.len() <= 1,
        "repository name is ambiguous: {value}; matches {}; use its ID, path, or source URL",
        matches
            .iter()
            .map(|repo| format!("{} ({}, {})", name(repo), repo.id, repo.path.display()))
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(repo) = matches.pop() {
        return Ok(repo);
    }
    if (canonical.is_some() || RemoteUrl::parse(value).is_some())
        && let Some(repo) = find_by_identity(repositories, selector.source()?, origins)
            .await
            .with_context(|| match &canonical {
                Some(path) => format!(
                    "repository selector {value} was read as a path: {}",
                    path.display()
                ),
                None => format!("resolve repository selector: {value}"),
            })?
    {
        return Ok(repo);
    }
    if let Some(path) = &canonical
        && let Some(repo) = find_by_origin_path(repositories, path, origins).await
    {
        return Ok(repo);
    }
    Err(NotRegistered {
        value: value.to_owned(),
        path: canonical,
        item: ItemOf::find(repositories, value, origins).await?,
    }
    .into())
}

/// No registered repository matches the selector.
#[derive(Debug)]
pub struct NotRegistered {
    value: String,
    path: Option<std::path::PathBuf>,
    item: Option<ItemOf>,
}

impl std::fmt::Display for NotRegistered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(item) = &self.item {
            return write!(
                f,
                "{} is {}, not a repository; {item}",
                self.value, item.kind
            );
        }
        write!(f, "repository is not registered: {}", self.value)?;
        if let Some(path) = &self.path {
            write!(f, " (read as a path: {})", path.display())?;
        }
        write!(f, "; run `shoal repo add <path-or-url>`")
    }
}

/// The repository of an issue, PR or CI run URL given where a repository was
/// expected.
#[derive(Debug)]
struct ItemOf {
    /// What the URL names, as in `an issue URL`.
    kind: &'static str,
    /// The repository's URL.
    repository: String,
    /// The name of the registered repository with that URL, if any.
    registered: Option<String>,
}

impl ItemOf {
    /// The repository of the item `value` is the URL of, if it is one.
    async fn find(
        repositories: &[Repository],
        value: &str,
        origins: &Origins,
    ) -> Result<Option<Self>> {
        let Ok(item) = value.parse::<ItemRef>() else {
            return Ok(None);
        };
        let kind = match item.kind() {
            LinkKind::Issue => "an issue URL",
            LinkKind::Pr => "a PR URL",
            LinkKind::Ci => "a CI run URL",
        };
        let repository = item.repository().repository_url();
        let registered = find_by_identity(repositories, &repository, origins).await?;
        Ok(Some(Self {
            kind,
            registered: registered.map(|repo| name(repo).to_owned()),
            repository,
        }))
    }
}

impl std::fmt::Display for ItemOf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.registered {
            Some(name) => write!(f, "name its repository instead: {name}"),
            None => write!(
                f,
                "its repository {} is not registered; run `shoal repo add {}`",
                self.repository, self.repository
            ),
        }
    }
}

impl std::error::Error for NotRegistered {}

/// A local checkout or clone URL that registration can accept, which an
/// item's URL is not. An item's URL ends in its number, so no repository URL
/// is taken for one, as a GitLab group named `tree` would be for a branch.
pub fn registrable(selector: &str) -> bool {
    Path::new(selector).exists()
        || RemoteUrl::parse(selector).is_some() && selector.parse::<ItemRef>().is_err()
}

/// The registered repository with the remote identity of `source`, reading
/// the checkouts' origins through `origins`.
pub async fn find_by_identity<'a>(
    repositories: &'a [Repository],
    source: &str,
    origins: &Origins,
) -> Result<Option<&'a Repository>> {
    // Reading the source's identity costs Git a process.
    if repositories.is_empty() {
        return Ok(None);
    }
    let Some(source_identity) = origins.identity(source).await? else {
        return Ok(None);
    };
    for repo in repositories {
        if origins.identity(&repo.source).await?.as_ref() == Some(&source_identity) {
            return Ok(Some(repo));
        }
    }
    Ok(None)
}

/// The registered checkout whose origin is the local repository at `path`, as
/// a URL selects the checkout whose origin is that URL.
async fn find_by_origin_path<'a>(
    repositories: &'a [Repository],
    path: &Path,
    origins: &Origins,
) -> Option<&'a Repository> {
    for repo in repositories {
        // An unavailable checkout, or one without origin, matches nothing.
        let Ok(Some(origin)) = origins.of(&repo.path).await else {
            continue;
        };
        let origin = origin.strip_prefix("file://").unwrap_or(&origin);
        // Git reads a relative origin from the checkout.
        if std::fs::canonicalize(repo.path.join(origin)).is_ok_and(|origin| origin == path) {
            return Some(repo);
        }
    }
    None
}

/// The only registered repository whose origin `item` belongs to, or none
/// when no registered repository has that origin.
pub async fn registered_for_item<'a>(
    repos: &'a [Repository],
    item: &ItemRef,
) -> Result<Option<&'a Repository>> {
    let forge = item.repository().clone();
    match with_remote(repos, forge, "--repo <ID>").await {
        Ok(repo) => Ok(Some(repo)),
        Err(error) if error.is::<UnregisteredRemote>() => Ok(None),
        Err(error) => Err(error),
    }
}

/// The refusal of a forge repository no registered repository has as its
/// origin.
#[derive(Debug)]
pub struct UnregisteredRemote(pub ForgeRepo);

impl std::fmt::Display for UnregisteredRemote {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "no registered repository has the remote {}/{}; register it with `shoal repo add {}` and retry",
            self.0.host,
            self.0.path,
            self.0.repository_url()
        )
    }
}

impl std::error::Error for UnregisteredRemote {}

/// The only registered repository whose origin is `forge`; `retry` names the
/// spelling that selects one explicitly.
pub async fn with_remote<'a>(
    repos: &'a [Repository],
    forge: ForgeRepo,
    retry: &str,
) -> Result<&'a Repository> {
    let mut matches = Vec::new();
    for repo in repos {
        let Some(remote) = remote_url_from_source(&repo.source).await? else {
            continue;
        };
        if forge.is_remote(&remote) {
            matches.push(repo);
        }
    }
    match matches.as_slice() {
        [repo] => Ok(repo),
        [] => Err(UnregisteredRemote(forge).into()),
        _ => anyhow::bail!(
            "several registered repositories share the remote {}/{}; use `{retry}`",
            forge.host,
            forge.path
        ),
    }
}

/// The root of the worktree `directory` is in, if it still exists.
pub async fn worktree_root(directory: &Path) -> Result<Option<std::path::PathBuf>> {
    match git::worktree_root(directory).await {
        Ok(root) => Ok(Some(std::fs::canonicalize(root.trim_end())?)),
        Err(_) => Ok(None),
    }
}

/// The repository whose checkout, or one of whose `workspaces`' worktree,
/// is at `root`.
pub fn at_root<'a>(
    root: &Path,
    repos: &'a [Repository],
    workspaces: &'a [crate::model::Workspace],
) -> Option<&'a shoal_ids::repository_ids::RepositoryId> {
    if let Some(repo) = repos.iter().find(|repo| repo.path == root) {
        return Some(&repo.id);
    }
    (workspaces.iter())
        .find(|workspace| workspace.path == root)
        .map(|workspace| workspace.repository())
}

/// The selector `value` names: a path, canonical once it exists, else a
/// repository's ID or name.
pub fn named(value: String) -> Result<Selector> {
    named_in(value, None)
}

/// [`named`] with a relative path resolved against `directory`, the one a
/// start was made in, else this process's.
pub fn named_in(value: String, directory: Option<&Path>) -> Result<Selector> {
    let candidate =
        directory.map_or_else(|| Path::new(&value).to_owned(), |base| base.join(&value));
    if candidate.exists() {
        let path = std::fs::canonicalize(&candidate)?;
        path.to_str().context("repository path is not UTF-8")?;
        Ok(Selector::Path { value, path })
    } else {
        Ok(value.into())
    }
}

#[cfg(test)]
mod tests;
