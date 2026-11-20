//! The records an absorbed space hands over: its links, attached files and
//! open threads go to the space that takes its work, and the requests
//! bound to its branch and commit are withdrawn. Issue proposals belong to
//! the repository and stay.
use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_links::linked;

use super::parties::Parties;
use crate::{
    daemon::{links::pr::Action, manager::Manager, scope::Caller},
    forge::items::link::LinkKind,
    model::{Relation, SpaceLinks},
};
use shoal_protocol::{
    approvals::{DecisionStatus, Target},
    item_numbers::ItemNumber,
    space_names::SpaceSelector,
};

/// What the space that takes the work took over.
pub(super) struct TakenOver {
    pub(super) links: SpaceLinks,
    pub(super) files: Vec<String>,
    pub(super) threads: Vec<ItemNumber>,
    pub(super) withdrawn: Vec<ItemNumber>,
    pub(super) warnings: Vec<String>,
}

impl Manager {
    /// Hand the target's records over to the space that takes its work,
    /// linking its items there as `caller` may.
    pub(super) async fn take_over(
        &self,
        parties: &Parties,
        caller: Option<&Caller>,
    ) -> Result<TakenOver> {
        let (from, to) = (&parties.target.id, &parties.into.id);
        let withdrawn = self.withdraw_branch_requests(from).await?;
        let (links, warnings) = self.move_links(from, to, caller).await?;
        let files = self.copy_attached_files(from, to).await?;
        let copies = (files.iter()).map(|(old, file)| (*old, file.id)).collect();
        let threads = self.move_open_threads(from, to, copies).await?;
        let files = files.into_iter().map(|(_, file)| file.name).collect();
        Ok(TakenOver {
            links,
            files,
            threads,
            withdrawn,
            warnings,
        })
    }

    /// Withdraw the pending PR proposals and push requests of the space
    /// `id`, which its branch and commit bind, returning their numbers.
    async fn withdraw_branch_requests(&self, id: &SpaceId) -> Result<Vec<ItemNumber>> {
        let selector = SpaceSelector::id(id);
        let requests = self.approval_requests(Some(&selector)).await?;
        let mut withdrawn = Vec::new();
        for request in requests {
            let bound = matches!(request.target, Target::PrOpen | Target::Push);
            if request.space_id == *id && request.status == DecisionStatus::Pending && bound {
                let request = self.withdraw_approval(&selector, request.number).await?;
                withdrawn.push(request.number);
            }
        }
        Ok(withdrawn)
    }

    /// Unlink the items of the space `from` and link them to the space `to`
    /// as they were, returning them with a warning for each that could not
    /// be linked there.
    async fn move_links(
        &self,
        from: &SpaceId,
        to: &SpaceId,
        caller: Option<&Caller>,
    ) -> Result<(SpaceLinks, Vec<String>)> {
        let id = from.clone();
        let links = (self.store)
            .run(move |db| Ok(linked::space_links(db, &id)?))
            .await?;
        self.unlink_all(from, &links).await?;
        let mut warnings = self.relink_space(to, &links, caller).await;
        for pr in &links.prs {
            let url = pr.url.clone().into();
            let watch = Action::Watch {
                url,
                title: pr.title.clone(),
            };
            if let Err(error) = self.set_pr(&SpaceSelector::id(to), watch).await {
                warnings.push(format!("cannot link PR {}: {error:#}", pr.url));
            }
        }
        Ok((links, warnings))
    }

    /// Unlink every item `links` lists from the space `id`.
    async fn unlink_all(&self, id: &SpaceId, links: &SpaceLinks) -> Result<()> {
        let selector = SpaceSelector::id(id);
        let linked = [
            (Relation::Assigned, Some(LinkKind::Issue), &links.issues),
            (Relation::Assigned, Some(LinkKind::Pr), &links.prs),
            (Relation::Opened, None, &links.opened),
            (Relation::Related, None, &links.related),
        ];
        for (relation, kind, items) in linked {
            if !items.is_empty() {
                self.unlink_items(&selector, relation, kind, None).await?;
            }
        }
        Ok(())
    }
}
