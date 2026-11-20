//! Terminals the daemon makes for viewers on other machines, or devices,
//! to see a place here: a terminal of the viewer's size and `TERM`, on
//! which the daemon runs one of Shoal's workers as the user, such as the
//! attach to an agent's session, then carries what that terminal shows to
//! the viewer and the viewer's keys and size to it, until the worker ends
//! or the viewer goes, which hangs up the terminal and leaves the place
//! running.
mod pty;

use std::{
    ffi::OsString,
    os::fd::{AsRawFd as _, OwnedFd},
    path::Path,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result};
use shoal_protocol::{
    attaching::{AttachReport, FromSession, Terminal, ToSession},
    protocol::{self, Body, Response},
};
use tokio::{
    io::{AsyncBufReadExt as _, AsyncWriteExt as _, BufReader, Lines},
    net::{
        UnixStream,
        unix::{OwnedReadHalf, OwnedWriteHalf},
    },
    process::Child,
    sync::mpsc,
    task::JoinHandle,
};

use self::pty::Pty;
use crate::daemon::{answer::OperationGuard, manager::Manager};

/// The most bytes of the terminal's output one frame carries.
const CHUNK: usize = 64 << 10;

/// How long the terminal's processes may write after the worker exited, as
/// a host's client that prints a last line, before the viewer hears it
/// ended.
const LAST_OUTPUT: Duration = Duration::from_secs(1);

/// How long a worker whose viewer went has to end on the hangup before it
/// is killed.
const HANGUP_GRACE: Duration = Duration::from_secs(5);

/// The descriptor a worker that reports writes its [`AttachReport`]s to.
pub(crate) const REPORT: i32 = 3;

/// A worker running for a viewer on a terminal of its own.
pub(crate) struct ViewerTerminal {
    pty: Arc<Pty>,
    worker: Child,
    /// Where an attach reports the session it chose and whether it left it
    /// open, until it closes it.
    report: Option<Lines<BufReader<UnixStream>>>,
    /// How the worker exited, while its terminal may still show more.
    exited: Option<ExitStatus>,
    screen: Vec<u8>,
}

/// What a viewer's terminal does next.
enum Step {
    /// It shows the viewer this.
    Show(FromSession),
    /// Its worker ended so.
    Ended(ExitStatus),
}

impl Manager {
    /// Answer the request `id` with `answer`, then carry `terminal`'s
    /// output, and what its worker reports, to the viewer on `stream`, and
    /// the viewer's keys and size to it, until the worker ends, which sends
    /// its exit code, or the viewer goes, `until` resolves or a daemon
    /// restart closes it, which hangs the terminal up.
    pub(crate) async fn relay_viewer_terminal(
        self: Arc<Self>,
        stream: UnixStream,
        (id, answer): (u64, Body),
        mut terminal: ViewerTerminal,
        until: impl Future<Output = ()>,
        startup: OperationGuard,
    ) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        protocol::write(&mut writer, &Response::new(id, answer)).await?;
        drop(startup);
        let mut viewer = Viewing {
            manager: &self,
            writer,
        };
        let mut input = ViewerInput::start(terminal.pty.clone(), reader);
        let mut until = std::pin::pin!(until);
        loop {
            let next = tokio::select! {
                next = terminal.next(&mut input) => next?,
                () = &mut until => None,
            };
            match next {
                Some(Step::Show(frame)) => {
                    if !viewer.send(frame).await? {
                        return terminal.hang_up(input).await;
                    }
                }
                Some(Step::Ended(status)) => {
                    input.stop().await;
                    let code = crate::execution::exit_code(status);
                    return viewer.send(FromSession::Exited(code)).await.map(drop);
                }
                None => return terminal.hang_up(input).await,
            }
        }
    }
}

impl ViewerTerminal {
    /// Start `argv`, a worker, in `dir` as the user on a terminal made for
    /// `viewer`, with the descriptor [`REPORT`] open for its reports.
    pub(crate) fn start(argv: &[OsString], viewer: &Terminal, dir: &Path) -> Result<Self> {
        let (pty, terminal) = pty::open(viewer.size)?;
        let (report, reported) = std::os::unix::net::UnixStream::pair()?;
        let term = viewer.term.as_deref();
        let mut command = worker_command(argv, term, terminal, Some(reported.as_raw_fd()))?;
        command.current_dir(dir);
        let worker = command.spawn().context("start the viewer's terminal")?;
        drop(reported);
        report.set_nonblocking(true)?;
        let report = BufReader::new(UnixStream::from_std(report)?).lines();
        Ok(Self {
            pty: Arc::new(pty),
            worker,
            report: Some(report),
            exited: None,
            screen: vec![0; CHUNK],
        })
    }

