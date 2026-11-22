//! What each forge does its own way with uploads and downloads of attached
//! files.
use anyhow::{Context, Result};
use serde_json::Value;

use crate::{
    backends::Backend,
    capability::{self, Supports},
    items::attach::Holder,
    transport::account::Account,
    transport::{api::Request, response::Fetched},
};
use shoal_forge_types::items::attach::{Attachment, Media};
use shoal_forge_types::{ForgeRepo, backends::ForgeKind, capability::Capability};

/// How the forge uploads, serves and shows attached files.
#[shoal_forge_derive::forwarded(ForgeKind, dispatch)]
pub(crate) trait Attachments: Backend + Supports {
    /// The repository's numeric ID, where the forge's uploads name it.
    async fn repository_id(&self, _repo: &ForgeRepo, _account: &Account) -> Result<Option<u64>> {
        Ok(None)
    }

    /// Refuse `attachments` the forge or `account` cannot upload, before
    /// anything is posted. Returns whether the forge keeps uploads with the
    /// item or comment that shows them rather than with the repository.
    async fn check_uploads(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        attachments: &[Attachment],
    ) -> Result<bool>;

    /// The response to a GET of `url`, a file on the forge's web host,
    /// sent as `account`.
    async fn download(&self, _repo: &ForgeRepo, _account: &Account, _url: &str) -> Result<Fetched> {
        Err(capability::unsupported(self, Capability::Downloads))
    }

    /// The request that uploads `attachment` to `holder`.
    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        repository: Option<u64>,
    ) -> Result<Request>;

    /// Where an upload's response says the file is served.
    fn uploaded_url(&self, response: &Value) -> Result<String> {
        served_at(response, "url")
    }

    /// The Markdown that shows an uploaded image or video, or links to
    /// another file.
    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match attachment.media() {
            Media::File => link(alt, url),
            Media::Image | Media::Video => image(alt, url),
        }
    }
}

/// The URL in an upload response's `field`.
pub(in crate::backends) fn served_at(response: &Value, field: &str) -> Result<String> {
    response[field]
        .as_str()
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .context("the forge did not report where it saved the file")
}

/// The Markdown image of `url`.
pub(in crate::backends) fn image(alt: &str, url: &str) -> String {
    format!("!{}", link(alt, url))
}

/// The Markdown link to `url`.
pub(in crate::backends) fn link(text: &str, url: &str) -> String {
    format!("[{}]({url})", text.replace(['[', ']'], ""))
}
