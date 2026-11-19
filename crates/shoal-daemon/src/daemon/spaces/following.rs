//! Following a space's agent session: the bytes of its files where its tool
//! keeps them, everything so far and then what the tool appends, copied
//! unread as an export copies them. A follow that names no session follows
//! the space's current one, going on in each new one its agents report. A
//! client that follows again, as after a daemon restart, says where it read
//! each file to, and the follow goes on from there.
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
    transcripts::{self, Environments, FollowedSession, SessionBytes, Store},
};
use tokio::net::{
    UnixStream,
    unix::{OwnedReadHalf, OwnedWriteHalf},
};

use crate::{
    daemon::{
        answer::{self, OperationGuard},
        workspace::Manager,
    },
    refusal::{Reason, Refusal},
};

/// How often a follow looks for what its session's files gained.
const POLL: Duration = Duration::from_millis(500);

/// The most bytes of a file one frame carries: what fits in a frame in
/// base64, with room for the rest of the frame and the file's path.
const CHUNK: usize = (MAX_FRAME - (8 << 10)) / 4 * 3;

/// A session to follow, of a space here.
pub(crate) struct Following {
    space: SpaceId,
    /// Whether the follow named it, so that it stays with it.
    named: bool,
    session: FollowedSession,
    store: &'static dyn Store,
    environments: Arc<Environments>,
    /// Where the client read each of its files to.
    read: HashMap<PathBuf, FileRead>,
}

impl Manager {
    /// The session `session` of an agent of the space `selector` names, or
    /// without one its current session, which a follow follows from where
    /// `from` says the client read each file to; refused for a session its
    /// agents never reported, so a follow reads no other session's files.
    pub(crate) async fn session_to_follow(
        &self,
        selector: &SpaceSelector,
        session: Option<AgentSessionId>,
        from: BTreeMap<PathBuf, u64>,
    ) -> Result<Following> {
        let space = self.space(selector).await?.id;
        let sessions = self.space_sessions(&SpaceSelector::id(&space)).await?;
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
            read: (from.into_iter())
                .map(|(file, offset)| {
                    (
                        file,
                        FileRead {
                            offset,
                            size: offset,
                        },
                    )
                })
                .collect(),
        })
    }

    /// Send `following`'s session, then its files' bytes as they grow,
    /// until the client hangs up, the scope token it came with, `scope`,
    /// stops acting for a caller, its files cannot be read, or a daemon
    /// restart closes the follow. It reads nothing more once the token
    /// stops acting, so a client that paused reading gets no byte read
    /// after that.
    pub(crate) async fn follow_session(
        self: Arc<Self>,
        stream: UnixStream,
        id: u64,
        (following, scope): (Following, Option<ScopeToken>),
        startup: OperationGuard,
    ) -> Result<()> {
        let (mut reader, writer) = stream.into_split();
        let mut client = Client {
            manager: &self,
            writer,
            id,
            scope,
        };
        drop(startup);
        let followed = self
            .keep_following(&mut client, &mut reader, following)
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

    /// Send `following`'s session, then its files' bytes as they grow, to
    /// `client` until it hangs up, which `reader` tells.
    async fn keep_following(
        &self,
        client: &mut Client<'_>,
        reader: &mut OwnedReadHalf,
        mut following: Following,
    ) -> Result<()> {
        client
            .send(Body::FollowedSession(following.session.clone()))
            .await?;
        loop {
            client.ensure_scoped().await?;
            if let Some(next) = self.next_following(&following).await? {
                following = next;
                client
                    .send(Body::FollowedSession(following.session.clone()))
                    .await?;
            }
            send_gained(client, &mut following).await?;
            tokio::select! {
                () = tokio::time::sleep(POLL) => {}
                () = answer::hangup(reader) => return Ok(()),
            }
        }
    }

    /// The space's current session, of whichever of its agents, where a
    /// follow that named none goes on in it, once it is another than the
    /// one it follows.
    async fn next_following(&self, following: &Following) -> Result<Option<Following>> {
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
            self.session_to_follow(&space, None, BTreeMap::new())
                .await?,
        ))
    }
}

