//! Following a space's agent session: the bytes of its files where its tool
//! keeps them, everything so far and then what the tool appends, copied
//! unread. A client that follows again, as after a daemon restart, says
//! where it read each file to, and the follow goes on from there.
use crate::Spaces;
use crate::daemon::Daemon;
use std::{
    collections::{BTreeMap, HashMap},
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result};
use shoal_ids::{agent_session_ids::AgentSessionId, space_ids::SpaceId};
use shoal_protocol::{
    protocol::{self, Body, ErrorCode, MAX_FRAME, Response},
    scope_tokens::ScopeToken,
    space_names::SpaceSelector,
    transcripts::{self, Environments, FollowedSession, ReadTo, SessionBytes, Store},
};
use tokio::net::{
    UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};

use crate::{
    frames::{self, OperationGuard},
    refusal::{Reason, Refusal},
};

const POLL: Duration = Duration::from_millis(500);

/// What fits in a frame in base64, with room for the rest of the frame and
/// the file's path.
pub const CHUNK: usize = (MAX_FRAME - (8 << 10)) / 4 * 3;

pub struct Following {
    space: SpaceId,
    /// A named session stays the one followed. Without one, the follow goes
    /// on in each new session the space's agents report.
    named: bool,
    session: FollowedSession,
    store: &'static dyn Store,
    environments: Arc<Environments>,
    read: HashMap<PathBuf, ReadTo>,
}

impl Spaces {
    /// Without `session`, the space's current one. `from` says where the
    /// client read each file to. Refuses a session the space's agents never
    /// reported, so a follow reads no other session's files.
    pub async fn session_to_follow(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        session: Option<AgentSessionId>,
        from: BTreeMap<PathBuf, ReadTo>,
    ) -> Result<Following> {
        let space = self.space(selector).await?.id;
        let sessions = self
            .space_sessions(daemon, &SpaceSelector::id(&space))
            .await?;
        let named = session.is_some();
        let reported = match &session {
            Some(id) => sessions.sessions.iter().find(|r| &r.session == id),
            None => sessions.sessions.first(),
        };
        let Some(reported) = reported.cloned() else {
            let message = match session {
                Some(id) => format!("no agent of space {selector} reported a session {id}"),
                None => format!("no agent of space {selector} reported a session yet"),
            };
            return Err(Refusal::new(Reason::NotFound, message).into());
        };
        let integration = (sessions.integrations.get(&reported.agent).copied())
            .unwrap_or(shoal_protocol::integrations::IntegrationKind::None);
        let Some(store) = transcripts::store(integration) else {
            let message = format!(
                "Shoal does not know where agent {} keeps its sessions' files",
                reported.agent
            );
            return Err(Refusal::new(Reason::NotFound, message).into());
        };
        Ok(Following {
            space: space.clone(),
            named,
            session: FollowedSession {
                session: reported,
                integration,
                space,
            },
            store,
            environments: Arc::new(Environments::new(&self.paths.home, &sessions.environments)),
            read: from.into_iter().collect(),
        })
    }

    /// Sends the session, then its files' bytes as they grow. Stops when the
    /// client hangs up, the scope token `scope` stops acting for a caller,
    /// the files cannot be read, or a daemon restart closes the follow. It
    /// reads nothing more once the token stops acting, so a client that
    /// paused reading gets no byte read after that.
    pub async fn follow_session(
        &self,
        daemon: &dyn Daemon,
        stream: UnixStream,
        id: u64,
        (following, scope): (Following, Option<ScopeToken>),
        startup: OperationGuard,
    ) -> Result<()> {
        let (mut reader, writer) = stream.into_split();
        let mut client = Client {
            daemon,
            writer,
            id,
            scope,
        };
        drop(startup);
        let followed = self
            .keep_following(daemon, &mut client, &mut reader, following)
            .await;
        let Err(error) = followed else {
            return Ok(());
        };
        match error.downcast::<Stopped>() {
            // The client follows again on the replacement.
            Ok(Stopped::Restarting) => Ok(()),
            Ok(Stopped::Revoked) => client.refuse_scope().await,
            Ok(Stopped::Unreadable(error)) => client.fail(&error).await,
            Err(error) => Err(error),
        }
    }

    /// Runs until the client hangs up, which `reader` tells.
    async fn keep_following(
        &self,
        daemon: &dyn Daemon,
        client: &mut Client<'_>,
        reader: &mut OwnedReadHalf,
        mut following: Following,
    ) -> Result<()> {
        client
            .send(Body::FollowedSession(following.session.clone()))
            .await?;
        loop {
            client.ensure_scoped().await?;
            if let Some(next) = self.next_following(daemon, &following).await? {
                following = next;
                client
                    .send(Body::FollowedSession(following.session.clone()))
                    .await?;
            }
            send_gained(client, &mut following).await?;
            tokio::select! {
                () = tokio::time::sleep(POLL) => {}
                () = frames::hangup(reader) => return Ok(()),
            }
        }
    }

    /// A follow that named no session goes on in it once it is another than
    /// the one it follows.
    async fn next_following(
        &self,
        daemon: &dyn Daemon,
        following: &Following,
    ) -> Result<Option<Following>> {
        if following.named {
            return Ok(None);
        }
        let current = self.reported_sessions(&following.space).await?;
        let followed = &following.session.session.session;
        if current
            .first()
            .is_none_or(|current| &current.session == followed)
        {
            return Ok(None);
        }
        let space = SpaceSelector::id(&following.space);
        Ok(Some(
            self.session_to_follow(daemon, &space, None, BTreeMap::new())
                .await?,
        ))
    }
}

