//! Listing and syncing repositories.
use shoal_client::{ApiError, SyncRepository, SyncedRepository, records::Repository};

use super::super::{Session, records};
use crate::{model, protocol::RepositoryMethod};

pub(super) async fn list(session: &Session) -> Result<Vec<Repository>, ApiError> {
    let repositories: Vec<model::Repository> =
        session.ask(RepositoryMethod::ListRepositories).await?;
    Ok(repositories.into_iter().map(records::repository).collect())
}

pub(super) async fn sync(
    session: &Session,
    params: SyncRepository,
) -> Result<SyncedRepository, ApiError> {
    let synced: model::SyncedRepository = session
        .ask(RepositoryMethod::SyncRepository {
            repository: params.repository.as_str().into(),
        })
        .await?;
    Ok(SyncedRepository {
        repository_id: synced.repository_id,
        remote: synced.remote,
        default_branch: synced.default_branch.branch,
    })
}
