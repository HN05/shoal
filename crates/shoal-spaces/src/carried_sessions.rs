//! Carrying the session a start on another joined machine forks, when only
//! this machine has it. The files go one after another over a duplex stream
//! of the hub's links, and that machine's daemon has `shoal internal
//! place-sessions` put them in place before the start comes.
use crate::Spaces;
use crate::daemon::Daemon;
use crate::fsutil::off_runtime;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context as _, Result, ensure};
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::{
    hub::Member,
    protocol::{self, Body, LaunchMethod, Operation, Response},
    space_names::SpaceSelector,
    transcripts::{self, CarriedSessionFile, Environments, SessionFile},
};
use tokio::{io::AsyncWriteExt as _, net::UnixStream};

use super::{frames::OperationGuard, scope::Caller};
use crate::{
    model::Repository,
    workers::{Worker, detached_command, internal_command},
};

const CARRIED: &str = "session";

impl Spaces {
    /// `origin` is the URL of the repository the start goes in, if any.
    /// Refuses a session whose files this machine does not have.
    pub async fn carry_session(
        &self,
        daemon: &dyn Daemon,
        machine: &Member,
        (forked, session): (&SpaceSelector, &AgentSessionId),
        origin: Option<&str>,
        caller: Option<&Caller>,
    ) -> Result<()> {
        let sources = self.session_sources_of(daemon, forked, session).await?;
        let directory = tempfile::tempdir()?;
        let file = directory.path().join(CARRIED);
        let files = off_runtime::run_io({
            let file = file.clone();
            move || join_files(&sources, &file)
        })
        .await?;
        let receive = LaunchMethod::ReceiveSession {
            origin: origin.map(str::to_owned),
            files,
        };
        let operation = Operation::Launch(receive);
        (daemon
            .deliver_file(machine, operation, caller, &file)
            .await
            .map(drop))
        .with_context(|| {
            format!(
                "machine {} could not take the files of the session the start forks",
                machine.name
            )
        })
    }

    /// Refuses when this machine has none of them.
    async fn session_sources_of(
        &self,
        daemon: &dyn Daemon,
        forked: &SpaceSelector,
        session: &AgentSessionId,
    ) -> Result<Vec<(SessionFile, PathBuf)>> {
        let sessions = self.space_sessions(daemon, forked).await?;
        let reported = (sessions.sessions.iter()).find(|reported| &reported.session == session);
        let reported = reported.with_context(|| {
            format!("no agent of space {forked} reported a session {session} to fork")
        })?;
        let integration = sessions.integrations.get(&reported.agent).copied();
        let store = integration.and_then(transcripts::store).with_context(|| {
            format!(
                "Shoal does not know where agent {} keeps its sessions' files, so it cannot carry one to another machine",
                reported.agent
            )
        })?;
        let environments = Environments::new(&self.paths.home, &sessions.environments);
        let id = session.clone();
        let found = tokio::task::spawn_blocking(move || {
            transcripts::find(store, &environments.each(), id.as_str())
        })
        .await??;
        let found = found.with_context(|| {
            format!(
                "this machine has no files of {} session {session}",
                reported.agent
            )
        })?;
        Ok(sources(found, &reported.agent, session))
    }

    /// Answers `request_id` first, then answers again once the files are in
    /// place under the settings of `repository`, else of the global config.
    /// Holds `guard` until then.
    pub async fn receive_session(
        &self,
        daemon: &dyn Daemon,
        mut stream: UnixStream,
        request_id: u64,
        (repository, files): (Option<Repository>, Vec<SessionFile>),
        guard: OperationGuard,
    ) -> Result<()> {
        protocol::write(&mut stream, &Response::new(request_id, Body::Ok)).await?;
        let placed = self.take_session(&mut stream, repository.as_ref(), files);
        let body = match placed.await {
            Ok(()) => Body::Ok,
            Err(error) => daemon.failure(&error),
        };
        protocol::write(&mut stream, &Response::new(request_id, body)).await?;
        drop(guard);
        Ok(())
    }

