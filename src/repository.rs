//! Resolving repository selectors against the registered repositories, and
//! the remote identity of a checkout, which Git reads.
use anyhow::{Context, Result, ensure};
use std::path::Path;

pub(crate) use crate::client::repository::*;
use crate::{
    client::repository::url_key,
    forge::{
        ForgeRepo, IssueInput,
        items::link::{self, LinkKind},
        repo::remote_url::RemoteUrl,
    },
    git,
    model::Repository,
};

/// Local checkouts use origin; clones retain their original source URL even
/// when their checkout is temporarily unavailable. No network access is needed.
pub async fn identity(source: &str) -> Result<Option<String>> {
    Ok(remote_url_from_source(source)
        .await?
        .map(|url| url_key(&url)))
}

/// Resolve a source that may be an existing checkout or a clone URL.
pub async fn remote_url_from_source(source: &str) -> Result<Option<String>> {
    let path = Path::new(source);
    if path.exists() {
        remote_url_from_path(path).await
    } else {
        Ok(Some(source.to_owned()))
    }
}

/// Read origin from a known checkout without interpreting its path as a URL.
pub async fn remote_url_from_path(path: &Path) -> Result<Option<String>> {
    let remotes = git::remotes(path, git::run).await?;
    if !remotes.lines().any(|remote| remote == "origin") {
        return Ok(None);
    }
    Ok(Some(
        git::remote_url(path, "origin").await?.trim().to_owned(),
    ))
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
    let selector = selector.into();
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
        && let Some(repo) = find_by_identity(repositories, selector.source()?)
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
        && let Some(repo) = find_by_origin_path(repositories, path).await
    {
        return Ok(repo);
    }
    Err(NotRegistered {
        value: value.to_owned(),
        path: canonical,
        item: ItemOf::find(repositories, value).await?,
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
    async fn find(repositories: &[Repository], value: &str) -> Result<Option<Self>> {
        let Some((kind, repository)) = item_repository(value) else {
            return Ok(None);
        };
        let kind = match kind {
            LinkKind::Issue => "an issue URL",
            LinkKind::Pr => "a PR URL",
            LinkKind::Ci => "a CI run URL",
        };
        let repository = repository.repository_url();
        let registered = find_by_identity(repositories, &repository).await?;
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
/// item's URL is not.
pub fn registrable(selector: &str) -> bool {
    Path::new(selector).exists()
        || RemoteUrl::parse(selector).is_some() && item_repository(selector).is_none()
}

/// The kind and repository of the issue, PR or CI run `url` names, if it
/// names one. Each ends in a number, so no repository URL is taken for one,
/// as a GitLab group named `tree` would be taken for a branch.
fn item_repository(url: &str) -> Option<(LinkKind, ForgeRepo)> {
    if IssueInput::parse(url) != IssueInput::Url {
        return None;
    }
    [LinkKind::Issue, LinkKind::Pr, LinkKind::Ci]
        .into_iter()
        .find_map(|kind| Some((kind, link::item(kind, url).ok()?.0)))
}

pub async fn find_by_identity<'a>(
    repositories: &'a [Repository],
    source: &str,
) -> Result<Option<&'a Repository>> {
    let Some(source_identity) = identity(source).await? else {
        return Ok(None);
    };
    for repo in repositories {
        if identity(&repo.source).await?.as_ref() == Some(&source_identity) {
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
) -> Option<&'a Repository> {
    for repo in repositories {
        // An unavailable checkout, or one without origin, matches nothing.
        let Ok(Some(origin)) = remote_url_from_path(&repo.path).await else {
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

#[cfg(test)]
mod tests;
