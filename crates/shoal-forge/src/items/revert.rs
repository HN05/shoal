//! Revert a merged PR through a forge that opens the revert itself.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    account::Account,
    backends::Backend,
    capability::Capability,
    items::{
        action::Edit,
        attach::Attachment,
        create::{Created, pending_edit},
        item::{Item, ItemState, PrSummary},
        link::ItemKind,
    },
};

/// The PR that undoes a merged one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct NewRevert {
    pub title: String,
    pub body: String,
    pub draft: bool,
    pub labels: Vec<String>,
    pub reviewers: Vec<String>,
    pub attachments: Vec<Attachment>,
}

impl NewRevert {
    /// The capabilities the forge needs to open the revert, besides
    /// attachments.
    fn needs(&self) -> Vec<Capability> {
        let labels = (!self.labels.is_empty()).then_some(Capability::Labels);
        let draft = self.draft.then_some(Capability::DraftPrs);
        [Capability::RevertPrs]
            .into_iter()
            .chain(labels)
            .chain(draft)
            .collect()
    }
}

impl ForgeRepo {
    /// Have the forge open `revert` for the merged PR `number`; only failing
    /// to open it fails.
    pub async fn revert_pull(
        &self,
        account: &Account,
        number: u64,
        revert: &NewRevert,
    ) -> Result<Created> {
        self.require_all(revert.needs())?;
        let current = self.read(account, ItemKind::Pr, number).await?;
        ensure!(
            current.item.state == ItemState::Merged,
            "PR #{number} has not merged, so there is nothing to revert"
        );
        let base = current
            .item
            .pr
            .as_ref()
            .context("only PRs revert")?
            .base
            .clone();
        let per_item = self.check_uploads(account, &revert.attachments).await?;
        let (body, pending) = self
            .new_body(account, &revert.body, &revert.attachments, per_item)
            .await?;
        let posted = NewRevert {
            body,
            ..revert.clone()
        };
        let (number, head) = self
            .kind
            .revert(self, account, &current, &posted)
            .await
            .with_context(|| format!("could not revert PR #{number}"))?;
        let posted = Item {
            kind: ItemKind::Pr,
            number,
            url: self.item_url(ItemKind::Pr, number)?,
            title: posted.title,
            state: ItemState::Open,
            labels: Vec::new(),
            pr: Some(PrSummary {
                head,
                base,
                draft: posted.draft,
            }),
        };
        let follow_up = Edit {
            add_labels: revert.labels.clone(),
            add_reviewers: revert.reviewers.clone(),
            ..pending_edit(pending)
        };
        Ok(self.finish_posted(account, posted, &follow_up).await)
    }
}

#[cfg(test)]
mod tests;
