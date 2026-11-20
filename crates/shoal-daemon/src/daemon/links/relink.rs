//! Linking a space's carried links again, as a restored workspace or a
//! moved chat brings them: its assigned issues as whoever assigned each,
//! with the user's assignments Shoal had added, and its opened and related
//! items. The caller links its PRs.
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{
        links::issue::{AssignerOf, IssueAssignee},
        manager::Manager,
        scope::Caller,
    },
    forge::items::reference::ItemRef,
    model::{Assigner, LinkedItem, Relation, SpaceLinks},
};

impl Manager {
    /// Link `links` to the space `id` again. For the user, each assigned
    /// issue is assigned to the space as it was, by the user or an agent,
    /// and Shoal takes on the user's assignments it had added; a scoped
    /// `caller` assigns each as an agent and takes on none. Each failure
    /// becomes a warning.
    pub(crate) async fn relink_space(
        &self,
        id: &SpaceId,
        links: &SpaceLinks,
        caller: Option<&Caller>,
    ) -> Vec<String> {
        let mut warnings = Vec::new();
        let carried = match caller {
            Some(_) => &[][..],
            None => &links.assignees[..],
        };
        for issue in &links.issues {
            let (selector, input) = (SpaceSelector::id(id), issue.url.clone().into());
            let assigner = assigned_as(carried, &issue.url, caller);
            match (self.set_issue(&selector, &input, issue.title.clone(), assigner)).await {
                Ok(assigned) => warnings.extend(assigned),
                Err(error) => warnings.push(format!("cannot link issue {}: {error:#}", issue.url)),
            }
        }
        if let Err(error) = self.links.claim_assignees(id, carried.to_vec()).await {
            warnings.push(format!("cannot record the issues' assignees: {error:#}"));
        }
        warnings.extend(self.relink_recorded(id, links, caller).await);
        warnings
    }

    /// Record the opened and related items `links` carries for the space
    /// `id`: as the user's, directly, and as a scoped `caller`'s, each
    /// through the link it would make, under the checks of its scope.
    /// Returns a warning for each failure.
    async fn relink_recorded(
        &self,
        id: &SpaceId,
        links: &SpaceLinks,
        caller: Option<&Caller>,
    ) -> Vec<String> {
        let Some(caller) = caller else {
            return match self.record_items(id, recorded(links)).await {
                Ok(()) => Vec::new(),
                Err(error) => vec![format!("cannot link opened and related items: {error:#}")],
            };
        };
        let mut warnings = Vec::new();
        for (relation, item) in recorded(links) {
            let (selector, input) = (SpaceSelector::id(id), item.url.clone().into());
            let linked = self.link_item(
                &selector,
                item.kind(),
                &input,
                relation,
                item.title,
                Some(caller),
            );
            if let Err(error) = linked.await {
                warnings.push(format!("cannot link {}: {error:#}", item.url));
            }
        }
        warnings
    }
}

/// An agent where `carried` records the agent account for the issue `url`,
/// as an agent's assignment carries it, else who `caller` assigns as.
fn assigned_as(carried: &[IssueAssignee], url: &ItemRef, caller: Option<&Caller>) -> Assigner {
    let agents = (carried.iter())
        .any(|assignee| assignee.url == *url && assignee.account == Assigner::Agent);
    match agents {
        true => Assigner::Agent,
        false => Assigner::of(caller),
    }
}

/// The opened and related items to record again from `links`, each under the
/// first relation it lists: a record taken before an item had one relation
/// can also list an assigned item as opened or related, or an item as both,
/// which keeps the assignment, or else opened, as the migration does.
fn recorded(links: &SpaceLinks) -> Vec<(Relation, LinkedItem)> {
    (links.relations().into_iter())
        .filter(|(relation, item)| {
            *relation != Relation::Assigned
                && links.relations_of(&item.url).first() == Some(relation)
        })
        .collect()
}

#[cfg(test)]
mod tests;
