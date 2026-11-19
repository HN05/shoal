//! A stand-in for the rest of the daemon, which the tests of reviews build
//! over: a temporary store with one ready workspace in it.
use std::path::PathBuf;

use anyhow::Result;
use rusqlite::Connection;
use shoal_ids::repository_ids::RepositoryId;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceName;
use shoal_store::Store;

use crate::daemon::Records;

/// A migrated store at `path` with the workspace `id` of the repository
/// `repo` recorded, which reviews belong to.
pub(crate) async fn store(path: PathBuf, id: &'static str) -> Store {
    let store = shoal_store::test_support::unsynced_store(path)
        .await
        .unwrap();
    store
        .write(move |tx| {
            tx.execute(
                "INSERT INTO repositories(id,path,source,last_used) VALUES ('repo','/repo','/repo',1)",
                [],
            )?;
            tx.execute(
                "INSERT INTO spaces(id,kind,repository_id,name) VALUES (?1,'workspace','repo',?1)",
                [id],
            )?;
            tx.execute(
                "INSERT INTO workspaces(id,path,branch,state) VALUES (?1,'/work',?1,'ready')",
                [id],
            )?;
            Ok(())
        })
        .await
        .unwrap();
    store
}

/// The records of one workspace of the repository `repo`.
pub(crate) struct StandInRecords {
    pub(crate) workspace: &'static str,
    pub(crate) name: &'static str,
}

impl Records for StandInRecords {
    fn space_name(&self, _db: &Connection, id: &SpaceId) -> Result<Option<SpaceName>> {
        Ok((id.as_str() == self.workspace).then(|| self.name.into()))
    }

    fn workspaces(
        &self,
        _db: &Connection,
        repository: Option<&RepositoryId>,
    ) -> Result<Vec<SpaceId>> {
        let listed = repository.is_none_or(|repository| repository.as_str() == "repo");
        Ok(listed.then(|| self.workspace.into()).into_iter().collect())
    }
}