    /// What the terminal shows the viewer next; none once the viewer goes,
    /// as `input` tells.
    async fn next(&mut self, input: &mut ViewerInput) -> Result<Option<Step>> {
        loop {
            tokio::select! {
                read = self.pty.read(&mut self.screen) => {
                    let step = match read? {
                        0 => self.last_step().await?,
                        read => Step::Show(FromSession::Screen(self.screen[..read].to_vec())),
                    };
                    return Ok(Some(step));
                }
                report = next_report(&mut self.report), if self.report.is_some() => match report? {
                    Some(report) => return Ok(Some(Step::Show(shown_report(report)))),
                    None => self.report = None,
                },
                () = input.gone() => return Ok(None),
                status = self.worker.wait(), if self.exited.is_none() => {
                    self.exited = Some(status?);
                }
                () = tokio::time::sleep(LAST_OUTPUT), if self.exited.is_some() => {
                    return Ok(Some(self.last_step().await?));
                }
            }
        }
    }

    /// The step after the terminal's last output: a report the worker
    /// wrote before it exited, which the viewer hears before the end, else
    /// the end.
    async fn last_step(&mut self) -> Result<Step> {
        match self.last_report().await? {
            Some(report) => Ok(Step::Show(shown_report(report))),
            None => Ok(Step::Ended(self.ended().await?)),
        }
    }

    /// The worker's next report, once its terminal shows no more: none
    /// once it closes its report, or [`LAST_OUTPUT`] after it exited, as
    /// when a process it started holds the report open.
    async fn last_report(&mut self) -> Result<Option<AttachReport>> {
        while self.report.is_some() {
            tokio::select! {
                report = next_report(&mut self.report) => match report? {
                    Some(report) => return Ok(Some(report)),
                    None => self.report = None,
                },
                status = self.worker.wait(), if self.exited.is_none() => {
                    self.exited = Some(status?);
                }
                () = tokio::time::sleep(LAST_OUTPUT), if self.exited.is_some() => {
                    self.report = None;
                }
            }
        }
        Ok(None)
    }

    /// How the worker exited.
    async fn ended(&mut self) -> Result<ExitStatus> {
        match self.exited {
            Some(status) => Ok(status),
            None => Ok(self.worker.wait().await?),
        }
    }

    /// Hang up the terminal, whose viewer went, so the host's client on it
    /// detaches and leaves the place running, and wait for the worker to
    /// end, killing it once it outlasts [`HANGUP_GRACE`].
    async fn hang_up(self, input: ViewerInput) -> Result<()> {
        // Once it ends, nothing else holds the terminal open.
        input.stop().await;
        let Self {
            pty, mut worker, ..
        } = self;
        drop(pty);
        if tokio::time::timeout(HANGUP_GRACE, worker.wait())
            .await
            .is_err()
        {
            worker.kill().await?;
        }
        Ok(())
    }
}

/// The frame that shows the viewer what the attach reported.
fn shown_report(report: AttachReport) -> FromSession {
    match report {
        AttachReport::Chosen(chosen) => FromSession::Session(chosen),
        AttachReport::Left { open } => FromSession::Left(open),
    }
}

/// The next report the attach writes on `report`; none once it closes.
async fn next_report(
    report: &mut Option<Lines<BufReader<UnixStream>>>,
) -> Result<Option<AttachReport>> {
    let Some(report) = report else {
        return Ok(None);
    };
    match report.next_line().await? {
        Some(line) => Ok(Some(
            serde_json::from_str(&line).context("the attach's report")?,
        )),
        None => Ok(None),
    }
}

