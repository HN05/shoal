//! Bitbucket's uploads and downloads of attached files.
use anyhow::Result;

use super::Bitbucket;
use crate::{
    ForgeRepo,
    account::Account,
    backends::Attachments,
    capability::{self, Capability},
    items::attach::{Attachment, Holder},
    transport::api::Request,
};

impl Attachments for Bitbucket {
    async fn check_uploads(
        &self,
        _repo: &ForgeRepo,
        _account: &Account,
        _attachments: &[Attachment],
    ) -> Result<bool> {
        Err(capability::unsupported(self, Capability::Attachments))
    }

    fn upload(
        &self,
        _repo: &ForgeRepo,
        _holder: Holder,
        _attachment: &Attachment,
        _repository: Option<u64>,
    ) -> Result<Request> {
        Err(capability::unsupported(self, Capability::Attachments))
    }
}
