//! Git remote syntax, separate from registration and forge lookup policy.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transport<'a> {
    Http,
    Https,
    Ssh,
    Git,
    Scp,
    Other(&'a str),
}

#[derive(Debug)]
pub(super) struct RemoteUrl<'a> {
    source: &'a str,
    pub host: Option<&'a str>,
    pub path: Option<&'a str>,
    pub transport: Transport<'a>,
}

impl<'a> RemoteUrl<'a> {
    /// Preserve incomplete and unknown URL forms: registration keeps their
    /// existing identity, while forge lookup applies stricter validation.
    pub fn parse(source: &'a str) -> Option<Self> {
        let (transport, authority, path) = if let Some((scheme, rest)) = source.split_once("://") {
            let transport = match scheme {
                "http" => Transport::Http,
                "https" => Transport::Https,
                "ssh" => Transport::Ssh,
                "git" => Transport::Git,
                other => Transport::Other(other),
            };
            let (authority, path) = match rest.split_once('/') {
                Some((authority, path)) => (authority, Some(path)),
                None => (rest, None),
            };
            (transport, authority, path)
        } else {
            let (authority, path) = source.split_once(':')?;
            (Transport::Scp, authority, Some(path))
        };
        Some(Self {
            source,
            host: (!authority.is_empty()).then(|| authority.rsplit('@').next().unwrap()),
            path: path.map(repository_path),
            transport,
        })
    }

    /// Registration retains ports, path case, and permissive path syntax.
    pub fn registration_key(&self) -> String {
        if !matches!(self.transport, Transport::Other(_))
            && let Some(path) = self.path
        {
            return format!(
                "remote:{}/{path}",
                self.host.unwrap_or_default().to_ascii_lowercase()
            );
        }
        self.source.to_owned()
    }
}

/// Naming also accepts local paths and retains the historical colon separator.
pub(super) fn source_name(source: &str) -> &str {
    let source = source.trim_end_matches('/');
    repository_path(source.rsplit(['/', ':']).next().unwrap_or(source))
}

fn repository_path(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git").unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::forge::ForgeRepo;

    #[test]
    fn registration_and_forge_policies_share_parsed_syntax() {
        for (source, key, forge_host, web_scheme) in [
            (
                "https://user:secret@EXAMPLE.com/team/Repo.git///",
                "remote:example.com/team/Repo",
                Some("example.com"),
                "https",
            ),
            (
                "http://example.com:3000/team/Repo",
                "remote:example.com:3000/team/Repo",
                Some("example.com:3000"),
                "http",
            ),
            (
                "ssh://git@EXAMPLE.com:2222/team/Repo.git/",
                "remote:example.com:2222/team/Repo",
                Some("example.com"),
                "https",
            ),
            (
                "git@EXAMPLE.com:team/Repo.git",
                "remote:example.com/team/Repo",
                Some("example.com"),
                "https",
            ),
            (
                "example.com:team/Repo",
                "remote:example.com/team/Repo",
                Some("example.com"),
                "https",
            ),
            (
                "git://example.com/team/Repo.git",
                "remote:example.com/team/Repo",
                None,
                "",
            ),
            ("file:///team/Repo.git", "file:///team/Repo.git", None, ""),
            (
                "HTTPS://example.com/team/Repo",
                "HTTPS://example.com/team/Repo",
                None,
                "",
            ),
            ("https://example.com", "https://example.com", None, ""),
            ("https:///team/Repo.git", "remote:/team/Repo", None, ""),
            (":team/Repo.git", "remote:/team/Repo", None, ""),
            (
                "https://-example.com/team/Repo",
                "remote:-example.com/team/Repo",
                None,
                "",
            ),
            (
                "https://example.com/group/team/Repo",
                "remote:example.com/group/team/Repo",
                None,
                "",
            ),
            (
                "https://example.com/team/../",
                "remote:example.com/team/..",
                None,
                "",
            ),
            (
                "https://example.com/team//Repo",
                "remote:example.com/team//Repo",
                None,
                "",
            ),
            (
                "https://example.com/team/Repo?query",
                "remote:example.com/team/Repo?query",
                None,
                "",
            ),
            (
                "https://example.com/team/Repo#fragment",
                "remote:example.com/team/Repo#fragment",
                None,
                "",
            ),
            (
                "https://example.com/team/repo%20name",
                "remote:example.com/team/repo%20name",
                None,
                "",
            ),
            (
                "https://example.com/team/répo",
                "remote:example.com/team/répo",
                None,
                "",
            ),
        ] {
            let remote = RemoteUrl::parse(source).unwrap();
            assert_eq!(remote.registration_key(), key, "{source}");
            let forge = ForgeRepo::from_remote(&remote);
            if let Some(host) = forge_host {
                let forge = forge.unwrap();
                assert_eq!(forge.host, host, "{source}");
                assert_eq!(forge.path, "team/Repo", "{source}");
                assert_eq!(forge.web_scheme, web_scheme, "{source}");
            } else {
                assert!(forge.is_err(), "{source}");
            }
        }
        assert!(RemoteUrl::parse("/local/checkout").is_none());
    }
}
