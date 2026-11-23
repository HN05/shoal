//! What the daemon checks and reads for the space a request about items
//! comes from: its worktree, the registered repository a name names, the
//! settings it acts with, and whether it takes changes.
use shoal_ids::space_ids::SpaceId;

use anyhow::{Context, Result, bail};
use rusqlite::Connection;
use shoal_links::space::LinkSpace;

use crate::{
    config::Effective,
    daemon::{manager::Manager, store},
    model::Repository,
};
use shoal_forge::RepoRequests;

impl Manager {
    /// The registered repository `named` names, as OWNER/NAME on its forge
    /// or by URL; none for another repository or without a name. A name two
    /// registrations answer to is refused, naming both.
    pub(crate) async fn registered_named(&self, named: Option<&str>) -> Result<Option<Repository>> {
        let Some(named) = named else {
            return Ok(None);
        };
        let mut found = Vec::new();
        for repository in self.repositories.list().await? {
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
        (self
            .repositories
            .space_settings_from(self, &config, space, Some(space.repository_id())))
        .await
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

pub(crate) use shoal_store::require_ready_space;

#[cfg(test)]
mod tests;