/// The command that runs `argv`, a worker, as the user on `terminal`,
/// with the viewer's `TERM`, `term`, keeping `reported`, when given, open
/// as its report descriptor.
fn worker_command(
    argv: &[OsString],
    term: Option<&str>,
    terminal: OwnedFd,
    reported: Option<i32>,
) -> Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env("TERM", term.unwrap_or("xterm-256color"))
        .env_remove(shoal_protocol::env::SCOPE_TOKEN)
        .stdin(Stdio::from(terminal.try_clone()?))
        .stdout(Stdio::from(terminal.try_clone()?))
        .stderr(Stdio::from(terminal))
        .kill_on_drop(true);
    // The terminal is no host's, whatever host's place or server this
    // daemon started in: a host reaches the place it shows through the
    // server the place records.
    for places in crate::terminal::hosts().filter_map(|host| host.places()) {
        let variables = places.place_variables().iter();
        for variable in variables.chain(places.server_variables()) {
            command.env_remove(variable);
        }
    }
    // SAFETY: the hook makes only async-signal-safe calls.
    unsafe {
        command.pre_exec(move || controlling_terminal(reported));
    }
    Ok(command)
}

/// Make the terminal on standard input this new process's controlling
/// terminal, in a session of its own, and keep `report`, when given, open
/// across exec. Runs between fork and exec.
fn controlling_terminal(report: Option<i32>) -> std::io::Result<()> {
    // SAFETY: setsid, ioctl and fcntl are async-signal-safe and act on this
    // process and its own descriptors.
    unsafe {
        if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
            return Err(std::io::Error::last_os_error());
        }
        let Some(report) = report else {
            return Ok(());
        };
        if libc::dup2(report, REPORT) == -1 {
            return Err(std::io::Error::last_os_error());
        }
        // dup2 onto itself keeps close-on-exec, which nothing else sets.
        if libc::fcntl(REPORT, libc::F_SETFD, 0) == -1 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// The viewer's keys and sizes: one task reads them as they come and ends
/// once the viewer goes, and another types them into the terminal, so a
/// paste the terminal takes slowly holds up neither what it shows nor
/// noticing that the viewer went.
struct ViewerInput {
    reading: JoinHandle<()>,
    typing: JoinHandle<()>,
}

impl ViewerInput {
    fn start(pty: Arc<Pty>, reader: OwnedReadHalf) -> Self {
        // Unbounded, so reading never waits for typing: what waits is what
        // the viewer typed.
        let (sender, typed) = mpsc::unbounded_channel();
        Self {
            reading: tokio::spawn(read_input(reader, sender)),
            typing: tokio::spawn(type_input(pty, typed)),
        }
    }

    /// Resolves once the viewer goes; polled no more after that.
    async fn gone(&mut self) {
        let _ = (&mut self.reading).await;
    }

    /// Stop reading and typing, once neither holds the terminal.
    async fn stop(self) {
        self.reading.abort();
        self.typing.abort();
        let _ = self.typing.await;
    }
}

/// Pass the viewer's frames on `reader` to `typed` until the viewer goes.
async fn read_input(mut reader: OwnedReadHalf, typed: mpsc::UnboundedSender<ToSession>) {
    while let Ok(Some(frame)) = ToSession::read(&mut reader).await {
        if typed.send(frame).is_err() {
            return;
        }
    }
}

/// Type the viewer's keys into `pty` and give it each new size, in turn.
/// A terminal whose processes ended takes no keys, which go nowhere then;
/// the relay reports the end.
async fn type_input(pty: Arc<Pty>, mut typed: mpsc::UnboundedReceiver<ToSession>) {
    while let Some(frame) = typed.recv().await {
        let _ = match frame {
            ToSession::Keys(keys) => pty.write_all(&keys).await,
            ToSession::Resize(size) => pty.resize(size),
        };
    }
}

/// Where a worker's terminal writes for its viewer: the viewer's connection.
struct Viewing<'a> {
    manager: &'a Manager,
    writer: OwnedWriteHalf,
}

impl Viewing<'_> {
    /// Send `frame` under a daemon operation of its own, so a viewer's
    /// terminal holds off a restart only while it writes; `false` once a
    /// restart holds operations off, which ends it.
    async fn send(&mut self, frame: FromSession) -> Result<bool> {
        let Some(_operation) = self.manager.gates.try_operation() else {
            return Ok(false);
        };
        // A viewer that went ends the worker as a hangup does.
        Ok(self.writer.write_all(&frame.encode()).await.is_ok())
    }
}

#[cfg(test)]
mod tests;
