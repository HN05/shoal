//! GitHub's uploads and downloads of attached files.
use anyhow::{Context, Result, ensure};

use super::GitHub;
use crate::{
    items::attach::{Attachments, Holder, image, link},
    transport::{
        account::Account,
        api::{Request, TransportRequests, Upload, encode},
        download,
        response::Fetched,
    },
};
use shoal_forge_types::{
    ForgeRepo,
    items::attach::{Attachment, Media},
};

impl Attachments for GitHub {
    /// GitHub uploads name the repository by its numeric ID.
    async fn repository_id(&self, repo: &ForgeRepo, account: &Account) -> Result<Option<u64>> {
        let repository = repo
            .send(account, &Request::get(format!("repos/{}", repo.path)))
            .await?;
        repository["id"]
            .as_u64()
            .context("GitHub did not report the repository's ID")
            .map(Some)
    }

    async fn check_uploads(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _attachments: &[Attachment],
    ) -> Result<bool> {
        Ok(false)
    }

    /// curl sends the token only to the host a URL names, so the storage
    /// an attachment redirects to gets none.
    async fn download(&self, repo: &ForgeRepo, account: &Account, url: &str) -> Result<Fetched> {
        let authorization = account.authorization(repo).await?;
        download::with_curl(url, authorization).await
    }

    /// GitHub's REST API has no upload endpoint, so this uses the one gh
    /// itself uses.
    fn upload(
        &self,
        repo: &ForgeRepo,
        holder: Holder,
        attachment: &Attachment,
        repository: Option<u64>,
    ) -> Result<Request> {
        ensure!(
            holder == Holder::Repository,
            "GitHub cannot attach files to {holder:?}"
        );
        let repository = repository.context("GitHub uploads need the repository's ID")?;
        let content_type = attachment.content_type();
        let name = encode(attachment.name(), false);
        let content = encode(content_type, false);
        Ok(Request::upload(
            format!(
                "https://uploads.{}/user-attachments/assets?name={name}&content_type={content}&repository_id={repository}",
                repo.host
            ),
            Upload {
                path: attachment.path.clone(),
                content_type,
                field: None,
            },
        ))
    }

    /// GitHub shows a video's bare URL as a player.
    fn markup(&self, attachment: &Attachment, alt: &str, url: &str) -> String {
        match attachment.media() {
            Media::Video => url.to_owned(),
            Media::Image => image(alt, url),
            Media::File => link(alt, url),
        }
    }
}
