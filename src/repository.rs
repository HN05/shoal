use anyhow::Result;
use std::path::Path;

use crate::worktrunk;

/// Local checkouts use origin; clones retain their original source URL even
/// when their checkout is temporarily unavailable. No network access is needed.
pub async fn identity(source: &str) -> Result<Option<String>> {
    let url = if Path::new(source).exists() {
        let remotes = worktrunk::git(Path::new(source), &["remote"]).await?;
        if !remotes.lines().any(|remote| remote == "origin") {
            return Ok(None);
        }
        worktrunk::git(Path::new(source), &["remote", "get-url", "origin"])
            .await?
            .trim()
            .to_owned()
    } else {
        source.to_owned()
    };
    Ok(Some(url_key(&url)))
}

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
    use super::url_key;

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
