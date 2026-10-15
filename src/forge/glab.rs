//! GitLab's CLI, `glab`: the hosts it has a login for identify self-managed
//! GitLab instances, and `glab api` sends requests with that login.
use std::path::PathBuf;

/// Whether glab has a login for `host`, read from its configuration without
/// running it, since every forge lookup selects a backend.
pub(super) fn has_login(host: &str) -> bool {
    let named = |candidate: &str| matches_host(candidate, host);
    if std::env::var("GITLAB_HOST").is_ok_and(|value| named(&value)) {
        return true;
    }
    config_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .is_some_and(|config| hosts(&config).iter().any(|candidate| named(candidate)))
}

/// Where glab keeps its configuration on every platform.
fn config_path() -> Option<PathBuf> {
    let directory = |variable| std::env::var_os(variable).filter(|dir| !dir.is_empty());
    if let Some(dir) = directory("GLAB_CONFIG_DIR") {
        return Some(PathBuf::from(dir).join("config.yml"));
    }
    let config = directory("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            crate::fsutil::home_dir()
                .ok()
                .map(|home| home.join(".config"))
        })?;
    Some(config.join("glab-cli/config.yml"))
}

/// The keys of the top-level `hosts` mapping in glab's YAML configuration.
fn hosts(config: &str) -> Vec<String> {
    let mut hosts = Vec::new();
    let mut indent = None;
    for line in config
        .lines()
        .skip_while(|line| line.trim_end() != "hosts:")
        .skip(1)
    {
        let content = line.trim();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        let depth = line.len() - line.trim_start().len();
        if depth == 0 {
            break;
        }
        if depth == *indent.get_or_insert(depth)
            && let Some(key) = content.strip_suffix(':')
        {
            hosts.push(key.trim_matches(['"', '\'']).to_owned());
        }
    }
    hosts
}

/// glab names a host bare or as a URL; a host without a port matches either.
fn matches_host(candidate: &str, host: &str) -> bool {
    let candidate = candidate
        .split_once("://")
        .map_or(candidate, |(_, rest)| rest)
        .trim_end_matches('/')
        .to_ascii_lowercase();
    candidate == host || (!host.contains(':') && candidate.split(':').next() == Some(host))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_are_the_keys_under_the_hosts_mapping() {
        let config = "git_protocol: ssh\n\
            hosts:\n    gitlab.com:\n        token: secret\n        api_host: gitlab.com\n\
            \n    # a comment\n    'git.corp.example:8443':\n        token: other\n\
            editor: vim\nother:\n    not.a.host:\n";
        assert_eq!(hosts(config), ["gitlab.com", "git.corp.example:8443"]);
        assert!(hosts("git_protocol: ssh\n").is_empty());
    }

    #[test]
    fn hosts_match_with_or_without_a_url_or_port() {
        assert!(matches_host(
            "https://Git.Corp.Example/",
            "git.corp.example"
        ));
        assert!(matches_host("git.corp.example:8443", "git.corp.example"));
        assert!(matches_host(
            "git.corp.example:8443",
            "git.corp.example:8443"
        ));
        assert!(!matches_host("git.corp.example", "git.corp.example:8443"));
        assert!(!matches_host("corp.example", "git.corp.example"));
    }
}
