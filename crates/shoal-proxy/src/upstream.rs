//! The upstream the proxy forwards to: a scheme and authority, and a path
//! prefix that comes before each request's own path.
use anyhow::{Context, Result, ensure};
use hyper::Uri;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upstream {
    scheme: String,
    authority: String,
    /// The path before each request's, without a trailing slash.
    prefix: String,
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
        })
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
mod tests {
    use super::*;

    #[test]
    fn requests_keep_their_path_after_the_upstream_prefix() {
        let plain = Upstream::parse("http://127.0.0.1:8317").unwrap();
        assert_eq!(plain.authority(), "127.0.0.1:8317");
        assert_eq!(
            plain.join("/v1/messages?beta=true").unwrap(),
            "http://127.0.0.1:8317/v1/messages?beta=true"
        );
        let prefixed = Upstream::parse("https://chatgpt.com/backend-api/codex/").unwrap();
        assert_eq!(
            prefixed.join("/responses").unwrap(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        for (url, message) in [
            ("ftp://host", "http:// or https://"),
            ("127.0.0.1:8317", "http:// or https://"),
            ("http://host/?a=b", "must not have a query"),
        ] {
            let error = format!("{:#}", Upstream::parse(url).unwrap_err());
            assert!(error.contains(message), "{url}: {error}");
        }
    }
}
