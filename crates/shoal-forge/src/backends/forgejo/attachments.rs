//! Forgejo's attached files, kept with the item or comment that shows them.
//! The instance's attachment settings are checked before a comment or item
//! is posted, since Forgejo takes its files only once it exists.
use std::io::Read;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::Value;

use super::Forgejo;
use crate::{
    ForgeRepo,
    account::Account,
    backends::{
        Attachments,
        area::{image, link, served_at},
    },
    items::attach::{Attachment, Holder, Media},
    transport::{
        api::{Request, Upload, encode},
        download,
        response::Fetched,
    },
};

mod sniff;

impl Attachments for Forgejo {
    /// The instance's settings are checked here, since an upload they refuse
    /// would fail only after its comment or item is posted.
    async fn check_uploads(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        attachments: &[Attachment],
    ) -> Result<bool> {
        let settings = Settings::read(repo, account).await?;
        for attachment in attachments {
            settings.check(attachment)?;
        }
        Ok(true)
    }

    /// Forgejo takes the host's token for attachments and raw files on its
    /// web host; an anonymous request reaches only what anyone may see.
    async fn download(&self, repo: &ForgeRepo, account: &Account, url: &str) -> Result<Fetched> {
        let authorization = account.authorization(repo).await?;
        download::with_curl(url, authorization).await
    }

    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        _repository: Option<u64>,
    ) -> Result<Request> {
        let name = encode(attachment.name(), false);
        let endpoint = match holder {
            Holder::Item(number) => format!("issues/{number}/assets?name={name}"),
            Holder::Comment(id) => format!("issues/comments/{id}/assets?name={name}"),
            Holder::Repository => bail!("Forgejo keeps attached files with items and comments"),
        };
        Ok(Request::upload(
            repo.repo_endpoint(&endpoint),
            Upload {
                path: attachment.path.clone(),
                content_type: attachment.content_type(),
                field: Some("attachment"),
            },
        ))
    }

    fn uploaded_url(&self, response: &Value) -> Result<String> {
        served_at(response, "browser_download_url")
    }

    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match attachment.media() {
            Media::Video => format!("<video src=\"{url}\" controls></video>"),
            Media::Image => image(alt, url),
            Media::File => link(alt, url),
        }
    }
}

/// What `GET /settings/attachment` reports.
#[derive(Debug, Deserialize)]
struct Settings {
    enabled: bool,
    /// Comma-separated extensions (`.png`), media types (`image/png`) and
    /// wildcards (`image/*`, `*/*`); none allows every file.
    #[serde(default)]
    allowed_types: String,
    /// In MiB.
    max_size: u64,
}

impl Settings {
    async fn read(repo: &ForgeRepo, account: &Account) -> Result<Self> {
        let response = repo
            .send(account, &Request::get("settings/attachment".to_owned()))
            .await
            .context("read the forge's attachment settings")?;
        serde_json::from_value(response).context("read the forge's attachment settings")
    }

    /// Refuse an attachment the instance would refuse to upload.
    fn check(&self, attachment: &Attachment) -> Result<()> {
        ensure!(self.enabled, "the forge does not accept attached files");
        let path = attachment.path.display();
        if !self.allows(attachment)? {
            bail!(
                "{path}: the forge accepts only these attached files: {}",
                self.allowed_types
            );
        }
        let len = std::fs::metadata(&attachment.path)
            .with_context(|| format!("read {path}"))?
            .len();
        ensure!(
            len <= self.max_size * 1024 * 1024,
            "{path}: the forge accepts attached files of at most {} MiB",
            self.max_size
        );
        Ok(())
    }

    /// Whether an entry allows the attachment's extension or the media type
    /// Forgejo detects in it.
    fn allows(&self, attachment: &Attachment) -> Result<bool> {
        let content_type = detected_type(attachment)?;
        let extension = attachment
            .path
            .extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| format!(".{}", extension.to_ascii_lowercase()));
        let entries: Vec<_> = self
            .allowed_types
            .split([',', '|'])
            .map(|entry| entry.trim().to_ascii_lowercase())
            .filter(|entry| !entry.is_empty())
            .collect();
        Ok(entries.is_empty()
            || entries.iter().any(|entry| {
                entry == "*/*"
                    || Some(entry) == extension.as_ref()
                    || entry == content_type
                    || entry.strip_suffix('*').is_some_and(|prefix| {
                        prefix.ends_with('/') && content_type.starts_with(prefix)
                    })
            }))
    }
}

/// The media type Forgejo detects in the attachment's leading bytes.
fn detected_type(attachment: &Attachment) -> Result<&'static str> {
    let path = attachment.path.display();
    let mut head = Vec::with_capacity(sniff::SNIFF_LEN);
    std::fs::File::open(&attachment.path)
        .and_then(|file| file.take(sniff::SNIFF_LEN as u64).read_to_end(&mut head))
        .with_context(|| format!("read {path}"))?;
    Ok(sniff::media_type(&head))
}

#[cfg(test)]
mod tests;
