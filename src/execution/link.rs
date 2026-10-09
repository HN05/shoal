//! The wrapper's daemon connection. A reader task owns the receive half, so
//! waiting for a control is cancel-safe and adjacent frames are never lost.
//! Once its command runs, a reattachable execution survives the daemon going
//! away: the link reconnects in the background and presents the execution's
//! identity to the next daemon.
use anyhow::{Result, anyhow, bail};
use tokio::{
    io::BufReader,
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    sync::mpsc,
    task::JoinHandle,
    time::{sleep, timeout},
};

use crate::{
    cli::client,
    paths::Paths,
    process::identity::Identity,
    protocol::{self, Body, Control, ExecutionEvent, Method, Reattach, Request, Response, timing},
};

pub(super) struct Link {
    connection: Connection,
    reattach: Option<Reattachment>,
}

/// What the link received from the daemon.
#[derive(Debug)]
pub(super) enum Received {
    Control(Control),
    /// The daemon went away; the link is reattaching in the background.
    Detached,
    /// A daemon accepted the execution again. Events sent since the link
    /// detached may have been lost.
    Reattached,
}

/// The daemon refused to take the execution back, so its command must stop.
#[derive(Debug)]
pub(super) struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "daemon refused reattachment: {}", self.0)
    }
}

impl std::error::Error for Refused {}

enum Connection {
    Attached(Attached),
    Reattaching(JoinHandle<Result<Attached, Refused>>),
    Refused(String),
}

struct Reattachment {
    paths: Paths,
    request: Reattach,
    /// Set while the command runs and the daemon's record still names it.
    armed: bool,
}

struct Attached {
    writer: OwnedWriteHalf,
    controls: mpsc::Receiver<Result<Control>>,
    reader: JoinHandle<()>,
}

impl Link {
    /// Send `method` and keep the connection for the execution's lifetime.
    pub(super) async fn open(paths: &Paths, method: Method) -> Result<(Self, Body)> {
        let (reader, writer, body) = client::open_split(paths, method).await?;
        Ok((Self::new(reader, writer), body))
    }

    /// Read controls from `reader`, including any already buffered.
    pub(super) fn new(reader: BufReader<OwnedReadHalf>, writer: OwnedWriteHalf) -> Self {
        Self {
            connection: Connection::Attached(Attached::new(reader, writer)),
            reattach: None,
        }
    }

    /// Let the execution reattach once its command is registered; the
    /// request's child and process group are filled in then.
    pub(super) fn allow_reattach(&mut self, paths: &Paths, request: Reattach) {
        self.reattach = Some(Reattachment {
            paths: paths.clone(),
            request,
            armed: false,
        });
    }

    /// Reattach with this child from now on, if the execution allows it.
    pub(super) fn arm(&mut self, child: Option<Identity>, group_id: u32) {
        if let Some(reattach) = &mut self.reattach {
            reattach.request.child = child;
            reattach.request.group_id = group_id;
            reattach.armed = true;
        }
    }

    /// Stop reattaching: the daemon's record no longer names a running child.
    pub(super) fn disarm(&mut self) {
        if let Some(reattach) = &mut self.reattach {
            reattach.armed = false;
        }
    }

    pub(super) fn is_attached(&self) -> bool {
        matches!(self.connection, Connection::Attached(_))
    }

    /// Why the daemon refused this execution, once it has.
    pub(super) fn refusal(&self) -> Option<Refused> {
        match &self.connection {
            Connection::Refused(reason) => Some(Refused(reason.clone())),
            _ => None,
        }
    }

    /// Send an event. While detached the event is dropped, and a failed write
    /// of an armed link is left for the reader to notice as a disconnect;
    /// callers resend after [`Received::Reattached`].
    pub(super) async fn send(&mut self, event: &ExecutionEvent) -> Result<()> {
        let armed = self.armed();
        match &mut self.connection {
            Connection::Attached(attached) => {
                let sent = protocol::write(&mut attached.writer, event).await;
                if armed { Ok(()) } else { sent }
            }
            Connection::Reattaching(_) if armed => Ok(()),
            Connection::Reattaching(_) => bail!("daemon disconnected"),
            Connection::Refused(reason) => Err(Refused(reason.clone()).into()),
        }
    }

