//! Finding the spaces that link an issue, PR or CI run, or hold a resource.
use anyhow::{Context, Result, bail, ensure};

use crate::{
    daemon::{links::wait::linked_items, store, workspace::Manager},
    forge::{
        ForgeRepo, IssueInput,
        items::link::{self, LinkKind},
    },
    model::{Relation, SpaceTarget, Spaces},
};

impl Manager {
    /// The spaces that link an issue, PR or CI run, or the workspaces that
    /// hold a lease on a resource, optionally only among those `within`.
    pub async fn find_spaces(
        &self,
        target: &SpaceTarget,
        within: Option<Vec<String>>,
    ) -> Result<Spaces> {
        let ids = match target {
            SpaceTarget::Item { kind, input } => self.linking_spaces(*kind, input, within).await?,
            SpaceTarget::Resource { name } => self.holding_workspaces(name, within).await?,
        };
        let mut spaces = Spaces {
            workspaces: self.list_workspaces().await?,
            chats: self.chats().await?,
        };
        spaces
            .workspaces
            .retain(|workspace| ids.contains(&workspace.id));
        spaces.chats.retain(|chat| ids.contains(&chat.id));
        Ok(spaces)
    }

    async fn linking_spaces(
        &self,
        kind: LinkKind,
        input: &str,
        within: Option<Vec<String>>,
    ) -> Result<Vec<String>> {
        let label = kind.noun();
        let wanted = match IssueInput::parse(input) {
            IssueInput::Url => Some(link::item(kind, input)?),
            IssueInput::Number => None,
            IssueInput::Invalid => bail!("{label} must be a number or URL"),
        };
        let number = match &wanted {
            Some((_, number)) => *number,
            None => input
                .parse::<u64>()
                .ok()
                .filter(|number| *number > 0)
                .with_context(|| format!("invalid {label} number"))?,
        };
        let links = self
            .store
            .run(move |db| {
                let ids = db
                    .prepare("SELECT id FROM workspaces UNION ALL SELECT id FROM chats")?
                    .query_map([], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut links = Vec::new();
                for id in ids.into_iter().filter(|id| within_ids(&within, id)) {
                    // Only an issue or PR can be assigned.
                    if let Some(kind) = kind.item() {
                        for (url, _) in linked_items(db, &id, Some(kind))? {
                            links.push((id.clone(), url));
                        }
                    }
                    for relation in [Relation::Opened, Relation::Related] {
                        for item in store::recorded_items(db, &id, relation)? {
                            if item.kind == kind {
                                links.push((id.clone(), item.url));
                            }
                        }
                    }
                }
                Ok(links)
            })
            .await?;
        let mut matches: Vec<(String, ForgeRepo)> = Vec::new();
        for (id, url) in links {
            let Ok((repository, linked)) = link::item(kind, &url) else {
                continue;
            };
            let found = linked == number
                && wanted
                    .as_ref()
                    .is_none_or(|(wanted, _)| *wanted == repository);
            if found && !matches.iter().any(|(matched, _)| *matched == id) {
                matches.push((id, repository));
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

    async fn holding_workspaces(
        &self,
        name: &str,
        within: Option<Vec<String>>,
    ) -> Result<Vec<String>> {
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
                    .query_map([name], |row| row.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            })
            .await?;
        ids.retain(|id| within_ids(&within, id));
        ensure!(!ids.is_empty(), "no workspace holds resource {label}");
        Ok(ids)
    }
}

/// Whether a lookup confined to the spaces `within`, or to none, covers `id`.
fn within_ids(within: &Option<Vec<String>>, id: &str) -> bool {
    within
        .as_ref()
        .is_none_or(|within| within.iter().any(|within| within == id))
}
