//! Git remote syntax, separate from registration and forge lookup policy.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Transport<'a> {
    Http,
    Https,
    Ssh,
    Git,
    Scp,
    Other(&'a str),
}

#[derive(Debug)]
pub struct RemoteUrl<'a> {
    source: &'a str,
    pub host: Option<&'a str>,
    pub path: Option<&'a str>,
    pub(crate) transport: Transport<'a>,
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

    /// The same URL naming the repository at `path` on the same host, keeping
    /// the transport, user, port and `.git` suffix.
    pub fn with_path(&self, path: &str) -> Option<String> {
        let current = self.path?;
        let start = current.as_ptr() as usize - self.source.as_ptr() as usize;
        let end = start + current.len();
        Some(format!(
            "{}{path}{}",
            &self.source[..start],
            &self.source[end..]
        ))
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
pub fn source_name(source: &str) -> &str {
    let source = source.trim_end_matches('/');
    repository_path(source.rsplit(['/', ':']).next().unwrap_or(source))
}

pub(crate) fn repository_path(path: &str) -> &str {
    let path = path.trim_end_matches('/');
    path.strip_suffix(".git").unwrap_or(path)
}

#[cfg(test)]
mod tests;
