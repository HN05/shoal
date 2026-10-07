//! Repository naming and identity derived from a source path or URL.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::{git, model::Repository};

use super::remote_url::{RemoteUrl, source_name};

/// Preserve the caller's argument alongside a path resolved in its directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Selector {
    Value(String),
    Path {
        value: String,
        path: std::path::PathBuf,
    },
}

impl Selector {
    pub fn value(&self) -> &str {
        match self {
            Self::Value(value) | Self::Path { value, .. } => value,
        }
    }

    pub fn source(&self) -> Result<&str> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Path { path, .. } => path.to_str().context("repository path is not UTF-8"),
        }
    }

    fn path(&self) -> Option<std::path::PathBuf> {
        match self {
            Self::Value(value) if Path::new(value).is_absolute() => {
                std::fs::canonicalize(value).ok()
            }
            Self::Value(_) => None,
            Self::Path { path, .. } => Some(path.clone()),
        }
    }
}

impl From<String> for Selector {
    fn from(value: String) -> Self {
        Self::Value(value)
    }
}

impl From<&str> for Selector {
    fn from(value: &str) -> Self {
        value.to_owned().into()
    }
}

impl From<&String> for Selector {
    fn from(value: &String) -> Self {
        value.as_str().into()
    }
}

impl From<&Selector> for Selector {
    fn from(value: &Selector) -> Self {
        value.clone()
    }
}

/// The explicit name, or the last path component of the source.
pub fn name(repo: &crate::model::Repository) -> &str {
    repo.name
        .as_deref()
        .unwrap_or_else(|| source_name(&repo.source))
}

/// A filesystem-safe clone directory name derived from the source.
pub fn directory_name(source: &str) -> String {
    let name: String = source_name(source)
        .chars()
        .take(crate::validate::MAX_NAME_LEN)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = name.trim_matches(['.', '-', '_']);
    if name.is_empty() {
        "repository".into()
    } else {
        name.into()
    }
}

/// The host part of a remote URL (`https://user@host/...` or `user@host:...`),
/// without credentials.
pub fn host(source: &str) -> Option<&str> {
    RemoteUrl::parse(source).and_then(|remote| remote.host)
}

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
    let remotes = git::run(path, &["remote"]).await?;
    if !remotes.lines().any(|remote| remote == "origin") {
        return Ok(None);
    }
    Ok(Some(
        git::run(path, &["remote", "get-url", "origin"])
            .await?
            .trim()
            .to_owned(),
    ))
}

/// Normalize equivalent transports of one remote to a comparable key.
fn url_key(url: &str) -> String {
    RemoteUrl::parse(url)
        .map(|remote| remote.registration_key())
        .unwrap_or_else(|| url.to_owned())
}

/// Resolve the same repository selectors in the CLI and daemon.
pub async fn select(
    repositories: &[Repository],
    selector: impl Into<Selector>,
) -> Result<&Repository> {
    let selector = selector.into();
    let value = selector.value();
    let canonical = selector.path();
    let exact = repositories.iter().find(|repo| {
        repo.id == value || repo.source == value || repo.name.as_deref() == Some(value)
    });
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
    Err(NotRegistered {
        value: value.to_owned(),
        path: canonical,
    }
    .into())
}

/// No registered repository matches the selector.
#[derive(Debug)]
pub struct NotRegistered {
    value: String,
    path: Option<std::path::PathBuf>,
}

impl std::fmt::Display for NotRegistered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "repository is not registered: {}", self.value)?;
        if let Some(path) = &self.path {
            write!(f, " (read as a path: {})", path.display())?;
        }
        write!(f, "; run `shoal repo add <path-or-url>`")
    }
}

impl std::error::Error for NotRegistered {}

