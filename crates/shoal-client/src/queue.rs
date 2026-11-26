//! Requests about the agent queue, the hold that keeps a starting entry's
//! slot, and a `shoal run` command's admission.
use shoal_ids::queue_entry_ids::QueueEntryId;
use shoal_protocol::queue::WorkerStart;
use std::time::Duration;

use anyhow::Result;
use tokio::{io::AsyncReadExt as _, sync::oneshot};

use crate::connection::{Daemon, open, open_split, request, stream_closed};
use shoal_protocol::space_names::SpaceSelector;
use shoal_protocol::{
    model::{QueueEntry, QueuedPlan},
    protocol::QueueMethod,
    runs::{AdmissionFrame, AdmitAs, CommandKey, RunReport},
};

/// How long the daemon may take to answer a command's admission request.
const ADMISSION_REPLY: Duration = Duration::from_secs(60);

/// Reconnect a hold this soon after the daemon restarts.
const HOLD_RETRY: Duration = Duration::from_secs(1);

/// Queue the worker `start` describes, which the daemon resolves under its
/// workspace's settings and starts detached once admitted, and return its
/// entry.
pub async fn start_agent(daemon: &impl Daemon, start: WorkerStart) -> Result<QueueEntry> {
    let WorkerStart {
        workspace,
        launch,
        blockers,
        pr,
    } = start;
    let method = QueueMethod::QueueAgent {
        workspace,
        launch: Box::new(launch),
        blockers,
        pr,
    };
    request(daemon, method).await
}

pub async fn entries(daemon: &impl Daemon) -> Result<Vec<QueueEntry>> {
    request(daemon, QueueMethod::ListQueue).await
}

/// The launch plan queued with `entry`.
pub async fn plan(daemon: &impl Daemon, entry: QueueEntryId) -> Result<QueuedPlan> {
    request(daemon, QueueMethod::QueuedPlan { entry }).await
}

/// Have the daemon open the space of the starting launch `entry` for the
/// agent this process runs there now, and return its plan.
pub async fn open_start(daemon: &impl Daemon, entry: QueueEntryId) -> Result<QueuedPlan> {
    request(daemon, QueueMethod::OpenStart { entry }).await
}

/// Start the entry `entry` names by ID now, whatever waits before it.
pub async fn start(daemon: &impl Daemon, entry: String) -> Result<QueueEntry> {
    let here = false;
    request(daemon, QueueMethod::QueueStart { entry, here }).await
}

/// Mark the waiting launch `entry` starting for this process, which starts
/// it itself, now, whatever waits before it, and holds its slot.
pub async fn start_here(daemon: &impl Daemon, entry: &QueueEntryId) -> Result<QueueEntry> {
    let (entry, here) = (entry.to_string(), true);
    request(daemon, QueueMethod::QueueStart { entry, here }).await
}

/// Start the launch `entry`, which this process holds, here once its
/// workspace is prepared: at once when its start asked for that, else when
/// the limits admit it now; one they do not stays waiting for the queue.
pub async fn claim(daemon: &impl Daemon, entry: &QueueEntryId) -> Result<QueueEntry> {
    let entry = entry.clone();
    request(daemon, QueueMethod::QueueClaim { entry }).await
}

/// The workspace the launch `entry`, which left the queue, started its
/// agent in, while the daemon still knows it.
pub async fn started_workspace(
    daemon: &impl Daemon,
    entry: QueueEntryId,
) -> Result<Option<shoal_ids::space_ids::SpaceId>> {
    request(daemon, QueueMethod::StartedLaunch { entry }).await
}

/// Take the entry `entry` names by ID out of the queue.
pub async fn remove(daemon: &impl Daemon, entry: String) -> Result<QueueEntry> {
    request(daemon, QueueMethod::QueueRemove { entry }).await
}

/// Record `space` as the workspace the starting `entry` opened, or the
/// existing one the waiting launch `entry` opens, or the chat the launch
/// `entry` starts its agent in.
pub async fn attach(daemon: &impl Daemon, entry: QueueEntryId, space: SpaceSelector) -> Result<()> {
    request(daemon, QueueMethod::QueueAttach { entry, space }).await
}

