//! Where agent tools keep their sessions' files on this machine, such as a
//! session's transcript. Shoal finds them by name and never reads them: an
//! app that shows a conversation reads its tool's own format. The daemon
//! says which sessions a space's agents reported, and
//! [`space_transcripts`] finds their files. [`follow_session`] follows a
//! session's files on whichever machine its space runs.
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::transcripts::{FollowedSession, SpaceSessions, Transcript, locate};

use anyhow::{Context as _, Result};
use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{Daemon, open_buffered, request};
use shoal_protocol::protocol::{Body, Responses, WorkspaceMethod};
use shoal_protocol::space_names::SpaceSelector;

/// The sessions the agents of the workspace or chat `space` reported.
pub async fn space_sessions(daemon: &impl Daemon, space: SpaceSelector) -> Result<SpaceSessions> {
    request(daemon, WorkspaceMethod::SpaceSessions { space }).await
}

/// The sessions the agents of the workspace or chat `space` reported, each
/// with its files on this machine.
pub async fn space_transcripts(
    daemon: &impl Daemon,
    space: SpaceSelector,
) -> Result<Vec<Transcript>> {
    let sessions = space_sessions(daemon, space).await?;
    let home = daemon.paths().home.clone();
    tokio::task::spawn_blocking(move || locate(&home, &sessions))
        .await
        .context("look for the sessions' files")
}

/// A follow of an agent's session, on a connection of its own until it is
/// dropped.
pub struct SessionFollow {
    responses: Responses<BufReader<UnixStream>>,
    /// The session followed, until [`Self::next`] returns it.
    first: Option<FollowedSession>,
}

/// What a follow of a session brings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followed {
    /// The session whose files' bytes come next: the one followed, and
    /// once its agent goes on in another, that one.
    Session(FollowedSession),
    /// Bytes of the session's file `file`, relative to its tool's session
    /// directory, as its tool wrote them from `offset` on; offset 0 again
    /// for a file its tool wrote anew.
    Bytes {
        file: std::path::PathBuf,
        offset: u64,
        bytes: Vec<u8>,
    },
}

/// Follow the session `session` of an agent of the workspace or chat
/// `space`, or else its agent's current one, wherever the space runs:
/// [`SessionFollow::next`] brings the session, then its files' bytes,
/// everything so far and then what its tool appends. Fails at once when
/// the daemon refuses, as for a space whose machine is offline.
pub async fn follow_session(
    daemon: &impl Daemon,
    space: SpaceSelector,
    session: Option<AgentSessionId>,
) -> Result<SessionFollow> {
    let method = WorkspaceMethod::FollowSession { space, session };
    let (stream, body) = open_buffered(daemon, method).await?;
    let session = match body {
        Body::FollowedSession(session) => session,
        body => return Err(body.unexpected("FollowedSession")),
    };
    Ok(SessionFollow {
        responses: Responses::new(stream),
        first: Some(session),
    })
}

impl SessionFollow {
    /// What comes next, waiting for the session's tool to write more; an
    /// error once the daemon ends the follow, as a restart does.
    pub async fn next(&mut self) -> Result<Followed> {
        if let Some(session) = self.first.take() {
            return Ok(Followed::Session(session));
        }
        let response = self.responses.next().await?;
        match response.body.into_result()? {
            Body::FollowedSession(session) => Ok(Followed::Session(session)),
            Body::SessionBytes(gained) => Ok(Followed::Bytes {
                bytes: gained.bytes()?,
                file: gained.file,
                offset: gained.offset,
            }),
            body => Err(body.unexpected("SessionBytes")),
        }
    }
}
