//! The spaces a package carries with its space: which go with it, and what
//! the package holds of each once its removal recorded it.
use crate::model::EventCause;
use anyhow::Result;

use crate::{
    daemon::manager::Manager,
    model::{RemovedSpace, Repository, Space},
    transfer::{CarriedSpace, CarriedSpaceExt, Carrying, Manifest, ManifestExt, Placed},
};
use shoal_ids::space_ids::SpaceId;

impl Manager {
    /// The spaces that go with the space `id` as `carrying` says, each
    /// after its parent and the workspace whose worktree it works in.
    pub(crate) async fn carried_by(&self, id: &SpaceId, carrying: &Carrying) -> Result<Vec<Space>> {
        let spaces = self.present_spaces().await?;
        let placed: Vec<Placed<'_>> = spaces.iter().map(Placed::space).collect();
        let carried = crate::transfer::carried_spaces(&placed, id, carrying);
        Ok((carried.iter())
            .filter_map(|id| spaces.iter().find(|space| &space.id == id))
            .cloned()
            .collect())
    }

    /// The settings describing the agents of each of `spaces`, read while
    /// they exist, since each one's own config may describe its agents.
    pub(super) async fn agent_settings_of(
        &self,
        spaces: impl Iterator<Item = &Space>,
    ) -> Vec<Result<crate::config::Effective>> {
        let mut settings = Vec::new();
        for space in spaces {
            settings.push(self.repositories.space_settings(self, space).await);
        }
        settings
    }

    /// What the removal of `space`, whose agents stopped, would record of
    /// it, a workspace at a new checkpoint with its pending review, for a
    /// move that ends it only once another machine imported it.
    pub(super) async fn moving_record(&self, space: &Space) -> Result<RemovedSpace> {
        let checkpoint = self.move_checkpoint(space).await?;
        let mut record =
            (self.present_removal_record(&space.id, checkpoint, EventCause::Manual)).await?;
        self.carry_review(&mut record).await?;
        Ok(record)
    }

    /// Keep in `record`, of a space that goes on elsewhere, a workspace's
    /// queued or running review, which its import queues again.
    pub(super) async fn carry_review(&self, record: &mut RemovedSpace) -> Result<()> {
        let review = self.reviews.pending(&record.space_id).await?;
        if let Ok(kept) = record.kept_workspace_mut() {
            kept.pending_review = review;
        }
        Ok(())
    }

    /// The package, without its session files, of the space `record`
    /// records with the spaces `carried` records, spaces of `repos` or of
    /// none.
    pub(super) async fn manifest_of(
        &self,
        repos: &[Repository],
        record: RemovedSpace,
        carried: Vec<RemovedSpace>,
    ) -> Result<Manifest> {
        let repo =
            (record.repository_id.as_ref()).and_then(|id| repos.iter().find(|repo| &repo.id == id));
        let mut manifest = Manifest::of(repo, record).await?;
        manifest.carried = self.carried_packages(repos, carried).await?;
        Ok(manifest)
    }

    /// What a package of spaces of `repos` holds of the spaces `records`
    /// records.
    async fn carried_packages(
        &self,
        repos: &[Repository],
        records: Vec<RemovedSpace>,
    ) -> Result<Vec<CarriedSpace>> {
        let machine = crate::process::machine::id();
        let mut carried = Vec::new();
        for record in records {
            let repo = (record.repository_id.as_ref())
                .and_then(|id| repos.iter().find(|repo| &repo.id == id));
            let remote_url = match repo {
                Some(repo) => crate::repository::remote_url_from_source(&repo.source).await?,
                None => None,
            };
            carried.push(CarriedSpace::of(remote_url, machine.clone(), record));
        }
        Ok(carried)
    }
}

/// Refuse to package `space` with the spaces `carried` when one of them is
/// a workspace whose removal keeps no record.
pub(super) fn ensure_packable(space: &Space, carried: &[Space]) -> Result<()> {
    std::iter::once(space)
        .chain(carried)
        .try_for_each(super::worktree::ensure_packable)
}

/// The records among `records` of the spaces the removal of the space `id`
/// took, each after the records it waits for, when `carrying` carries the
/// spaces below it; a selection leaves them here, below the space's record.
pub(super) fn taken_by_removal(
    records: &[RemovedSpace],
    id: &SpaceId,
    carrying: &Carrying,
) -> Vec<RemovedSpace> {
    if let Carrying::Selected(_) = carrying {
        return Vec::new();
    }
    let taken: Vec<&RemovedSpace> = (records.iter())
        .filter(|record| record.cause == EventCause::Parent)
        .collect();
    let placed: Vec<Placed<'_>> = taken.iter().map(|record| Placed::removed(record)).collect();
    let carried = crate::transfer::carried_spaces(&placed, id, carrying);
    (carried.iter())
        .filter_map(|id| taken.iter().find(|record| &record.space_id == id))
        .map(|record| (*record).clone())
        .collect()
}
