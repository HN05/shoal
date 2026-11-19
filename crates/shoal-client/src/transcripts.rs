//! Where agent tools keep their sessions' files on this machine, such as a
//! session's transcript. Shoal finds them by name and never reads them: an
//! app that shows a conversation reads its tool's own format. The daemon
//! says which sessions a space's agents reported, and
//! [`space_transcripts`] finds their files. [`follow_session`] follows a
//! session's files on whichever machine its space runs.
use shoal_ids::agent_session_ids::AgentSessionId;
use shoal_protocol::transcripts::{FollowedSession, ReadTo, SpaceSessions, Transcript, locate};

use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};

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
    cursor: Cursor,
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
        cursor: Cursor::default(),
        first: Some(followed),
    })
}

/// Follow `space`'s session `session` on a new connection, from `from`,
/// and read the session it follows.
async fn open(
    daemon: &impl Daemon,
    space: &SpaceSelector,
    session: &Option<AgentSessionId>,
    from: &BTreeMap<PathBuf, ReadTo>,
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
                body => {
                    if let Some(followed) = self.take(body?)? {
                        return Ok(followed);
                    }
                }
            }
        }
    }

    /// Follow again on a new connection, giving a restarting daemon time to
    /// come back, from where each file was read to; the session it then
    /// follows when it is another.
    async fn follow_again(&mut self) -> Result<Option<FollowedSession>> {
        let from = self.cursor.resume();
        let (daemon, space, named) = (&self.daemon, &self.space, &self.named);
        let (responses, followed) =
            connection::after_restart(|| open(daemon, space, named, &from)).await?;
        self.responses = Some(responses);
        Ok(self.went_on_in(followed))
    }

    /// What `body`, a frame of the follow, brings that it has not read
    /// yet, keeping where the bytes it carries end.
    fn take(&mut self, body: Body) -> Result<Option<Followed>> {
        match body {
            Body::FollowedSession(followed) => {
                self.cursor = Cursor::default();
                self.session = followed.clone();
                Ok(Some(Followed::Session(followed)))
            }
            Body::SessionBytes(gained) => {
                let bytes = gained.bytes()?;
                let read = Chunk {
                    offset: gained.offset,
                    size: gained.size,
                    reset: gained.reset,
                };
                let unread = self.cursor.take(&gained.file, read, bytes);
                Ok(unread.map(|(offset, bytes)| Followed::Bytes {
                    file: gained.file,
                    offset,
                    bytes,
                }))
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
        self.cursor = Cursor::default();
        self.session = followed.clone();
        Some(followed)
    }
}

/// Where a chunk of a session file starts, how long the file was then, and
/// whether the daemon sends it again from 0 as written anew.
#[derive(Debug, Clone, Copy)]
struct Chunk {
    offset: u64,
    size: u64,
    reset: bool,
}

/// The most a follow opened again spends of its request on where it read
/// each file to, well within a frame.
const RESUME_BYTES: usize = 32 << 10;

/// Where a follow read each of its session's files to, and what of them
/// the daemon sends again once it follows again.
#[derive(Debug, Default)]
struct Cursor {
    read: BTreeMap<PathBuf, ReadTo>,
    /// The files a follow opened again left out of where it read to, which
    /// the daemon sends from their start, with where it read each to.
    replaying: HashMap<PathBuf, ReadTo>,
}

impl Cursor {
    /// Where a follow opened again reads on from: the files read furthest,
    /// as many as [`RESUME_BYTES`] holds; the daemon sends the others from
    /// their start, which [`Self::take`] skips to where it read them to.
    fn resume(&mut self) -> BTreeMap<PathBuf, ReadTo> {
        let mut read: Vec<_> = self.read.iter().collect();
        read.sort_by_key(|(_, read)| std::cmp::Reverse(read.offset));
        let mut from = BTreeMap::new();
        let mut spent = 0;
        self.replaying.clear();
        for (file, &read) in read {
            // Its path as JSON, its offset and size and what separates them.
            let cost = serde_json::to_string(file).map_or(usize::MAX, |path| path.len() + 64);
            if spent + cost <= RESUME_BYTES {
                spent += cost;
                from.insert(file.clone(), read);
            } else {
                self.replaying.insert(file.clone(), read);
            }
        }
        from
    }

    /// What of `bytes`, a `chunk` of `file`, the follow has not read, from
    /// its offset, keeping where it read `file` to; none once it read them
    /// all before. A file the daemon resets, or that comes again shorter
    /// than it was when it read it, was written anew, so it comes whole.
    fn take(&mut self, file: &Path, chunk: Chunk, bytes: Vec<u8>) -> Option<(u64, Vec<u8>)> {
        let Chunk {
            offset,
            size,
            reset,
        } = chunk;
        let end = offset + bytes.len() as u64;
        let shorter = |seen: &ReadTo| size < seen.offset.max(seen.size);
        if reset || self.replaying.get(file).is_some_and(shorter) {
            self.replaying.remove(file);
        }
        let read = self.replaying.get(file).map(|seen| seen.offset);
        let (offset, bytes) = match read {
            Some(read) if end <= read => {
                if end == read {
                    self.replaying.remove(file);
                }
                return None;
            }
            Some(read) => {
                self.replaying.remove(file);
                let skip = read.saturating_sub(offset) as usize;
                (offset.max(read), bytes[skip..].to_vec())
            }
            None => (offset, bytes),
        };
        let read = ReadTo { offset: end, size };
        self.read.insert(file.to_owned(), read);
        Some((offset, bytes))
    }
}

#[cfg(test)]
mod tests;
