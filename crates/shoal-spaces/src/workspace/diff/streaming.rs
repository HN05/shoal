//! A workspace's diff streamed to its caller, as to one on another machine:
//! what Git writes as it writes it, then its exit code. It runs as a plain
//! read of the worktree, so it counts as no use of the workspace.
use crate::Spaces;
use crate::daemon::Daemon;
use std::process::Stdio;

use anyhow::{Context as _, Result};
use shoal_protocol::{
    diffs::{DiffDisplay, DiffOutput},
    protocol::Body,
    space_names::SpaceSelector,
};
use tokio::{
    io::{AsyncRead, AsyncReadExt as _},
    net::UnixStream,
    process::Child,
};

use crate::{
    frames::{self, OperationGuard},
    git,
};

/// Git writing a workspace's diff, which a client reads.
pub struct RunningDiff(Child);

impl Spaces {
    /// Start `git diff` in the worktree of the workspace `selector` names,
    /// from the commit `shoal diff` compares against, shown as `display`
    /// says.
    pub async fn start_diff(
        &self,
        daemon: &dyn Daemon,
        selector: &SpaceSelector,
        display: DiffDisplay,
    ) -> Result<RunningDiff> {
        let base = self.diff_base(daemon, selector).await?;
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
    pub async fn stream_diff(
        &self,
        stream: UnixStream,
        id: u64,
        RunningDiff(mut git): RunningDiff,
        startup: OperationGuard,
    ) -> Result<()> {
        let (mut reader, writer) = stream.into_split();
        let mut client = frames::Frames::new(&self.gates, writer, id);
        drop(startup);
        let mut stdout = git.stdout.take().context("git diff's output")?;
        let mut stderr = git.stderr.take().context("git diff's errors")?;
        let (mut out_open, mut err_open) = (true, true);
        let (mut out, mut err) = (vec![0; frames::CHUNK], vec![0; frames::CHUNK]);
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
                () = frames::hangup(&mut reader) => return Ok(()),
            };
            if !client.send(Body::Diff(frame)).await? {
                return Ok(());
            }
        }
        let status = git.wait().await.context("wait for git diff")?;
        let exited = DiffOutput::Exited(crate::execution::exit_code(status));
        client.send(Body::Diff(exited)).await.map(drop)
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