    /// Reads until the caller ends its side.
    async fn take_session(
        &self,
        stream: &mut UnixStream,
        repository: Option<&Repository>,
        files: Vec<SessionFile>,
    ) -> Result<()> {
        let directory = tempfile::tempdir()?;
        let joined = directory.path().join(CARRIED);
        super::carried_commits::receive_file(stream, &joined).await?;
        let carried = off_runtime::run_io({
            let directory = directory.path().to_owned();
            move || split_files(&joined, files, &directory)
        })
        .await?;
        self.place_sessions(repository, &carried).await
    }

    /// Runs as the user, in this daemon's environment.
    async fn place_sessions(
        &self,
        repository: Option<&Repository>,
        carried: &[CarriedSessionFile],
    ) -> Result<()> {
        let repository = repository.map(|repository| &repository.id);
        let worker = Worker::PlaceSessions { repository };
        let argv = internal_command(&self.paths, true, worker)?;
        let mut command = detached_command(&argv);
        command
            .current_dir(&self.paths.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .env_remove(crate::env::EXECUTION_ID)
            .env_remove(crate::env::SCOPE_TOKEN);
        let mut child = command
            .spawn()
            .context("start placing the session's files")?;
        let mut stdin = child.stdin.take().context("the placing takes no input")?;
        stdin.write_all(&serde_json::to_vec(carried)?).await?;
        drop(stdin);
        let output = child.wait_with_output().await?;
        ensure!(
            output.status.success(),
            "cannot place the files of the forked session: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        Ok(())
    }
}

/// Each one's size is its length once it is copied.
fn sources(
    found: transcripts::Found,
    agent: &shoal_protocol::agent::Agent,
    session: &AgentSessionId,
) -> Vec<(SessionFile, PathBuf)> {
    let directory = found.directory;
    (found.files.files.into_iter())
        .map(|path| {
            let source = directory.join(&path);
            let file = SessionFile {
                agent: agent.clone(),
                session: session.clone(),
                path,
                size: 0,
            };
            (file, source)
        })
        .collect()
}

/// `joined` must be a new file. Returns each file's place in the session's
/// directory and its length there.
pub fn join_files(
    sources: &[(SessionFile, PathBuf)],
    joined: &Path,
) -> std::io::Result<Vec<SessionFile>> {
    let mut output = std::fs::File::create_new(joined)?;
    let mut files = Vec::new();
    for (file, source) in sources {
        let size = std::io::copy(&mut std::fs::File::open(source)?, &mut output)?;
        files.push(SessionFile {
            size,
            ..file.clone()
        });
    }
    Ok(files)
}

/// Refuses a length that does not match.
pub fn split_files(
    joined: &Path,
    files: Vec<SessionFile>,
    directory: &Path,
) -> std::io::Result<Vec<CarriedSessionFile>> {
    let mut input = std::fs::File::open(joined)?;
    let mut carried = Vec::new();
    for (index, file) in files.into_iter().enumerate() {
        if !file.stays_in_session_directory() {
            return Err(std::io::Error::other(format!(
                "the start names the session file {}, outside the agent's session directory",
                file.path.display()
            )));
        }
        let contents = directory.join(index.to_string());
        let mut part = std::io::Read::take(&mut input, file.size);
        let copied = std::io::copy(&mut part, &mut std::fs::File::create_new(&contents)?)?;
        if copied != file.size {
            return Err(std::io::Error::other(format!(
                "{} arrived with {copied} of its {} bytes",
                file.path.display(),
                file.size
            )));
        }
        carried.push(CarriedSessionFile { file, contents });
    }
    let mut rest = [0; 1];
    match std::io::Read::read(&mut input, &mut rest)? {
        0 => Ok(carried),
        _ => Err(std::io::Error::other(
            "more arrived than the session's files",
        )),
    }
}
