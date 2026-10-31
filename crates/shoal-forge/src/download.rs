//! Files the forge serves on its web host to signed-in users, such as the
//! images and attachments in descriptions and comments of a private
//! repository. Only URLs on the repository's own forge are fetched, so its
//! login goes nowhere else.
use anyhow::{Context, Result, ensure};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    transport::{
        api::http_body,
        response::{Fetched, Headed},
    },
};

/// A downloaded file and the content type the forge served it as.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

impl ForgeRepo {
    /// The file at `url` on this repository's forge, read as `account`.
    pub async fn download(&self, account: &Account, url: &str) -> Result<Download> {
        self.require(Capability::Downloads)?;
        self.check_download(url)?;
        let fetched = self.kind.download(self, account, url);
        async { self.limited(account, fetched).await?.into_download() }
            .await
            .with_context(|| format!("download {url}"))
    }

    /// Refuse a URL that is not on this repository's forge, whose login
    /// must not reach another host.
    pub fn check_download(&self, url: &str) -> Result<()> {
        let (scheme, authority) = split_url(url)?;
        ensure!(
            (scheme == "https" || scheme == self.web_scheme)
                && authority.eq_ignore_ascii_case(&self.host),
            "{url} is not on the repository's forge at {}",
            self.host
        );
        Ok(())
    }
}

/// Refuse what is not an HTTP or HTTPS URL with a host, before any
/// repository is known.
pub fn check_url(url: &str) -> Result<()> {
    split_url(url).map(drop)
}

/// The scheme and authority of the HTTP or HTTPS URL `url`.
fn split_url(url: &str) -> Result<(&str, &str)> {
    let (scheme, rest) = url
        .split_once("://")
        .filter(|(scheme, _)| matches!(*scheme, "http" | "https"))
        .with_context(|| format!("{url} is not an HTTP or HTTPS URL"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    ensure!(
        !authority.is_empty() && !url.chars().any(|c| c.is_whitespace() || c.is_control()),
        "{url} is not a valid URL"
    );
    Ok((scheme, authority))
}

impl Fetched {
    /// The file a successful response carries, or the error a failed one
    /// reports.
    fn into_download(self) -> Result<Download> {
        if !(200..300).contains(&self.status) {
            http_body(self.status, &String::from_utf8_lossy(&self.body))?;
        }
        let content_type = self
            .header("content-type")
            .and_then(|value| value.split(';').next())
            .map(|value| value.trim().to_ascii_lowercase())
            .filter(|value| !value.is_empty());
        // A forge answers a request its login cannot read with a page, such
        // as its sign-in page, which is never the file itself.
        ensure!(
            content_type.as_deref() != Some("text/html"),
            "the forge answered with a web page instead of the file; check that the login can read the repository"
        );
        Ok(Download {
            content_type,
            bytes: self.body,
        })
    }
}

#[cfg(test)]
mod tests;
