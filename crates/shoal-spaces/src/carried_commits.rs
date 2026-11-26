//! Carrying commits only this machine has to a start on another joined
//! machine, as a Git bundle of the history the origin's branches lack. See
//! `docs/design/hub.md`.
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

const BUNDLE: &str = "carried.bundle";

const CHUNK: usize = 64 << 10;

impl Spaces {
    /// The bundle holds the history the origin's `branch` lacks. The start
    /// there then names the commit by its ID.
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

    /// Waits for `machine`'s answer that it took the commits.
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

    /// Answers `request_id` first, and again once `repository`, registered
    /// from `origin`, has the commit. The commit is held for the launch it
    /// is carried for. Holds `guard` until then.
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

/// Reads until the caller ends its side. `file` must be a new file.
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

/// Reads the bundle until the caller ends its side.
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
