//! The workspace a launch's new workspace copies: a live one, or with a
//! checkpoint of its own a removed one.
use anyhow::{Context as _, Result};
use shoal_ids::repository_ids::RepositoryId;
use shoal_protocol::launches::{Launch, SpaceChoice};
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    daemon::{manager::Manager, scope::Caller},
    protocol::CopySource,
    start_plan::CopyCreation,
};

/// What a launch copies, and the repository of the copied workspace.
pub(in crate::daemon::launches) struct Copied {
    pub creation: CopyCreation,
    pub repository: RepositoryId,
}

impl Manager {
    /// What `launch` copies: the workspace `--copy` names by number or ID,
    /// else the one the start was made in, live, or removed for a copy of
    /// one of its checkpoints; `None` when it asks for no copy.
    pub(in crate::daemon::launches) async fn launch_copy(
        &self,
        launch: &Launch,
        caller: Option<&Caller>,
    ) -> Result<Option<Copied>> {
        if launch.copy.is_none() && launch.checkpoint.is_none() {
            return Ok(None);
        }
        let selector = match launch.copy.as_ref().and_then(SpaceChoice::named) {
            Some(selector) => selector.clone(),
            None => {
                SpaceSelector::Id((self.launch_directory_workspace(launch).await?).context(
                    "the start was made in no workspace; name the one to copy with --copy",
                )?)
            }
        };
        let live = (self
            .coordination
            .visible_spaces(self, caller)
            .await?
            .into_iter())
        .filter_map(|space| space.into_workspace().ok())
        .find(|workspace| selector.selects(&workspace.id, workspace.number));
        let (workspace, repository, named) = match live {
            Some(workspace) => (
                workspace.id.clone(),
                workspace.repository().clone(),
                workspace.branch_or_name().to_owned(),
            ),
            None if launch.checkpoint.is_some() => self.removed_copy(&selector, caller).await?,
            None => anyhow::bail!("no workspace {selector}"),
        };
        let source = CopySource {
            workspace: SpaceSelector::id(&workspace),
            checkpoint: launch.checkpoint,
        };
        Ok(Some(Copied {
            creation: CopyCreation {
                source,
                name: format!("{named}-copy"),
            },
            repository,
        }))
    }

    /// The ID of the removed workspace `selector` selects by ID, of its
    /// repository, and its branch or else its name.
    async fn removed_copy(
        &self,
        selector: &SpaceSelector,
        caller: Option<&Caller>,
    ) -> Result<(shoal_ids::space_ids::SpaceId, RepositoryId, String)> {
        let removed = (self.restorable_spaces(caller).await?.into_iter())
            .find(|removed| selector.given_id() == Some(&removed.space_id))
            .with_context(|| format!("no workspace or removed workspace {selector}"))?;
        let head = &removed.kept_workspace()?.head;
        let named = (head.branch()).map_or_else(|| removed.name.to_string(), ToString::to_string);
        let repository = removed.repository()?.clone();
        Ok((removed.space_id, repository, named))
    }
}
