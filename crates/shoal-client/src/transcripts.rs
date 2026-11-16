//! Where agent tools keep their sessions' files on this machine, such as a
//! session's transcript. Shoal finds them by name and never reads them: an
//! app that shows a conversation reads its tool's own format. The daemon
//! says which sessions a space's agents reported, and
//! [`space_transcripts`] finds their files.
use shoal_protocol::transcripts::{SpaceSessions, Transcript, locate};

use anyhow::{Context as _, Result};

use crate::connection::{Daemon, request};
use shoal_protocol::protocol::WorkspaceMethod;
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
