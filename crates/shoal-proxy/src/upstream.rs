//! The upstream the proxy forwards to: a scheme and authority, a path
//! prefix that comes before each request's own path, and the key it may
//! take in place of the agent's own credentials.
use anyhow::{Context, Result, ensure};
use hyper::{Uri, header::HeaderValue};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    scheme: String,
    authority: String,
    /// The path before each request's, without a trailing slash.
    prefix: String,
    /// The `Authorization` the upstream gets instead of the agent's.
    key: Option<HeaderValue>,
}

impl Upstream {
    /// An `http` or `https` URL such as `http://127.0.0.1:8317` or
    /// `https://chatgpt.com/backend-api/codex`.
    pub fn parse(url: &str) -> Result<Self> {
        let uri: Uri = url
            .parse()
            .with_context(|| format!("invalid upstream URL {url:?}"))?;
        let scheme = uri
            .scheme_str()
            .context("upstream URL needs http:// or https://")?;
        ensure!(
            scheme == "http" || scheme == "https",
            "upstream URL needs http:// or https://, not {scheme}://"
        );
        ensure!(
            uri.query().is_none(),
            "upstream URL {url:?} must not have a query"
        );
        let authority = uri.authority().context("upstream URL needs a host")?;
        Ok(Self {
            scheme: scheme.to_owned(),
            authority: authority.as_str().to_owned(),
            prefix: uri.path().trim_end_matches('/').to_owned(),
            key: None,
        })
    }

    /// This upstream reached with `key` as a bearer token, such as an API key
    /// of CLIProxyAPI's, instead of the credentials each agent sends.
    pub fn with_key(mut self, key: &str) -> Result<Self> {
        let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
            .ok()
            .filter(|_| !key.is_empty())
            .context("an upstream key must be visible ASCII text")?;
        value.set_sensitive(true);
        self.key = Some(value);
        Ok(self)
    }

    /// The `Authorization` that replaces the agent's credentials.
    pub(crate) fn key(&self) -> Option<&HeaderValue> {
        self.key.as_ref()
    }

    /// The upstream's `host[:port]`, which requests name as their `Host`.
    pub(crate) fn authority(&self) -> &str {
        &self.authority
    }

    /// The upstream URI for a request to `path_and_query` on the proxy.
    pub(crate) fn join(&self, path_and_query: &str) -> Result<Uri> {
        let path = if path_and_query.starts_with('/') {
            path_and_query.to_owned()
        } else {
            format!("/{path_and_query}")
        };
        format!("{}://{}{}{path}", self.scheme, self.authority, self.prefix)
            .parse()
            .context("build the upstream request URI")
    }
}

#[cfg(test)]
mod tests;
