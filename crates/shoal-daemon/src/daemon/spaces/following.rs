//! Following a space's agent session: the bytes of its files where its tool
//! keeps them, everything so far and then what the tool appends, copied
//! unread as an export copies them. A follow that names no session follows
//! the space's current one, going on in each new one its agent reports.
use std::{
    collections::HashMap,
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result};
use shoal_ids::{agent_session_ids::AgentSessionId, space_ids::SpaceId};
use shoal_protocol::{
    protocol::{self, Body, Response},
    space_names::SpaceSelector,
    transcripts::{self, Environments, FollowedSession, ReportedSession, SessionBytes, Store},
};
use tokio::net::{UnixStream, unix::OwnedWriteHalf};

use crate::{
    daemon::{
        answer::{self, OperationGuard},
        workspace::Manager,
    },
    refusal::{Reason, Refusal},
};

/// How often a follow looks for what its session's files gained.
const POLL: Duration = Duration::from_millis(500);

/// The most bytes of a file one frame carries.
const CHUNK: usize = 48 << 10;

/// A session to follow, of a space here.
pub(crate) struct Following {
    space: SpaceId,
    /// Whether the follow named it, so that it stays with it.
    named: bool,
    session: FollowedSession,
    store: &'static dyn Store,
    environments: Arc<Environments>,
}

impl Manager {
    /// The session `session` of an agent of the space `selector` names, or
    /// without one its current session, which a follow follows; refused
    /// for a session its agents never reported, so a follow reads no other
    /// session's files.
    pub(crate) async fn session_to_follow(
        &self,
        selector: &SpaceSelector,
        session: Option<AgentSessionId>,
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
            space,
            named,
            session: FollowedSession {
                session: reported,
                integration,
            },
            store,
            environments: Arc::new(Environments::new(&self.paths.home, &sessions.environments)),
        })
    }

    /// Send `following`'s session, then its files' bytes as they grow,
    /// until the client hangs up or a daemon restart closes the follow.
    pub(crate) async fn follow_session(
        self: Arc<Self>,
        stream: UnixStream,
        id: u64,
        mut following: Following,
        startup: OperationGuard,
    ) -> Result<()> {
        let (mut reader, mut writer) = stream.into_split();
        let mut offsets = HashMap::new();
        send(
            &mut writer,
            id,
            Body::FollowedSession(following.session.clone()),
        )
        .await?;
        drop(startup);
        loop {
            // A restarting daemon closes the follow, which the client opens
            // again on its replacement.
            let Some(operation) = self.gates.try_operation() else {
                return Ok(());
            };
            if let Some(next) = self.next_session(&following).await? {
                following.session.session = next;
                offsets.clear();
                send(
                    &mut writer,
                    id,
                    Body::FollowedSession(following.session.clone()),
                )
                .await?;
            }
            send_gained(&mut writer, id, &following, &mut offsets).await?;
            drop(operation);
            tokio::select! {
                () = tokio::time::sleep(POLL) => {}
                () = answer::hangup(&mut reader) => return Ok(()),
            }
        }
    }

    /// The space's current session where a follow that named none goes on
    /// in it, once it is another than the one it follows.
    async fn next_session(&self, following: &Following) -> Result<Option<ReportedSession>> {
        if following.named {
            return Ok(None);
        }
        let current = self.reported_sessions(&following.space).await?;
        let current = current.into_iter().next();
        Ok(current.filter(|current| {
            current.session != following.session.session.session
                && current.agent == following.session.session.agent
        }))
    }
}

/// Send what the files of `following`'s session gained since `offsets`,
/// keeping where each now ends.
async fn send_gained(
    writer: &mut OwnedWriteHalf,
    id: u64,
    following: &Following,
    offsets: &mut HashMap<PathBuf, u64>,
) -> Result<()> {
    let Some(found) = locate(following).await? else {
        return Ok(());
    };
    for file in found.files.files {
        let path = found.directory.join(&file);
        send_file_gained(writer, id, &path, file, offsets).await?;
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

/// Send what the session's file `file`, at `path`, gained since its offset
/// in `offsets`, keeping where it now ends.
async fn send_file_gained(
    writer: &mut OwnedWriteHalf,
    id: u64,
    path: &Path,
    file: PathBuf,
    offsets: &mut HashMap<PathBuf, u64>,
) -> Result<()> {
    loop {
        let offset = offsets.get(&file).copied().unwrap_or(0);
        let path = path.to_owned();
        let read = tokio::task::spawn_blocking(move || read_from(&path, offset));
        // Gone, as a file its tool replaced, which may come back.
        let Ok((offset, bytes)) = read.await.context("read a session file")? else {
            return Ok(());
        };
        offsets.insert(file.clone(), offset + bytes.len() as u64);
        if bytes.is_empty() {
            return Ok(());
        }
        let gained = SessionBytes::new(file.clone(), offset, &bytes);
        send(writer, id, Body::SessionBytes(gained)).await?;
    }
}

/// The offset `path` is read from, `offset` unless the file shrank below
/// it, as one its tool wrote again, then 0, and up to [`CHUNK`] bytes from
/// there.
fn read_from(path: &Path, offset: u64) -> std::io::Result<(u64, Vec<u8>)> {
    let mut file = std::fs::File::open(path)?;
    let offset = match file.metadata()?.len() < offset {
        true => 0,
        false => offset,
    };
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = Vec::with_capacity(CHUNK);
    file.take(CHUNK as u64).read_to_end(&mut bytes)?;
    Ok((offset, bytes))
}

async fn send(writer: &mut OwnedWriteHalf, id: u64, body: Body) -> Result<()> {
    protocol::write(writer, &Response::new(id, body)).await
}

#[cfg(test)]
mod tests;
