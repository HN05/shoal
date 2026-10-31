//! Files the forge serves on its web host to signed-in users, such as the
//! images and attachments in descriptions and comments of a private
//! repository. Only URLs on the repository's own forge are fetched, so its
//! login goes nowhere else.
use anyhow::{Context, Result, ensure};

use crate::{
    ForgeRepo, account::Account, backends::Backend, capability::Capability,
    transport::api::http_body,
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
        async {
            let fetched = self.kind.download(self, account, url).await?;
            if !(200..300).contains(&fetched.status) {
                http_body(fetched.status, &String::from_utf8_lossy(&fetched.body))?;
            }
            let content_type = fetched
                .header("content-type")
                .and_then(|value| value.split(';').next())
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty());
            // A forge answers a request its login cannot read with a page,
            // such as its sign-in page, which is never the file itself.
            ensure!(
                content_type.as_deref() != Some("text/html"),
                "the forge answered with a web page instead of the file; check that the login can read the repository"
            );
            Ok(Download {
                content_type,
                bytes: fetched.body,
            })
        }
        .await
        .with_context(|| format!("download {url}"))
    }

    /// Refuse a URL that is not on this repository's forge, whose login
    /// must not reach another host.
    pub fn check_download(&self, url: &str) -> Result<()> {
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix(&format!("{}://", self.web_scheme)))
            .with_context(|| format!("{url} is not a {} URL", self.web_scheme))?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
        ensure!(
            authority.eq_ignore_ascii_case(&self.host)
                && !url.chars().any(|c| c.is_whitespace() || c.is_control()),
            "{url} is not on the repository's forge at {}",
            self.host
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests;
