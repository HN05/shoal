//! Attaching a terminal on another machine, or a device, to an agent's
//! session here: the daemon makes a terminal of the viewer's size and runs
//! Shoal's own attach on it as the user, the same code a local
//! `shoal attach` takes, then carries what that terminal shows to the
//! viewer and the viewer's keys and size to it, until the attach ends or
//! the viewer goes, which hangs up the terminal and leaves the session
//! running.
mod pty;

use std::{
    os::fd::{AsRawFd as _, OwnedFd},
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};

use anyhow::{Context as _, Result};
use shoal_protocol::{
    attaching::{FromSession, ToSession, Viewer},
    protocol::{self, Body, Response},
    space_names::SpaceSelector,
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
use crate::{
    daemon::{answer::OperationGuard, manager::Manager},
    workers::{Worker, internal_command},
};

/// The most bytes of the terminal's output one frame carries.
const CHUNK: usize = 64 << 10;

/// How long the terminal's processes may write after the attach exited, as
/// a host's client that prints a last line, before the viewer hears it
/// ended.
const LAST_OUTPUT: Duration = Duration::from_secs(1);

/// How long an attach whose viewer went has to end on the hangup before it
/// is killed.
const HANGUP_GRACE: Duration = Duration::from_secs(5);

/// The descriptor the attach writes the session it chose to.
const REPORT: i32 = 3;

/// An attach running for a viewer on a terminal of its own.
pub(crate) struct ViewerTerminal {
    pty: Arc<Pty>,
    attach: Child,
    /// Where the attach writes the session it chose, until it closes it.
    report: Option<Lines<BufReader<UnixStream>>>,
    /// How the attach exited, while its terminal may still show more.
    exited: Option<ExitStatus>,
    screen: Vec<u8>,
}

/// What a viewer's terminal does next.
enum Step {
    /// It shows the viewer this.
    Show(FromSession),
    /// Its attach ended so.
    Ended(ExitStatus),
}

impl Manager {
    /// Start Shoal's attach to a session of the space `selector` names on a
    /// terminal made for `viewer`.
    pub(crate) async fn open_viewer_terminal(
        &self,
        selector: &SpaceSelector,
        viewer: &Viewer,
    ) -> Result<ViewerTerminal> {
        let space = self.space(selector).await?;
        let machine = self.machine_name().await;
        let (pty, terminal) = pty::open(viewer.size)?;
        let (report, reported) = std::os::unix::net::UnixStream::pair()?;
        let viewer_json = serde_json::to_string(viewer)?;
        let argv = internal_command(
            &self.paths,
            false,
            Worker::Attach {
                space: &space.id,
                viewer: &viewer_json,
                machine: &machine,
                report: REPORT,
            },
        )?;
        let mut command = attach_command(&argv, viewer, terminal, reported.as_raw_fd())?;
        command.current_dir(&self.paths.home);
        let attach = command.spawn().context("start the attach")?;
        drop(reported);
        report.set_nonblocking(true)?;
        let report = BufReader::new(UnixStream::from_std(report)?).lines();
        Ok(ViewerTerminal {
            pty: Arc::new(pty),
            attach,
            report: Some(report),
            exited: None,
            screen: vec![0; CHUNK],
        })
    }

    /// Answer the request `id`, then carry `terminal`'s output, and the
    /// session its attach chose, to the viewer on `stream`, and the
    /// viewer's keys and size to it, until the attach ends, which sends its
    /// exit code, or the viewer goes or a daemon restart closes it, which
    /// hangs the terminal up.
    pub(crate) async fn relay_viewer_terminal(
        self: Arc<Self>,
        stream: UnixStream,
        id: u64,
        mut terminal: ViewerTerminal,
        startup: OperationGuard,
    ) -> Result<()> {
        let (reader, mut writer) = stream.into_split();
        protocol::write(&mut writer, &Response::new(id, Body::Ok)).await?;
        drop(startup);
        let mut viewer = Viewing {
            manager: &self,
            writer,
        };
        let mut input = ViewerInput::start(terminal.pty.clone(), reader);
        loop {
            match terminal.next(&mut input).await? {
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
    /// What the terminal shows the viewer next; none once the viewer goes,
    /// as `input` tells.
    async fn next(&mut self, input: &mut ViewerInput) -> Result<Option<Step>> {
        loop {
            tokio::select! {
                read = self.pty.read(&mut self.screen) => {
                    let step = match read? {
                        0 => Step::Ended(self.ended().await?),
                        read => Step::Show(FromSession::Screen(self.screen[..read].to_vec())),
                    };
                    return Ok(Some(step));
                }
                line = next_report(&mut self.report), if self.report.is_some() => match line? {
                    Some(line) => {
                        let chosen = serde_json::from_str(&line).context("the attach's session")?;
                        return Ok(Some(Step::Show(FromSession::Session(chosen))));
                    }
                    None => self.report = None,
                },
                () = input.gone() => return Ok(None),
                status = self.attach.wait(), if self.exited.is_none() => {
                    self.exited = Some(status?);
                }
                () = tokio::time::sleep(LAST_OUTPUT), if self.exited.is_some() => {
                    return Ok(Some(Step::Ended(self.ended().await?)));
                }
            }
        }
    }

    /// How the attach exited.
    async fn ended(&mut self) -> Result<ExitStatus> {
        match self.exited {
            Some(status) => Ok(status),
            None => Ok(self.attach.wait().await?),
        }
    }

    /// Hang up the terminal, whose viewer went, so the host's client on it
    /// detaches and leaves the session running, and wait for the attach to
    /// end, killing it once it outlasts [`HANGUP_GRACE`].
    async fn hang_up(self, input: ViewerInput) -> Result<()> {
        // Once it ends, nothing else holds the terminal open.
        input.stop().await;
        let Self {
            pty, mut attach, ..
        } = self;
        drop(pty);
        if tokio::time::timeout(HANGUP_GRACE, attach.wait())
            .await
            .is_err()
        {
            attach.kill().await?;
        }
        Ok(())
    }
}

/// The next session the attach reports on `report`; none once it closes.
async fn next_report(report: &mut Option<Lines<BufReader<UnixStream>>>) -> Result<Option<String>> {
    match report {
        Some(report) => Ok(report.next_line().await?),
        None => Ok(None),
    }
}

/// The command that runs `argv`, the attach, as the user on `terminal`,
/// with `viewer`'s `TERM`, keeping `reported` open as its report
/// descriptor.
fn attach_command(
    argv: &[std::ffi::OsString],
    viewer: &Viewer,
    terminal: OwnedFd,
    reported: i32,
) -> Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env("TERM", viewer.term.as_deref().unwrap_or("xterm-256color"))
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
/// terminal, in a session of its own, and keep `report` open across exec.
/// Runs between fork and exec.
fn controlling_terminal(report: i32) -> std::io::Result<()> {
    // SAFETY: setsid, ioctl and fcntl are async-signal-safe and act on this
    // process and its own descriptors.
    unsafe {
        if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
            return Err(std::io::Error::last_os_error());
        }
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

/// Where an attach writes for its viewer: the viewer's connection.
struct Viewing<'a> {
    manager: &'a Manager,
    writer: OwnedWriteHalf,
}

impl Viewing<'_> {
    /// Send `frame` under a daemon operation of its own, so an attach holds
    /// off a restart only while it writes; `false` once a restart holds
    /// operations off, which ends it.
    async fn send(&mut self, frame: FromSession) -> Result<bool> {
        let Some(_operation) = self.manager.gates.try_operation() else {
            return Ok(false);
        };
        // A viewer that went ends the attach as a hangup does.
        Ok(self.writer.write_all(&frame.encode()).await.is_ok())
    }
}

#[cfg(test)]
mod tests;