/// Why a follow stops before its client hangs up.
#[derive(Debug)]
enum Stopped {
    /// A daemon restart holds operations off.
    Restarting,
    /// The follow's scope token no longer acts for a caller.
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

/// Where a follow writes: its client's connection, for the scope token
/// the follow came with.
struct Client<'a> {
    manager: &'a Manager,
    writer: OwnedWriteHalf,
    id: u64,
    scope: Option<ScopeToken>,
}

impl Client<'_> {
    /// Send `body` under a daemon operation of its own, so a follow holds
    /// off a restart only while it writes; [`Stopped::Restarting`] once a
    /// restart holds operations off.
    async fn send(&mut self, body: Body) -> Result<()> {
        let Some(_operation) = self.manager.gates.try_operation() else {
            return Err(Stopped::Restarting.into());
        };
        protocol::write(&mut self.writer, &Response::new(self.id, body)).await
    }

    /// Go on only while the follow's scope token acts for a caller, so
    /// that the follow reads more for it; [`Stopped::Revoked`] once not.
    async fn ensure_scoped(&self) -> Result<()> {
        match self.manager.still_scoped(self.scope.as_ref()).await? {
            true => Ok(()),
            false => Err(Stopped::Revoked.into()),
        }
    }

    /// End the follow, whose scope token no longer acts for a caller.
    async fn refuse_scope(&mut self) -> Result<()> {
        let body = Body::error(ErrorCode::ScopeDenied, "expired or unknown workspace scope");
        self.send(body).await
    }

    /// End the follow, whose session's files could not be looked for or
    /// read, for `error`.
    async fn fail(&mut self, error: &anyhow::Error) -> Result<()> {
        let message = format!("could not follow the session's files: {error:#}");
        self.send(Body::error(ErrorCode::OperationFailed, message))
            .await
    }
}

/// Send what the files of `following`'s session gained since its offsets,
/// keeping where each now ends.
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

/// Where the files of `following`'s session are now, none while its tool
/// has written none.
async fn locate(following: &Following) -> Result<Option<transcripts::Found>> {
    let (store, environments) = (following.store, following.environments.clone());
    let session = following.session.session.session.clone();
    tokio::task::spawn_blocking(move || {
        transcripts::find(store, &environments.each(), session.as_str())
    })
    .await
    .context("look for the session's files")?
}

/// How far a follow read a file, and how long the file was then.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct FileRead {
    offset: u64,
    size: u64,
}

/// Send what the session's file `file`, at `path`, gained since `read`
/// says the follow read it to, keeping where it now ends, checking the
/// follow's scope token before each read. A file that is gone, as one its
/// tool replaced, may come back, so it waits; one that cannot be read
/// otherwise ends the follow. A file written anew, shorter, comes again
/// from offset 0 as a reset, and one the follow reads for the first time
/// comes even when it is empty, so the client learns it was emptied.
async fn send_file_gained(
    client: &mut Client<'_>,
    path: &Path,
    file: PathBuf,
    read: &mut HashMap<PathBuf, FileRead>,
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
            FileRead {
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

/// What a read of a session file gained.
#[derive(Debug, PartialEq, Eq)]
struct Gained {
    /// Where its bytes start.
    offset: u64,
    /// The file's length when they were read.
    size: u64,
    bytes: Vec<u8>,
    /// Whether the file was written anew, shorter than `seen` says it was
    /// read, so that it comes again from 0.
    rewritten: bool,
}

/// Up to [`CHUNK`] bytes of `path` from where `seen` says it was read to,
/// or from 0 where it is now shorter than it was then, as a file its tool
/// wrote again.
fn read_from(path: &Path, seen: FileRead) -> std::io::Result<Gained> {
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
