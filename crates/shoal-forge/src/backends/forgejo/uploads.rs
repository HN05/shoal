//! The instance's attachment settings, checked before a comment or item is
//! posted, since Forgejo takes its files only once it exists.
use std::io::Read;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

use super::sniff;
use crate::{ForgeRepo, account::Account, items::attach::Attachment, transport::api::Request};

/// What `GET /settings/attachment` reports.
#[derive(Debug, Deserialize)]
pub(super) struct Settings {
    enabled: bool,
    /// Comma-separated extensions (`.png`), media types (`image/png`) and
    /// wildcards (`image/*`, `*/*`); none allows every file.
    #[serde(default)]
    allowed_types: String,
    /// In MiB.
    max_size: u64,
}

impl Settings {
    pub(super) async fn read(repo: &ForgeRepo, account: &Account) -> Result<Self> {
        let response = repo
            .send(account, &Request::get("settings/attachment".to_owned()))
            .await
            .context("read the forge's attachment settings")?;
        serde_json::from_value(response).context("read the forge's attachment settings")
    }

    /// Refuse an attachment the instance would refuse to upload.
    pub(super) fn check(&self, attachment: &Attachment) -> Result<()> {
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
