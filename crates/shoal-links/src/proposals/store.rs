//! The stored approval requests of proposals, each read or change in a
//! transaction of its own.
use anyhow::Result;
use shoal_daemon_base::item_numbers::{self, ItemKind, ItemNumber};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::approvals::ApprovalRequest;

use crate::{Links, daemon::ProposalKind};

impl Links {
    /// Record `request` as the proposal of `kind`, or find the identical
    /// one still pending, returning it and whether this recorded it.
    pub async fn store_proposal(
        &self,
        request: ApprovalRequest,
        kind: ProposalKind,
    ) -> Result<(ApprovalRequest, bool)> {
        let records = self.records.clone();
        (self.store)
            .write(move |tx| records.record_proposal(tx, request, kind))
            .await
    }

    /// Store the changes made to the pending proposal `request`.
    pub async fn save_proposal(&self, request: ApprovalRequest) -> Result<()> {
        let records = self.records.clone();
        (self.store)
            .write(move |tx| records.save_proposal(tx, &request))
            .await
    }

    /// Journal the decided proposal `request` and forget it.
    pub async fn forget_proposal(&self, request: &ApprovalRequest) -> Result<()> {
        let (request, records) = (request.clone(), self.records.clone());
        (self.store)
            .write(move |tx| records.forget_proposal(tx, &request))
            .await
    }

    /// Keep `request` as outdated, so it is no longer decided.
    pub(crate) async fn retire_approval(&self, request: ApprovalRequest) -> Result<()> {
        let records = self.records.clone();
        (self.store)
            .write(move |tx| records.retire_approval(tx, &request))
            .await
    }

    pub async fn approval_request(&self, number: ItemNumber) -> Result<Option<ApprovalRequest>> {
        let records = self.records.clone();
        (self.store)
            .read(move |db| records.approval_by_number(db, number))
            .await
    }

    /// The approval request numbered `number`, refusing a number no request
    /// has.
    pub(crate) async fn show_approval(&self, number: ItemNumber) -> Result<ApprovalRequest> {
        let records = self.records.clone();
        (self.store)
            .read(move |db| match records.approval_by_number(db, number)? {
                Some(request) => Ok(request),
                None => Err(item_numbers::unknown(db, ItemKind::Approval, number)?),
            })
            .await
    }

    pub(crate) async fn approval_by_key(&self, key: &str) -> Result<Option<ApprovalRequest>> {
        let (key, records) = (key.to_owned(), self.records.clone());
        (self.store)
            .read(move |db| records.approval_by_key(db, &key))
            .await
    }

    /// The approval requests of the space `space`, or of every space.
    pub(crate) async fn approval_requests(
        &self,
        space: Option<&SpaceId>,
    ) -> Result<Vec<ApprovalRequest>> {
        let (space, records) = (space.cloned(), self.records.clone());
        (self.store)
            .read(move |db| records.approval_requests(db, space.as_ref()))
            .await
    }
}