/// Free the slot of the starting `entry`, whose launch failed with `error`;
/// the daemon tells the user.
pub async fn fail(daemon: &impl Daemon, entry: QueueEntryId, error: String) -> Result<()> {
    request(daemon, QueueMethod::QueueFail { entry, error }).await
}

/// Queue the failed launch `entry` again as it was, starting it as `start`
/// says, and return it as it waits or starts.
pub async fn retry(
    daemon: &impl Daemon,
    entry: QueueEntryId,
    start: shoal_protocol::protocol::RetryStart,
) -> Result<QueueEntry> {
    request(daemon, QueueMethod::QueueRetry { entry, start }).await
}

/// Keeps a starting entry's slot, or a waiting entry out of admission for
/// its holder to start, while it lives, until the entry leaves the queue.
/// It reconnects after a daemon restart.
pub struct Hold(tokio::task::JoinHandle<()>);

impl Hold {
    /// Returns once the daemon holds the slot, ends the entry or cannot be
    /// reached. A daemon at its connection limit closes the hold unanswered,
    /// so preparation waits instead of outliving the reservation.
    pub async fn start(
        daemon: &(impl Daemon + Clone + Send + 'static),
        entry: &QueueEntryId,
    ) -> Self {
        let (settled, held) = oneshot::channel();
        let hold = Self(tokio::spawn(keep(daemon.clone(), entry.clone(), settled)));
        let _ = held.await;
        hold
    }
}

async fn keep(daemon: impl Daemon, entry: QueueEntryId, settled: oneshot::Sender<()>) {
    let mut settled = Some(settled);
    loop {
        let method = QueueMethod::QueueHold {
            entry: entry.clone(),
        };
        let opened = open(&daemon, method).await;
        if !matches!(&opened, Err(error) if stream_closed(error))
            && let Some(settled) = settled.take()
        {
            let _ = settled.send(());
        }
        if let Ok((mut stream, body)) = opened {
            // A refusal means the agent took the slot over or the entry ended.
            if body.into_result().is_err() {
                return;
            }
            let mut closed = [0u8; 1];
            let _ = stream.read(&mut closed).await;
        }
        tokio::time::sleep(HOLD_RETRY).await;
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// A `shoal run` command's admission, from its request until the command
/// ends and the connection with it.
pub struct CommandAdmission {
    frames: tokio::sync::mpsc::Receiver<Result<AdmissionFrame>>,
    writer: tokio::net::unix::OwnedWriteHalf,
}

impl CommandAdmission {
    /// Ask to run `command` in `space`, as `admit_as` says, at once with
    /// `now`.
    pub async fn request(
        daemon: &impl Daemon,
        space: SpaceSelector,
        command: CommandKey,
        now: bool,
        admit_as: AdmitAs,
    ) -> Result<Self> {
        let method = QueueMethod::AdmitCommand {
            space,
            command,
            now,
            admit_as,
        };
        let (reader, writer, body) = open_split(daemon, method, ADMISSION_REPLY).await?;
        body.into_result()?;
        Ok(Self {
            frames: admission_frames(reader),
            writer,
        })
    }

    /// The daemon's next frame. Cancelling it, as `tokio::select!` does,
    /// loses none.
    pub async fn frame(&mut self) -> Result<AdmissionFrame> {
        self.frames
            .recv()
            .await
            .unwrap_or_else(|| Err(anyhow::anyhow!("the daemon closed the admission")))
    }

    /// Tell the daemon `report`.
    pub async fn report(&mut self, report: RunReport) -> Result<()> {
        shoal_protocol::protocol::write(&mut self.writer, &report).await
    }
}

/// The frames the daemon sends on `reader`, until it fails or hangs up.
fn admission_frames(
    mut reader: tokio::io::BufReader<tokio::net::unix::OwnedReadHalf>,
) -> tokio::sync::mpsc::Receiver<Result<AdmissionFrame>> {
    let (sender, receiver) = tokio::sync::mpsc::channel(4);
    tokio::spawn(async move {
        loop {
            let frame = shoal_protocol::protocol::read_buffered(&mut reader).await;
            let failed = frame.is_err();
            if sender.send(frame).await.is_err() || failed {
                break;
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests;
