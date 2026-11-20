//! Finding the space a request about items comes from, and what it reads
//! of the space's repository.
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use shoal_links::space::LinkSpace;

use crate::{
    config::Effective,
    daemon::{manager::Manager, store},
    forge::ForgeRepo,
    model::{Repository, Space},
    protocol::ItemContext,
};
use shoal_forge::RepoRequests;
use shoal_protocol::space_names::SpaceSelector;

impl Manager {
    /// The workspace or running chat `selector` names, by ID or name.
    pub async fn link_space(&self, selector: &SpaceSelector) -> Result<LinkSpace> {
        self.own_link_space(self.space(selector).await?).await
    }

    /// `space` acting in its own repository, reading its `origin` in the
    /// registered checkout while its worktree is still to be created.
    pub(crate) async fn own_link_space(&self, space: Space) -> Result<LinkSpace> {
        let pending = (space.worktree()).is_some_and(|worktree| worktree.checkout.is_pending());
        let linked = LinkSpace::own(space)?;
        if !pending {
            return Ok(linked);
        }
        let checkout = self.repository(linked.repository_id()).await?.path;
        Ok(linked.origin_in(checkout))
    }

    /// The space `context` names, a workspace or a chat, acting in its own
    /// repository or, for a chat without one, in the registered repository
    /// it names; a registered checkout is none.
    pub(crate) async fn context_space(&self, context: &ItemContext) -> Result<LinkSpace> {
        match context {
            ItemContext::Space {
                space,
                repository: None,
            } => self.link_space(space).await,
            ItemContext::Space {
                space,
                repository: Some(repository),
            } => self.space_acting_in(space, repository).await,
            ItemContext::Checkout(_) => bail!("a registered checkout is no space"),
        }
    }

    /// The space `space` names acting in the registered repository
    /// `repository` names: its own, or for a space without one, a chat, any,
    /// which then counts as its own. A space with a repository acts only in
    /// that one.
    async fn space_acting_in(
        &self,
        space: &SpaceSelector,
        repository: &RepositoryId,
    ) -> Result<LinkSpace> {
        let space = self.space(space).await?;
        match &space.repository_id {
            Some(own) => {
                anyhow::ensure!(
                    own == repository,
                    "space {} acts only in its own repository",
                    space.name
                );
                self.own_link_space(space).await
            }
            None => Ok(LinkSpace::acting_in(
                space,
                self.repository(repository).await?,
            )),
        }
    }

    /// The space `id`, while it exists, acting in the registered repository
    /// `repository` when it is a chat without a repository.
    pub(crate) async fn find_link_space(
        &self,
        id: &SpaceId,
        repository: Option<&RepositoryId>,
    ) -> Result<Option<LinkSpace>> {
        let owned = id.to_owned();
        let kind = (self.store)
            .run(move |db| crate::daemon::spaces::space_kind(db, &owned))
            .await?;
        if kind.is_none() {
            return Ok(None);
        }
        let outside = (self.chat_by_id(id).await?).is_some_and(|chat| chat.repository_id.is_none());
        let space = SpaceSelector::id(id);
        match repository.filter(|_| outside) {
            Some(repository) => self.space_acting_in(&space, repository).await.map(Some),
            None => self.link_space(&space).await.map(Some),
        }
    }

    /// Verify a workspace's worktree before a request relies on it; a
    /// chat owns no worktree to verify, nor does a workspace whose worktree
    /// is still to be created, which reads its forge in the checkout.
    pub(crate) async fn verify_space(&self, space: &Space) -> Result<()> {
        match space.to_workspace() {
            Some(workspace) if !workspace.checkout.unborn() => {
                self.verify_worktree(&workspace).await
            }
            _ => Ok(()),
        }
    }

    /// The registered repository `named` names, as OWNER/NAME on its forge
    /// or by URL; none for another repository or without a name. A name two
    /// registrations answer to is refused, naming both.
    pub(crate) async fn registered_named(&self, named: Option<&str>) -> Result<Option<Repository>> {
        let Some(named) = named else {
            return Ok(None);
        };
        let mut found = Vec::new();
        for repository in self.repositories().await? {
            if self.answers_to(&repository, named).await? {
                found.push(repository);
            }
        }
        ensure_one(named, &found)?;
        Ok(found.pop())
    }

    /// Whether the registered `repository` is the one `named` names: never
    /// for a checkout without an origin, while any other failure to tell
    /// refuses rather than passing for another repository.
    async fn answers_to(&self, repository: &Repository, named: &str) -> Result<bool> {
        let path = &repository.path;
        let remote = self
            .file_access
            .checked(
                &self.notifications,
                path,
                crate::repository::remote_url_from_path(path),
            )
            .await?;
        let Some(remote) = remote else {
            return Ok(false);
        };
        let matches = async {
            let own = self.forge(&remote).await?;
            anyhow::Ok(own.resolve_named(named, &self.forge_hosts()).await? == own)
        };
        (matches.await).with_context(|| {
            let name = repository.display_name();
            format!("cannot tell whether {named} is the registered repository {name}")
        })
    }

    /// The settings of the space in the repository it acts in.
    pub(crate) async fn acting_settings(&self, space: &LinkSpace) -> Result<Effective> {
        let config = self.config();
        (self.space_settings_from(&config, space, Some(space.repository_id()))).await
    }

    /// The forge of the space's `origin`.
    pub(crate) async fn space_forge(&self, space: &LinkSpace) -> Result<ForgeRepo> {
        self.origin_forge(space.origin_path()).await
    }
}

/// Refuse a name `found` lists several registered repositories for,
/// naming each.
fn ensure_one(named: &str, found: &[Repository]) -> Result<()> {
    if found.len() > 1 {
        let names: Vec<String> = (found.iter())
            .map(|repo| format!("{} ({})", repo.display_name(), repo.id))
            .collect();
        bail!(
            "{named} names several registered repositories: {}; name one by its URL",
            names.join(", ")
        );
    }
    Ok(())
}

/// Whether `id` names a recorded chat, whose links change whenever it runs.
pub(crate) fn is_chat(db: &Connection, id: &SpaceId) -> Result<bool> {
    store::exists(db, "SELECT 1 FROM spaces WHERE id=?1 AND kind='chat'", [id])
}

/// Refuse a change to the links, cursor or marks of a workspace that is not
/// ready; a chat takes one whenever it is recorded.
pub(crate) fn require_ready_space(db: &Connection, id: &SpaceId) -> Result<()> {
    if is_chat(db, id)? {
        return Ok(());
    }
    store::require_ready(db, id)
}

#[cfg(test)]
mod tests;
