//! Where agent tools keep their sessions' files on this machine, such as a
//! session's transcript. Shoal finds them by name and never reads them: an
//! app that shows a conversation reads its tool's own format. The daemon
//! says which sessions a space's agents reported, and
//! [`space_transcripts`] finds their files. [`follow_session`] follows a
//! session's files on whichever machine its space runs.
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::transcripts::{FollowedSession, SpaceSessions, Transcript, locate};

use std::{collections::BTreeMap, path::PathBuf};

use anyhow::{Context as _, Result};
use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{self, Daemon, open_buffered, request};
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
/// dropped. A daemon restart closes the connection; the follow follows
/// again on the replacement, from where it read each file to.
pub struct SessionFollow<D> {
    daemon: D,
    space: SpaceSelector,
    /// The session it was asked for, none for the space's current one.
    named: Option<AgentSessionId>,
    responses: Option<Responses<BufReader<UnixStream>>>,
    /// The session it follows now.
    session: FollowedSession,
    /// Where it read each of the session's files to.
    offsets: BTreeMap<PathBuf, u64>,
    /// The session followed, until [`Self::next`] returns it.
    first: Option<FollowedSession>,
}

/// What a follow of a session brings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followed {
    /// The session whose files' bytes come next: the one followed, and
    /// once the space goes on in another, that one.
    Session(FollowedSession),
    /// Bytes of the session's file `file`, relative to its tool's session
    /// directory, as its tool wrote them from `offset` on; offset 0 again
    /// for a file its tool wrote anew.
    Bytes {
        file: PathBuf,
        offset: u64,
        bytes: Vec<u8>,
    },
}

/// Follow the session `session` of an agent of the workspace or chat
/// `space`, or else its current one, wherever the space runs:
/// [`SessionFollow::next`] brings the session, then its files' bytes,
/// everything so far and then what its tool appends. Fails at once when
/// the daemon refuses, as for a space whose machine is offline.
pub async fn follow_session<D: Daemon>(
    daemon: D,
    space: SpaceSelector,
    session: Option<AgentSessionId>,
) -> Result<SessionFollow<D>> {
    let (responses, followed) = open(&daemon, &space, &session, &BTreeMap::new()).await?;
    Ok(SessionFollow {
        daemon,
        // Its number may name another space by the time it follows again.
        space: SpaceSelector::id(&followed.space),
        named: session,
        responses: Some(responses),
        session: followed.clone(),
        offsets: BTreeMap::new(),
        first: Some(followed),
    })
}

/// Follow `space`'s session `session` on a new connection, from `from`,
/// and read the session it follows.
async fn open(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    session: &Option<AgentSessionId>,
    from: &BTreeMap<PathBuf, u64>,
) -> Result<(Responses<BufReader<UnixStream>>, FollowedSession)> {
    let method = WorkspaceMethod::FollowSession {
        space: space.clone(),
        session: session.clone(),
        from: from.clone(),
    };
    let (stream, body) = open_buffered(daemon, method).await?;
    match body {
        Body::FollowedSession(followed) => Ok((Responses::new(stream), followed)),
        body => Err(body.unexpected("FollowedSession")),
    }
}

impl<D: Daemon> SessionFollow<D> {
    /// What comes next, waiting for the session's tool to write more, and
    /// following again once a daemon restart closed the connection or the
    /// space's machine went out of reach.
    pub async fn next(&mut self) -> Result<Followed> {
        if let Some(session) = self.first.take() {
            return Ok(Followed::Session(session));
        }
        loop {
            let Some(responses) = &mut self.responses else {
                if let Some(session) = self.follow_again().await? {
                    return Ok(Followed::Session(session));
                }
                continue;
            };
            let response = match responses.next().await {
                Ok(response) => response,
                Err(error) if connection::stream_closed(&error) => {
                    self.responses = None;
                    continue;
                }
                Err(error) => return Err(error),
            };
            match response.body.into_result() {
                // As while the space's machine or the hub restarts.
                Err(error) if connection::machine_unreachable(&error) => self.responses = None,
                body => return self.take(body?),
            }
        }
    }

    /// Follow again on a new connection, giving a restarting daemon time to
    /// come back, from where each file was read to; the session it then
    /// follows when it is another.
    async fn follow_again(&mut self) -> Result<Option<FollowedSession>> {
        let (daemon, space, named) = (&self.daemon, &self.space, &self.named);
        let offsets = &self.offsets;
        let (responses, followed) =
            connection::after_restart(|| open(daemon, space, named, offsets)).await?;
        self.responses = Some(responses);
        Ok(self.went_on_in(followed))
    }

    /// What `body`, a frame of the follow, brings, keeping where the bytes
    /// it carries end.
    fn take(&mut self, body: Body) -> Result<Followed> {
        match body {
            Body::FollowedSession(followed) => {
                self.offsets.clear();
                self.session = followed.clone();
                Ok(Followed::Session(followed))
            }
            Body::SessionBytes(gained) => {
                let bytes = gained.bytes()?;
                let end = gained.offset + bytes.len() as u64;
                self.offsets.insert(gained.file.clone(), end);
                Ok(Followed::Bytes {
                    file: gained.file,
                    offset: gained.offset,
                    bytes,
                })
            }
            body => Err(body.unexpected("SessionBytes")),
        }
    }

    /// `followed`, the session a follow opened again follows, when it is
    /// another than the one it followed, which its offsets no longer hold
    /// for.
    fn went_on_in(&mut self, followed: FollowedSession) -> Option<FollowedSession> {
        if followed.session.session == self.session.session.session {
            return None;
        }
        self.offsets.clear();
        self.session = followed.clone();
        Some(followed)
    }
}
