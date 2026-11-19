//! A workspace's diff on whichever machine has it, as that machine's Git
//! writes it, for a caller whose own Git colours and pages it.
use anyhow::Result;
use shoal_protocol::{
    diffs::{DiffDisplay, DiffOutput},
    protocol::{Body, Responses, WorkspaceMethod},
    space_names::SpaceSelector,
};
use tokio::{io::BufReader, net::UnixStream};

use crate::connection::{Daemon, open_buffered};

/// A diff's frames, as the Git of the workspace's machine writes them.
pub struct Diff {
    responses: Responses<BufReader<UnixStream>>,
    first: Option<Body>,
}

/// Run `git diff` of `workspace` since its branch's fork point on the
/// machine that has it, shown as `display` says. It counts as no use of
/// the workspace. Fails at once when the daemon refuses, as for a
/// workspace on a machine that is offline.
pub async fn diff(
    daemon: &impl Daemon,
    workspace: SpaceSelector,
    display: DiffDisplay,
) -> Result<Diff> {
    let method = WorkspaceMethod::Diff { workspace, display };
    let (stream, first) = open_buffered(daemon, method).await?;
    let first = Some(first.into_result()?);
    Ok(Diff {
        responses: Responses::new(stream),
        first,
    })
}

impl Diff {
    /// The next frame, waiting for Git to write; the last is
    /// [`DiffOutput::Exited`].
    pub async fn next(&mut self) -> Result<DiffOutput> {
        let body = match self.first.take() {
            Some(body) => body,
            None => self.responses.next().await?.body.into_result()?,
        };
        match body {
            Body::Diff(output) => Ok(output),
            body => Err(body.unexpected("Diff")),
        }
    }
}
