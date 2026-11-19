//! Commits a start on another joined machine carries there: a base whose
//! commits only this machine has, which the other machine cannot fetch from
//! the origin, goes as a Git bundle of the history the origin's branches
//! lack, over a duplex stream of the hub's links. That machine's daemon
//! fetches it into the repository the start goes in, registering it as a
//! URL clone where it has none, under a ref of Shoal's own, and the start
//! then names the commit by its full ID.
use std::{path::Path, sync::Arc};

use anyhow::{Context as _, Result, bail};
use shoal_protocol::{
    hub::Member,
    protocol::{self, Body, Operation, RepositoryMethod, Response},
};
use shoal_refs::CommitId;
use tokio::{io::AsyncReadExt as _, net::UnixStream};

use super::{
    answer::{self, OperationGuard},
    hub::{Incoming, Outgoing},
    manager::Manager,
    scope::Caller,
};
use crate::model::Repository;

/// The name of a carried bundle in the temporary directory that holds it.
const BUNDLE: &str = "carried.bundle";

impl Manager {
    /// Carry `commit` of `repository`, whose origin's URL is `origin`, to
    /// `machine` for `caller`'s start there, which then names it by its ID,
    /// with the history the origin's `branch` lacks.
    pub(crate) async fn carry_commit(
        &self,
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
        let operation = Operation::Repository(receive);
        let (incoming, mut outgoing) = self.open_duplex(machine, operation, caller).await?;
        let mut answers = Answers::new(incoming);
        answers.next().await?;
        send_file(&file, &mut outgoing).await?;
        outgoing.end(None).await;
        answers.next().await.with_context(|| {
            format!(
                "machine {} could not take the commits of the base",
                machine.name
            )
        })
    }

    /// Take the bundle of `commit` that the caller writes on `stream` once
    /// this daemon answered `request_id`, into `repository`, after the
    /// origin's `branch`, and answer again once the repository has the
    /// commit, holding `guard` until then.
    pub(crate) async fn receive_commits(
        self: Arc<Self>,
        mut stream: UnixStream,
        request_id: u64,
        (repository, commit, branch): (Repository, CommitId, String),
        guard: OperationGuard,
    ) -> Result<()> {
        protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        let carried = (&commit, branch.as_str());
        let body = match take_bundle(&mut stream, &repository.path, carried).await {
            Ok(()) => Body::Ok,
            Err(error) => answer::failure(&error),
        };
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }
}

/// Write the file `file` on `outgoing`.
async fn send_file(file: &Path, outgoing: &mut Outgoing) -> Result<()> {
    let file = file.to_owned();
    let bytes = tokio::task::spawn_blocking(move || std::fs::read(file)).await??;
    outgoing.write(&bytes).await
}

/// Fetch `commit` into `repo`, after the origin's `branch`, from the bundle
/// read from `stream` until the caller ends its side.
async fn take_bundle(
    stream: &mut UnixStream,
    repo: &Path,
    carried: (&CommitId, &str),
) -> Result<()> {
    let mut bytes = Vec::new();
    stream.read_to_end(&mut bytes).await?;
    let directory = tempfile::tempdir()?;
    let file = directory.path().join(BUNDLE);
    let written = file.clone();
    tokio::task::spawn_blocking(move || std::fs::write(written, bytes)).await??;
    crate::git::clone_base::fetch_carried(repo, &file, carried).await
}

/// The answers another machine's daemon writes on a stream, frame by frame.
struct Answers {
    incoming: Incoming,
    read: Vec<u8>,
}

impl Answers {
    fn new(incoming: Incoming) -> Self {
        Self {
            incoming,
            read: Vec::new(),
        }
    }

    /// The next answer, past any the daemon sent while it still prepared
    /// it; its refusal as an error.
    async fn next(&mut self) -> Result<()> {
        loop {
            if let Some(end) = self.read.iter().position(|byte| *byte == b'\n') {
                let line: Vec<u8> = self.read.drain(..=end).collect();
                let response: Response = serde_json::from_slice(&line)?;
                if !matches!(response.body, Body::Keepalive) {
                    return response.body.into_result().map(drop);
                }
                continue;
            }
            match self.incoming.next().await? {
                Some(bytes) => self.read.extend(bytes),
                None => bail!("the other machine's daemon ended the stream unanswered"),
            }
        }
    }
}