    /// The next control, or a change of connection. Cancel-safe.
    pub(super) async fn recv(&mut self) -> Result<Received> {
        match &mut self.connection {
            Connection::Attached(attached) => {
                let received = attached
                    .controls
                    .recv()
                    .await
                    .unwrap_or_else(|| Err(anyhow!("daemon disconnected")));
                match received {
                    Ok(Control::Detach) | Err(_) if self.armed() => {
                        self.detach();
                        Ok(Received::Detached)
                    }
                    Ok(control) => Ok(Received::Control(control)),
                    Err(error) => Err(error),
                }
            }
            Connection::Reattaching(task) => {
                let reattached = task
                    .await
                    .unwrap_or_else(|error| Err(Refused(format!("reattachment failed: {error}"))));
                match reattached {
                    Ok(attached) => {
                        self.connection = Connection::Attached(attached);
                        Ok(Received::Reattached)
                    }
                    Err(Refused(reason)) => {
                        self.connection = Connection::Refused(reason.clone());
                        Err(Refused(reason).into())
                    }
                }
            }
            Connection::Refused(reason) => Err(Refused(reason.clone()).into()),
        }
    }

    /// The next control where the execution cannot reattach.
    pub(super) async fn control(&mut self) -> Result<Control> {
        match self.recv().await? {
            Received::Control(control) => Ok(control),
            Received::Detached | Received::Reattached => bail!("daemon disconnected"),
        }
    }

    fn armed(&self) -> bool {
        self.reattach
            .as_ref()
            .is_some_and(|reattach| reattach.armed)
    }

    fn detach(&mut self) {
        let reattach = self.reattach.as_ref().expect("an armed link can reattach");
        let task = tokio::spawn(reattach_with_backoff(
            reattach.paths.clone(),
            reattach.request.clone(),
        ));
        self.connection = Connection::Reattaching(task);
    }
}

impl Drop for Link {
    fn drop(&mut self) {
        if let Connection::Reattaching(task) = &self.connection {
            task.abort();
        }
    }
}

impl Attached {
    fn new(reader: BufReader<OwnedReadHalf>, writer: OwnedWriteHalf) -> Self {
        let (sender, controls) = mpsc::channel(4);
        Self {
            writer,
            controls,
            reader: tokio::spawn(read_controls(reader, sender)),
        }
    }
}

impl Drop for Attached {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

async fn read_controls(
    mut reader: BufReader<OwnedReadHalf>,
    controls: mpsc::Sender<Result<Control>>,
) {
    loop {
        let control = protocol::read_buffered::<Control>(&mut reader).await;
        let failed = control.is_err();
        if controls.send(control).await.is_err() || failed {
            break;
        }
    }
}

/// Retry until a daemon answers; only its answer can refuse the execution.
async fn reattach_with_backoff(paths: Paths, request: Reattach) -> Result<Attached, Refused> {
    let mut delay = timing::REATTACH_RETRY_INITIAL;
    loop {
        if let Some(attached) = attempt(&paths, &request).await? {
            return Ok(attached);
        }
        sleep(delay).await;
        delay = (delay * 2).min(timing::REATTACH_RETRY_MAX);
    }
}

/// `None` while no daemon is accepting the request.
async fn attempt(paths: &Paths, request: &Reattach) -> Result<Option<Attached>, Refused> {
    let Ok(stream) = UnixStream::connect(&paths.socket).await else {
        return Ok(None);
    };
    let (reader, mut writer) = stream.into_split();
    // The launch scope belongs to the previous daemon; the request carries
    // the execution's own token instead.
    let request = Request {
        protocol: protocol::VERSION,
        id: 1,
        method: Method::Reattach(request.clone()),
        scope: None,
    };
    if protocol::write(&mut writer, &request).await.is_err() {
        return Ok(None);
    }
    let mut reader = BufReader::new(reader);
    let reply = timeout(
        timing::REATTACH_RESPONSE_TIMEOUT,
        protocol::read_buffered::<Response>(&mut reader),
    )
    .await;
    // Any protocol version may answer; only its verdict matters.
    let body = match reply {
        Ok(Ok(reply)) => reply.body,
        Ok(Err(error)) if !client::stream_closed(&error) && !client::is_unreachable(&error) => {
            return Err(Refused(format!("{error:#}")));
        }
        Ok(Err(_)) | Err(_) => return Ok(None),
    };
    <()>::try_from(body).map_err(|error| Refused(format!("{error:#}")))?;
    Ok(Some(Attached::new(reader, writer)))
}