#[derive(Debug)]
pub enum Stopped {
    /// A daemon restart holds operations off.
    Restarting,
    Revoked,
    /// The session's files could not be looked for or read.
    Unreadable(anyhow::Error),
}

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Restarting => f.write_str("the daemon is restarting"),
            Self::Revoked => f.write_str("the follow's scope token was revoked"),
            Self::Unreadable(error) => write!(f, "{error:#}"),
        }
    }
}

impl std::error::Error for Stopped {}

/// The client's connection, for the scope token the follow came with.
pub struct Client<'a> {
    pub daemon: &'a dyn Daemon,
    pub writer: OwnedWriteHalf,
    pub id: u64,
    pub scope: Option<ScopeToken>,
}

impl Client<'_> {
    /// Sends under a daemon operation of its own, so a follow holds off a
    /// restart only while it writes. [`Stopped::Restarting`] once a restart
    /// holds operations off.
    pub async fn send(&mut self, body: Body) -> Result<()> {
        let Some(_operation) = self.daemon.spaces().gates.try_operation() else {
            return Err(Stopped::Restarting.into());
        };
        protocol::write(&mut self.writer, &Response::new(self.id, body)).await
    }

    /// A follow reads more only while its scope token acts for a caller.
    /// [`Stopped::Revoked`] once it does not.
    async fn ensure_scoped(&self) -> Result<()> {
        match self
            .daemon
            .coordination()
            .still_scoped(self.daemon, self.scope.as_ref())
            .await?
        {
            true => Ok(()),
            false => Err(Stopped::Revoked.into()),
        }
    }

    async fn refuse_scope(&mut self) -> Result<()> {
        let body = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
        self.send(body).await
    }

    async fn fail(&mut self, error: &anyhow::Error) -> Result<()> {
        let message = format!("could not follow the session's files: {error:#}");
        self.send(Body::error(ErrorCode::OperationFailed, message))
            .await
    }
}

/// Keeps where each file now ends.
async fn send_gained(client: &mut Client<'_>, following: &mut Following) -> Result<()> {
    let found = locate(following).await;
    let Some(found) = found.map_err(Stopped::Unreadable)? else {
        return Ok(());
    };
    for file in found.files.files {
        let path = found.directory.join(&file);
        send_file_gained(client, &path, file, &mut following.read).await?;
    }
    Ok(())
}

/// None while the tool has written none.
async fn locate(following: &Following) -> Result<Option<transcripts::Found>> {
    let (store, environments) = (following.store, following.environments.clone());
    let session = following.session.session.session.clone();
    tokio::task::spawn_blocking(move || {
        transcripts::find(store, &environments.each(), session.as_str())
    })
    .await
    .context("look for the session's files")?
}

/// Keeps where the file now ends, and checks the follow's scope token
/// before each read. A file that is gone, as one its tool replaced, may come
/// back, so the follow waits. One that cannot be read otherwise ends the
/// follow. A file written anew, shorter, comes again from offset 0 as a
/// reset. One the follow reads for the first time comes even when it is
/// empty, so the client learns it was emptied.
pub async fn send_file_gained(
    client: &mut Client<'_>,
    path: &Path,
    file: PathBuf,
    read: &mut HashMap<PathBuf, ReadTo>,
) -> Result<()> {
    loop {
        client.ensure_scoped().await?;
        let first = !read.contains_key(&file);
        let seen = read.get(&file).copied().unwrap_or_default();
        let path = path.to_owned();
        let reading = tokio::task::spawn_blocking(move || read_from(&path, seen));
        let gained = match reading.await.context("read a session file")? {
            Ok(gained) => gained,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                let error = anyhow::Error::from(error).context(format!("read {}", file.display()));
                return Err(Stopped::Unreadable(error).into());
            }
        };
        let end = gained.offset + gained.bytes.len() as u64;
        read.insert(
            file.clone(),
            ReadTo {
                offset: end,
                size: gained.size,
            },
        );
        if gained.bytes.is_empty() && !gained.rewritten && !first {
            return Ok(());
        }
        let bytes = SessionBytes {
            reset: gained.rewritten,
            ..SessionBytes::new(file.clone(), gained.offset, gained.size, &gained.bytes)
        };
        client.send(Body::SessionBytes(bytes)).await?;
        if gained.bytes.is_empty() {
            return Ok(());
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Gained {
    pub offset: u64,
    /// The file's length when they were read.
    pub size: u64,
    pub bytes: Vec<u8>,
    /// The file was written anew, shorter than `seen` says it was read, so
    /// it comes again from 0.
    pub rewritten: bool,
}

/// Up to [`CHUNK`] bytes from where `seen` says it was read to, or from 0
/// where the file is now shorter, as one its tool wrote again.
pub fn read_from(path: &Path, seen: ReadTo) -> std::io::Result<Gained> {
    let mut file = std::fs::File::open(path)?;
    let size = file.metadata()?.len();
    let rewritten = size < seen.offset.max(seen.size);
    let offset = match rewritten {
        true => 0,
        false => seen.offset,
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(CHUNK);
    file.take(CHUNK as u64).read_to_end(&mut bytes)?;
    Ok(Gained {
        offset,
        size,
        bytes,
        rewritten,
    })
}

#[cfg(test)]
mod tests;