/// A local checkout or clone URL that registration can accept.
pub fn registrable(selector: &str) -> bool {
    Path::new(selector).exists() || RemoteUrl::parse(selector).is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn repository(id: &str, path: &str, name: Option<&str>) -> Repository {
        Repository {
            id: id.into(),
            path: path.into(),
            source: format!("https://example.test/team/{id}"),
            last_used: 0,
            name: name.map(str::to_owned),
            workspaces_dir: None,
        }
    }

    #[tokio::test]
    async fn typed_name_wins_over_a_colliding_caller_path() {
        let repositories = [repository(
            "registered",
            "/tmp/registered",
            Some("saldoir-server"),
        )];
        let selector = Selector::Path {
            value: "saldoir-server".into(),
            path: "/tmp/unrelated/saldoir-server".into(),
        };
        assert_eq!(
            select(&repositories, selector).await.unwrap().id,
            "registered"
        );
    }

    #[tokio::test]
    async fn colliding_name_and_path_are_reported_as_ambiguous() {
        let repositories = [
            repository("named", "/tmp/named", Some("saldoir-server")),
            repository("path", "/tmp/unrelated/saldoir-server", None),
        ];
        let selector = Selector::Path {
            value: "saldoir-server".into(),
            path: "/tmp/unrelated/saldoir-server".into(),
        };
        let error = select(&repositories, selector)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("named") && error.contains("path"), "{error}");
    }

    #[tokio::test]
    async fn source_lookup_keeps_urls_and_reads_only_origin_from_checkouts() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path();
        git::run(path, &["init", "--quiet", "--template="])
            .await
            .unwrap();
        let source = path.to_str().unwrap();
        assert_eq!(remote_url_from_source(source).await.unwrap(), None);
        git::run(
            path,
            &["remote", "add", "upstream", "https://other.test/a/b"],
        )
        .await
        .unwrap();
        assert_eq!(remote_url_from_path(path).await.unwrap(), None);

        let url = "ssh://git@example.com:2222/team/Repo.git";
        git::run(path, &["remote", "add", "origin", url])
            .await
            .unwrap();
        assert_eq!(
            remote_url_from_path(path).await.unwrap().as_deref(),
            Some(url)
        );
        assert_eq!(
            remote_url_from_source(source).await.unwrap().as_deref(),
            Some(url)
        );
        assert_eq!(
            identity(source).await.unwrap(),
            identity(url).await.unwrap()
        );
        assert_eq!(
            remote_url_from_source(url).await.unwrap().as_deref(),
            Some(url)
        );

        let missing = path.join("missing");
        assert!(remote_url_from_path(&missing).await.is_err());
        let missing = missing.to_str().unwrap();
        assert_eq!(
            remote_url_from_source(missing).await.unwrap().as_deref(),
            Some(missing)
        );
    }

    // macOS filesystems reject non-UTF-8 directory names.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn origin_lookup_accepts_non_utf8_checkout_paths() {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};

        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(OsStr::from_bytes(b"repo-\xff"));
        std::fs::create_dir(&path).unwrap();
        git::run(&path, &["init", "--quiet", "--template="])
            .await
            .unwrap();
        let url = "git@example.com:team/repo.git";
        git::run(&path, &["remote", "add", "origin", url])
            .await
            .unwrap();
        let remote = remote_url_from_path(&path).await.unwrap().unwrap();
        let forge = crate::forge::ForgeRepo::parse(&remote).unwrap();
        assert_eq!(
            forge.issue("178").unwrap().1,
            "https://example.com/team/repo/issues/178"
        );
        assert_eq!(
            forge.pull("178").unwrap().1,
            "https://example.com/team/repo/pulls/178"
        );
    }

    #[test]
    fn clone_directory_names_are_readable_single_components() {
        for (source, expected) in [
            (
                "https://example.com/team/saldoir-server.git",
                "saldoir-server",
            ),
            ("git@example.com:team/project.git", "project"),
            (
                "ssh://git@example.com:2222/team/My.Project.git/",
                "My.Project",
            ),
            ("file:///tmp/local repo.git", "local-repo"),
            ("file:///tmp/..", "repository"),
            ("file:///tmp/.git", "repository"),
        ] {
            assert_eq!(directory_name(source), expected);
        }
        assert_eq!(
            directory_name(&format!("https://example.com/{}", "a".repeat(300))).len(),
            64
        );
    }

    #[test]
    fn hosts_drop_credentials_and_paths() {
        assert_eq!(
            host("https://user@example.com/team/repo"),
            Some("example.com")
        );
        assert_eq!(host("git@example.com:team/repo.git"), Some("example.com"));
        assert_eq!(host("/local/checkout"), None);
    }

    #[test]
    fn equivalent_transports_match_without_conflating_hosts_paths_or_ports() {
        let key = url_key("https://example.com/team/Repo.git");
        assert_eq!(key, url_key("git@EXAMPLE.com:team/Repo.git"));
        assert_eq!(key, url_key("ssh://git@example.com/team/Repo/"));
        for other in [
            "https://other.com/team/Repo.git",
            "https://example.com/other/Repo.git",
            "https://example.com/team/repo.git",
            "ssh://git@example.com:2222/team/Repo.git",
        ] {
            assert_ne!(key, url_key(other));
        }
        assert_ne!(url_key("file:///repo.git"), url_key("file:///repo"));
    }
}
