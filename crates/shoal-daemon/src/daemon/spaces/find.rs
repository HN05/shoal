//! Finding the spaces that link an issue, PR or CI run, or hold a resource.
use anyhow::{Result, ensure};
use shoal_ids::space_ids::SpaceId;

use crate::{
    daemon::{links::wait::linked_items, manager::Manager, store},
    forge::{
        ForgeRepo,
        items::{link::LinkKind, reference::ItemInput},
    },
    model::{Relation, Space, SpaceTarget},
};

impl Manager {
    /// The spaces among `among` that link an issue, PR or CI run, or the
    /// workspaces among them that hold a lease on a resource, in their
    /// order there.
    pub async fn find_spaces(&self, target: &SpaceTarget, among: Vec<Space>) -> Result<Vec<Space>> {
        let within: Vec<SpaceId> = among.iter().map(|space| space.id.clone()).collect();
        let ids = match target {
            SpaceTarget::Item { kind, input } => self.linking_spaces(*kind, input, within).await?,
            SpaceTarget::Resource { name } => self.holding_workspaces(name, within).await?,
        };
        Ok((among.into_iter())
            .filter(|space| ids.contains(&space.id))
            .collect())
    }

    async fn linking_spaces(
        &self,
        kind: LinkKind,
        input: &ItemInput,
        within: Vec<SpaceId>,
    ) -> Result<Vec<SpaceId>> {
        let label = kind.noun();
        let (wanted, number) = match input {
            ItemInput::Url(item) => {
                item.of_kind(kind)?;
                (Some(item.repository().clone()), item.number().get())
            }
            ItemInput::Number(number) => (None, *number),
        };
        let links = self
            .store
            .run(move |db| {
                let mut links = Vec::new();
                for id in within {
                    // Only an issue or PR can be assigned.
                    if let Some(kind) = kind.item() {
                        for url in linked_items(db, &id, Some(kind))? {
                            links.push((id.clone(), url));
                        }
                    }
                    for relation in [Relation::Opened, Relation::Related] {
                        for item in store::recorded_items(db, &id, relation)? {
                            if item.kind() == kind {
                                links.push((id.clone(), item.url));
                            }
                        }
                    }
                }
                Ok(links)
            })
            .await?;
        let mut matches: Vec<(SpaceId, ForgeRepo)> = Vec::new();
        for (id, url) in links {
            let repository = url.repository();
            let found = url.number().get() == number
                && wanted.as_ref().is_none_or(|wanted| wanted == repository);
            if found {
                matches.push((id, repository.clone()));
            }
        }
        ensure!(!matches.is_empty(), "no space links {label} {input}");
        ensure!(
            matches
                .iter()
                .all(|(_, repository)| *repository == matches[0].1),
            "{label} {input} is linked in several repositories; pass its URL"
        );
        Ok(matches.into_iter().map(|(id, _)| id).collect())
    }

    async fn holding_workspaces(&self, name: &str, within: Vec<SpaceId>) -> Result<Vec<SpaceId>> {
        let name = name.to_owned();
        let label = name.clone();
        let mut ids = self
            .store
            .run(move |db| {
                Ok(db
                    .prepare(
                        "SELECT DISTINCT workspace_id FROM resource_leases
                         WHERE pool=?1 OR resource=?1",
                    )?
                    .query_map([name], |row| row.get::<_, SpaceId>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        ids.retain(|id| within.contains(id));
        ensure!(!ids.is_empty(), "no workspace holds resource {label}");
        Ok(ids)
    }
}
