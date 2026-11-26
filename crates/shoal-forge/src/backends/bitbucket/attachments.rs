use anyhow::Result;

use super::Bitbucket;
use crate::{
    capability,
    items::attach::{Attachments, Holder},
    transport::{account::Account, api::Request},
};
use shoal_forge_types::{ForgeRepo, capability::Capability, items::attach::Attachment};

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
