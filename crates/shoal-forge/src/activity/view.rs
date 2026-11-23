//! An issue's or PR's content, status and discussion, read on request so
//! callers need not know which forge serves it.
use anyhow::Result;

use crate::{
    activity::status::StatusRequests,
    capability::CapabilityRequests,
    items::{discussion::Views, merging::Merging, reaction::ReactionRequests},
    transport::account::Account,
};
use shoal_forge_types::{
    ForgeRepo,
    activity::view::{Details, ItemView},
    capability::Capability,
    items::link::ItemKind,
    numbers::PullNumber,
};

/// Reading items as views.
#[shoal_forge_derive::extension(pub ViewRequests, pub(crate) ViewSteps)]
impl ForgeRepo {
    /// The item at `url` with its discussion when `comments` is set; PRs also
    /// carry the state `shoal status` reports.
    pub async fn view(
        &self,
        account: &Account,
        kind: ItemKind,
        item: &shoal_forge_types::items::reference::ItemRef,
        comments: bool,
    ) -> ItemView {
        let mut errors = Vec::new();
        let details = match self
            .details(account, kind, item, comments, &mut errors)
            .await
        {
            Ok(details) => Some(details),
            Err(error) => {
                errors.insert(0, format!("{error:#}"));
                None
            }
        };
        ItemView {
            url: item.clone(),
            kind,
            details,
            check_logs: Vec::new(),
            errors,
        }
    }

    async fn details(
        &self,
        account: &Account,
        kind: ItemKind,
        item: &shoal_forge_types::items::reference::ItemRef,
        comments: bool,
        errors: &mut Vec<String>,
    ) -> Result<Details> {
        item.belongs_to(self)?;
        let number = item.number().get();
        let forge = self.kind();
        let content = forge.view_details(self, account, kind, number, comments);
        let status = async {
            match kind {
                ItemKind::Pr => Some(tokio::join!(
                    self.pr_status(account, item),
                    forge.behind(self, account, PullNumber::new(number))
                )),
                ItemKind::Issue => None,
            }
        };
        let (content, status) = tokio::join!(content, status);
        let mut details = content?;
        if let (Some(pr), Some((status, behind))) = (&mut details.pr, status) {
            pr.record(status, errors);
            match behind {
                Ok(behind) => pr.behind = behind,
                Err(error) => errors.push(format!("behind lookup failed: {error:#}")),
            }
        }
        if comments {
            let discussion = self
                .kind()
                .view_discussion(self, account, kind, number, &mut details, errors)
                .await;
            match discussion {
                Ok(Some(reviews)) => details.reviews = Some(reviews),
                Ok(None) => {}
                Err(error) => errors.push(format!("comments lookup failed: {error:#}")),
            }
            if self.supports(Capability::Reactions)
                && let Err(error) = self.view_reactions(account, kind, &mut details).await
            {
                errors.push(format!("reactions lookup failed: {error:#}"));
            }
        }
        Ok(details)
    }
}

#[cfg(test)]
mod tests;
