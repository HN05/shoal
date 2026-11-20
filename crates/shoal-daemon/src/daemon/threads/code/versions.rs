//! Keeping the file versions code references point at: each space's ref
//! in its repository holds exactly the versions its references keep, and
//! goes with the last of them.
use anyhow::Result;
use shoal_ids::{repository_ids::RepositoryId, space_ids::SpaceId};

use super::records;
use crate::{daemon::manager::Manager, git, model::Repository};

impl Manager {
    /// Point the ref of each of `spaces`, in the repository `repository`,
    /// at the versions its references keep now. A failure leaves the
    /// versions to the next expiry pass, which writes the refs again.
    pub(in crate::daemon::threads) async fn keep_code_versions(
        &self,
        repository: &RepositoryId,
        spaces: impl IntoIterator<Item = SpaceId>,
    ) {
        let kept = async {
            let repo = self.repository(repository).await?;
            let _refs = self.gates.lock_checkpoints().await;
            for space in spaces {
                self.keep_versions_of(&repo, &space).await?;
            }
            anyhow::Ok(())
        };
        if let Err(error) = kept.await {
            crate::daemon::log!("versions of referenced code not kept: {error:#}");
        }
    }

    /// Point the ref of the space `space` in `repo` at the versions its
    /// references keep, deleting it once they keep none; whether it
    /// changed. The caller holds the checkpoints gate, which orders refs
    /// against expiry.
    async fn keep_versions_of(&self, repo: &Repository, space: &SpaceId) -> Result<bool> {
        let reference = git::code_versions::reference(space.as_str());
        let id = space.clone();
        let blobs = self.store.read(move |db| records::kept(db, &id)).await?;
        if git::code_versions::kept(&repo.path, &reference).await? == blobs {
            return Ok(false);
        }
        git::code_versions::keep(&repo.path, &reference, &blobs).await?;
        Ok(true)
    }
}
