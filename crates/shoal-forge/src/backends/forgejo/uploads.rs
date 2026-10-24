//! The instance's attachment settings, checked before a comment or item is
//! posted, since Forgejo takes its files only once it exists.
use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;

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
        if !self.allows(attachment) {
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

    /// Whether an entry allows the attachment's extension or media type.
    /// Forgejo matches the type it detects in the file; the type named by its
    /// extension stands in for it.
    fn allows(&self, attachment: &Attachment) -> bool {
        let content_type = attachment.content_type();
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
        entries.is_empty()
            || entries.iter().any(|entry| {
                entry == "*/*"
                    || Some(entry) == extension.as_ref()
                    || entry == content_type
                    || entry.strip_suffix('*').is_some_and(|prefix| {
                        prefix.ends_with('/') && content_type.starts_with(prefix)
                    })
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(enabled: bool, allowed_types: &str, max_size: u64) -> Settings {
        Settings {
            enabled,
            allowed_types: allowed_types.to_owned(),
            max_size,
        }
    }

    #[test]
    fn settings_refuse_disabled_uploads_other_types_and_large_files() {
        let dir = tempfile::tempdir().unwrap();
        let write = |name: &str, len: u64| {
            let path = dir.path().join(name);
            std::fs::File::create(&path).unwrap().set_len(len).unwrap();
            Attachment { path, alt: None }
        };
        let png = write("a.PNG", 10);
        let svg = write("b.svg", 10);
        let video = write("c.mp4", 2 * 1024 * 1024);
        let error = |settings: Settings, attachment: &Attachment| {
            format!("{:#}", settings.check(attachment).unwrap_err())
        };
        for allowed in ["", "*/*", ".png", "image/png", "image/*", ".txt|image/*"] {
            assert!(settings(true, allowed, 1).check(&png).is_ok(), "{allowed}");
        }
        assert!(error(settings(false, "", 1), &png).contains("does not accept"));
        assert!(error(settings(true, ".png,.mp4", 1), &svg).contains("only these"));
        assert!(error(settings(true, "image/", 1), &png).contains("only these"));
        assert!(error(settings(true, "", 1), &video).contains("at most 1 MiB"));
        assert!(settings(true, "video/*", 2).check(&video).is_ok());
    }
}
