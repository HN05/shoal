//! Commits a start on another joined machine carries there: a base whose
//! commits only this machine has, which the other machine cannot fetch from
//! the origin, goes as a Git bundle of the history the origin's branches
//! lack, over a duplex stream of the hub's links. That machine's daemon
//! fetches it into the repository the start goes in, registering it as a
//! URL clone where it has none, under a ref of Shoal's own, and the start
//! then names the commit by its full ID.
use std::{
    io::{Read as _, Write as _},
    path::Path,
    sync::Arc,
};

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

mod retention;
pub(crate) use retention::CarriedHolds;

/// The name of a carried bundle in the temporary directory that holds it.
const BUNDLE: &str = "carried.bundle";

/// The most bytes of a bundle read or written at once.
const CHUNK: usize = 64 << 10;

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
        (self.send_bundle(machine, receive, &file, caller).await).with_context(|| {
            format!(
                "machine {} could not take the commits of the base",
                machine.name
            )
        })
    }

    /// Send the bundle `file` to `machine` for `caller` as `receive` asks:
    /// once that machine's daemon answers it, write the bundle and end this
    /// side, then wait for its answer that it took the commits.
    async fn send_bundle(
        &self,
        machine: &Member,
        receive: RepositoryMethod,
        file: &Path,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let operation = Operation::Repository(receive);
        let (incoming, mut outgoing) = self.open_duplex(machine, operation, caller).await?;
        let mut answers = Answers::new(incoming);
        answers.next().await?;
        send_file(file, &mut outgoing).await?;
        outgoing.end(None).await;
        answers.next().await
    }

    /// Take the bundle of `commit` that the caller writes on `stream` once
    /// this daemon answered `request_id`, into `repository`, registered
    /// from `origin`, after the origin's `branch`, held for the launch it is
    /// carried for, and answer again once the repository has the commit,
    /// holding `guard` until then.
    pub(crate) async fn receive_commits(
        self: Arc<Self>,
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
                self.release_carried_hold(held).await;
                answer::failure(&error)
            }
        };
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }
}

/// Write the file `file` on `outgoing`, a chunk at a time.
async fn send_file(file: &Path, outgoing: &mut Outgoing) -> Result<()> {
    let path = file.to_owned();
    let mut input = blocking(move || std::fs::File::open(path)).await?;
    loop {
        let (file, chunk) = blocking(move || {
            let mut chunk = vec![0; CHUNK];
            let read = input.read(&mut chunk)?;
            chunk.truncate(read);
            Ok((input, chunk))
        })
        .await?;
        if chunk.is_empty() {
            return Ok(());
        }
        input = file;
        outgoing.write(&chunk).await?;
    }
}

/// Write what `stream` reads until the caller ends its side to the new
/// file `file`, a chunk at a time.
async fn receive_file(stream: &mut UnixStream, file: &Path) -> Result<()> {
    let path = file.to_owned();
    let mut output = blocking(move || std::fs::File::create(path)).await?;
    let mut chunk = vec![0; CHUNK];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        let written = chunk[..read].to_vec();
        output = blocking(move || output.write_all(&written).map(|()| output)).await?;
    }
}

/// What `work`, which blocks on the file system, returns, run on the
/// blocking pool.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T> {
    Ok(tokio::task::spawn_blocking(work).await??)
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
