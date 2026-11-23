//! Commits a start on another joined machine carries there: a base whose
//! commits only this machine has, which the other machine cannot fetch from
//! the origin, goes as a Git bundle of the history the origin's branches
//! lack, over a duplex stream of the hub's links. That machine's daemon
//! fetches it into the repository the start goes in, registering it as a
//! URL clone where it has none, under a ref of Shoal's own, and the start
//! then names the commit by its full ID.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::fsutil::off_runtime;
use std::{io::Write as _, path::Path};

use anyhow::{Context as _, Result};
use shoal_protocol::{
    hub::Member,
    protocol::{self, Body, Operation, RepositoryMethod, Response},
};
use shoal_refs::CommitId;
use tokio::{io::AsyncReadExt as _, net::UnixStream};

use super::{frames::OperationGuard, scope::Caller};
use crate::model::Repository;

pub mod retention;
pub use retention::CarriedHolds;

/// The name of a carried bundle in the temporary directory that holds it.
const BUNDLE: &str = "carried.bundle";

/// The most bytes of a bundle written at once.
const CHUNK: usize = 64 << 10;

impl Spaces {
    /// Carry `commit` of `repository`, whose origin's URL is `origin`, to
    /// `machine` for `caller`'s start there, which then names it by its ID,
    /// with the history the origin's `branch` lacks.
    pub async fn carry_commit(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        (repository, origin): (&Repository, &str),
        (commit, branch): (&CommitId, &str),
        caller: Option<&Caller>,
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let file = directory.path().join(BUNDLE);
        let bundled = (commit, branch);
        crate::git::clone_base::bundle_carried(&repository.path, &file, bundled).await?;
        let receive = RepositoryMethod::ReceiveCommits {
            origin: origin.to_owned(),
            commit: commit.clone(),
            branch: branch.to_owned(),
        };
        (self
            .send_bundle(daemon, machine, receive, &file, caller)
            .await)
            .with_context(|| {
                format!(
                    "machine {} could not take the commits of the base",
                    machine.name
                )
            })
    }

    /// Send the bundle `file` to `machine` for `caller` as `receive` asks,
    /// and wait for its answer that it took the commits.
    async fn send_bundle(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        receive: RepositoryMethod,
        file: &Path,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let operation = Operation::Repository(receive);
        daemon
            .deliver_file(machine, operation, caller, file)
            .await
            .map(drop)
    }

    /// Take the bundle of `commit` that the caller writes on `stream` once
    /// this daemon answered `request_id`, into `repository`, registered
    /// from `origin`, after the origin's `branch`, held for the launch it is
    /// carried for, and answer again once the repository has the commit,
    /// holding `guard` until then.
    pub async fn receive_commits(
        &self,
        daemon: &dyn Daemon,
        mut stream: UnixStream,
        request_id: u64,
        (repository, origin, commit, branch): (Repository, String, CommitId, String),
        guard: OperationGuard,
    ) -> Result<()> {
        protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        let held = (origin.as_str(), &commit);
        self.carried_holds.take(&repository.id, held).await;
        let carried = (&commit, branch.as_str());
        let body = match take_bundle(&mut stream, &repository.path, carried).await {
            Ok(()) => Body::Ok,
            Err(error) => {
                // The start that waits for it fails, so no launch comes.
                self.release_carried_hold(daemon, held).await;
                daemon.failure(&error)
            }
        };
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }
}

/// Write what `stream` reads until the caller ends its side to the new
/// file `file`, a chunk at a time.
pub async fn receive_file(stream: &mut UnixStream, file: &Path) -> Result<()> {
    let path = file.to_owned();
    let mut output = off_runtime::run_io(move || std::fs::File::create(path)).await?;
    let mut chunk = vec![0; CHUNK];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        let written = chunk[..read].to_vec();
        output = off_runtime::run_io(move || output.write_all(&written).map(|()| output)).await?;
    }
}

/// Fetch `commit` into `repo`, after the origin's `branch`, from the bundle
/// read from `stream` until the caller ends its side.
async fn take_bundle(
    stream: &mut UnixStream,
    repo: &Path,
    carried: (&CommitId, &str),
) -> Result<()> {
    let directory = tempfile::tempdir()?;
    let file = directory.path().join(BUNDLE);
    receive_file(stream, &file).await?;
    crate::git::clone_base::fetch_carried(repo, &file, carried).await
}
