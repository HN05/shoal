//! A workspace's diff streamed to its caller, as to one on another machine:
//! what Git writes as it writes it, then its exit code. It runs as a plain
//! read of the worktree, so it counts as no use of the workspace.
use std::{process::Stdio, sync::Arc};

use anyhow::{Context as _, Result};
use shoal_protocol::{
    diffs::{DiffDisplay, DiffOutput},
    protocol::{self, Body, Response},
    space_names::SpaceSelector,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _},
    net::{UnixStream, unix::OwnedWriteHalf},
    process::Child,
};

use crate::{
    daemon::{
        answer::{self, OperationGuard},
        manager::Manager,
    },
    git,
};

/// The most bytes of Git's output one frame carries: what fits in a frame
/// in base64, with room for the rest of the frame.
const CHUNK: usize = (protocol::MAX_FRAME - (8 << 10)) / 4 * 3;

/// Git writing a workspace's diff, which a client reads.
pub(crate) struct RunningDiff(Child);

impl Manager {
    /// Start `git diff` in the worktree of the workspace `selector` names,
    /// from the commit `shoal diff` compares against, shown as `display`
    /// says.
    pub(crate) async fn start_diff(
        &self,
        selector: &SpaceSelector,
        display: DiffDisplay,
    ) -> Result<RunningDiff> {
        let base = self.diff_base(selector).await?;
        let child = git::diff::shown_elsewhere(&base.path, base.commit.as_str(), display)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("run git diff")?;
        Ok(RunningDiff(child))
    }

    /// Send what `diff`'s Git writes as the answer to the request `id`,
    /// then its exit code, until the client hangs up, which stops Git, or
    /// a daemon restart closes the answer.
    pub(crate) async fn stream_diff(
        self: Arc<Self>,
        stream: UnixStream,
        id: u64,
        RunningDiff(mut git): RunningDiff,
        startup: OperationGuard,
    ) -> Result<()> {
        let (mut reader, writer) = stream.into_split();
        let mut client = Client {
            manager: &self,
            writer,
            id,
        };
        drop(startup);
        let mut stdout = git.stdout.take().context("git diff's output")?;
        let mut stderr = git.stderr.take().context("git diff's errors")?;
        let (mut out_open, mut err_open) = (true, true);
        let (mut out, mut err) = (vec![0; CHUNK], vec![0; CHUNK]);
        while out_open || err_open {
            let frame = tokio::select! {
                read = read_some(&mut stdout, &mut out), if out_open => match read? {
                    Some(bytes) => DiffOutput::stdout(bytes),
                    None => {
                        out_open = false;
                        continue;
                    }
                },
                read = read_some(&mut stderr, &mut err), if err_open => match read? {
                    Some(bytes) => DiffOutput::stderr(bytes),
                    None => {
                        err_open = false;
                        continue;
                    }
                },
                () = answer::hangup(&mut reader) => return Ok(()),
            };
            if !client.send(frame).await? {
                return Ok(());
            }
        }
        let status = git.wait().await.context("wait for git diff")?;
        client
            .send(DiffOutput::Exited(crate::execution::exit_code(status)))
            .await
            .map(drop)
    }
}

/// Where a diff writes: its client's connection.
struct Client<'a> {
    manager: &'a Manager,
    writer: OwnedWriteHalf,
    id: u64,
}

impl Client<'_> {
    /// Send `output` under a daemon operation of its own, so a diff holds
    /// off a restart only while it writes; `false` once a restart holds
    /// operations off, which closes the answer.
    async fn send(&mut self, output: DiffOutput) -> Result<bool> {
        let Some(_operation) = self.manager.gates.try_operation() else {
            return Ok(false);
        };
        let response = Response::new(self.id, Body::Diff(output));
        protocol::write(&mut self.writer, &response).await?;
        Ok(true)
    }
}

/// What `source` writes next into `buffer`; none once it ends.
async fn read_some<'b>(
    source: &mut (impl AsyncRead + Unpin),
    buffer: &'b mut [u8],
) -> Result<Option<&'b [u8]>> {
    let read = source
        .read(buffer)
        .await
        .context("read git diff's output")?;
    Ok((read > 0).then_some(&buffer[..read]))
}
