//! How `shoal merge` merges a space's work: through the one open PR the
//! space links as assigned or opened, else, for a workspace, by merging its
//! branch into the default branch.
use anyhow::{Result, bail};
use rusqlite::Connection;
use shoal_ids::space_ids::SpaceId;

use crate::{
    daemon::{links::space::LinkSpace, manager::Manager},
    forge::items::{item::ItemState, link::ItemKind, reference::ItemRef},
    model::{MergePath, Relation},
};
use shoal_forge::items::action::ActionRequests;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// How the space `selector` names merges its work, from the state the
    /// forge reports for each PR it links as assigned or opened.
    pub async fn merge_path(&self, selector: &SpaceSelector) -> Result<MergePath> {
        let space = self.space(selector).await?;
        let workspace = space.worktree().is_some();
        let name = space.name.to_string();
        let open = match space.repository_id.is_some() {
            true => {
                self.open_own_pulls(self.own_link_space(space).await?)
                    .await?
            }
            // A chat without a repository records no links.
            false => Vec::new(),
        };
        choose(workspace, &name, open)
    }

    /// The PRs `space` links as assigned or opened that the forge reports
    /// open; one outside its repository, as after its `origin` changed, is
    /// refused, since its state there says nothing of the PR.
    async fn open_own_pulls(&self, space: LinkSpace) -> Result<Vec<ItemRef>> {
        let id = space.id.clone();
        let linked = self.store.read(move |db| own_pulls(db, &id)).await?;
        // Without one, the space needs no forge.
        if linked.is_empty() {
            return Ok(linked);
        }
        let forge = self.space_forge(&space).await?;
        let account = self.agent_account();
        let mut open = Vec::new();
        for pull in linked {
            pull.belongs_to(&forge)?;
            let current = forge
                .read(&account, ItemKind::Pr, pull.pull()?.get())
                .await?;
            if current.item.state == ItemState::Open {
                open.push(pull);
            }
        }
        Ok(open)
    }
}

/// The PRs the space `id` links as assigned or opened, each once.
fn own_pulls(db: &Connection, id: &SpaceId) -> Result<Vec<ItemRef>> {
    let mut pulls = super::wait::linked_items(db, id, Some(ItemKind::Pr))?;
    let opened = (shoal_links::linked::recorded_items(db, id, Relation::Opened)?.into_iter())
        .filter(|item| item.kind() == ItemKind::Pr.into())
        .map(|item| item.url);
    for pull in opened {
        if !pulls.contains(&pull) {
            pulls.push(pull);
        }
    }
    Ok(pulls)
}

/// How the space `name`, a workspace when `workspace`, merges, given the
/// open PRs it links: through its only one, else a workspace's branch
/// locally; a chat without one, or a space with several, is refused.
fn choose(workspace: bool, name: &str, open: Vec<ItemRef>) -> Result<MergePath> {
    match open.as_slice() {
        [pr] => Ok(MergePath::Pr { pr: pr.clone() }),
        [] if workspace => Ok(MergePath::Local),
        [] => bail!("chat {name} has no open PR to merge; pass a PR's number or URL"),
        several => {
            let urls: Vec<String> = several.iter().map(ToString::to_string).collect();
            bail!(
                "{name} has several open PRs: {}; pass the number or URL of the one to merge",
                urls.join(", ")
            )
        }
    }
}

#[cfg(test)]
mod tests;
