//! The spaces a package carries with its space, and what the package holds
//! of each once its removal recorded it.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::model::EventCause;
use anyhow::Result;

use crate::{
    model::{RemovedSpace, Repository, Space},
    transfer::{CarriedSpace, CarriedSpaceExt, Carrying, Manifest, ManifestExt, Placed},
};
use shoal_ids::space_ids::SpaceId;

impl Spaces {
    /// Each after its parent and the workspace whose worktree it works in.
    pub async fn carried_by(
        &self,
        daemon: &dyn Daemon,
        id: &SpaceId,
        carrying: &Carrying,
    ) -> Result<Vec<Space>> {
        let spaces = self.present_spaces(daemon).await?;
        let placed: Vec<Placed<'_>> = spaces.iter().map(Placed::space).collect();
        let carried = crate::transfer::carried_spaces(&placed, id, carrying);
        Ok((carried.iter())
            .filter_map(|id| spaces.iter().find(|space| &space.id == id))
            .cloned()
            .collect())
    }

    /// Read while the spaces exist, since each one's own config may describe
    /// its agents.
    pub(super) async fn agent_settings_of(
        &self,
        daemon: &dyn Daemon,
        spaces: impl Iterator<Item = &Space>,
    ) -> Vec<Result<crate::config::Effective>> {
        let mut settings = Vec::new();
        for space in spaces {
            settings.push(daemon.space_settings(space).await);
        }
        settings
    }

    /// A workspace is recorded at a new checkpoint with its pending review,
    /// for a move that ends it only once another machine imported it.
    pub(super) async fn moving_record(
        &self,
        daemon: &dyn Daemon,
        space: &Space,
    ) -> Result<RemovedSpace> {
        let checkpoint = self.move_checkpoint(daemon, space).await?;
        let mut record =
            (self.present_removal_record(daemon, &space.id, checkpoint, EventCause::Manual))
                .await?;
        carry_review(daemon, &mut record).await?;
        Ok(record)
    }

    /// Leaves out the session files.
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

/// Refuses a workspace whose removal keeps no record.
pub(super) fn ensure_packable(space: &Space, carried: &[Space]) -> Result<()> {
    std::iter::once(space)
        .chain(carried)
        .try_for_each(super::worktree::ensure_packable)
}

/// Each after the records it waits for. Empty unless `carrying` carries the
/// spaces below: a selection leaves them here, below the space's record.
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

/// Keeps a workspace's queued or running review in the record of a space
/// that goes on elsewhere. Its import queues the review again.
pub(super) async fn carry_review(daemon: &dyn Daemon, record: &mut RemovedSpace) -> Result<()> {
    let review = daemon.pending_review(&record.space_id).await?;
    if let Ok(kept) = record.kept_workspace_mut() {
        kept.pending_review = review;
    }
    Ok(())
}
