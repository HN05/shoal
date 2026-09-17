//! Repository naming and identity derived from a source path or URL.
use anyhow::Result;
use std::path::Path;

use crate::git;

/// The explicit name, or the last path component of the source.
pub fn name(repo: &crate::model::Repository) -> &str {
    repo.name
        .as_deref()
        .unwrap_or_else(|| source_name(&repo.source))
}

pub fn source_name(source: &str) -> &str {
    let source = source.trim_end_matches('/');
    let name = source.rsplit(['/', ':']).next().unwrap_or(source);
    name.strip_suffix(".git").unwrap_or(name)
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
    let authority = source
        .split_once("://")
        .map(|(_, rest)| rest.split('/').next().unwrap_or(rest))
        .or_else(|| source.split_once(':').map(|(host, _)| host))
        .filter(|host| !host.is_empty())?;
    Some(authority.rsplit('@').next().unwrap_or(authority))
}

/// Local checkouts use origin; clones retain their original source URL even
/// when their checkout is temporarily unavailable. No network access is needed.
pub async fn identity(source: &str) -> Result<Option<String>> {
    Ok(remote_url(source).await?.map(|url| url_key(&url)))
}

pub async fn remote_url(source: &str) -> Result<Option<String>> {
    let url = if Path::new(source).exists() {
        let remotes = git::run(Path::new(source), &["remote"]).await?;
        if !remotes.lines().any(|remote| remote == "origin") {
            return Ok(None);
        }
        git::run(Path::new(source), &["remote", "get-url", "origin"])
            .await?
            .trim()
            .to_owned()
    } else {
        source.to_owned()
    };
    Ok(Some(url))
}

/// Normalize equivalent transports of one remote to a comparable key.
fn url_key(url: &str) -> String {
    let remote = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "http" | "https" | "ssh" | "git") {
            return url.to_owned();
        }
        rest.split_once('/')
    } else {
        url.split_once(':')
    };
    let Some((authority, path)) = remote else {
        return url.to_owned();
    };
    let host = authority.rsplit('@').next().unwrap_or(authority);
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    format!("remote:{}/{path}", host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::{directory_name, host, url_key};

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
