//! `shoal attach` of a space on another joined machine: that machine runs
//! the attach on a terminal of its own, which this one shows in raw mode,
//! sending it every key and each new size, so the host's own detach key
//! detaches. A dropped link leaves the session running there; this
//! terminal says so and attaches again to the same session once the
//! machine answers, until Ctrl-C.
use std::{
    io::{Read as _, Write as _},
    time::Duration,
};

use anyhow::{Context as _, Result, bail};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::{
    attaching::{ChosenSession, FromSession, Terminal, TerminalSize, ToSession, Viewer, ViewerApp},
    space_names::SpaceSelector,
};
use tokio::{
    signal::unix::{Signal, SignalKind, signal},
    sync::mpsc,
};

use shoal_core::execution::INPUT_MODES_RESET;

use crate::{
    cli::context::Context,
    client::{
        attaching::{Keys, Screen},
        connection,
    },
};

/// The first wait before attaching again after a dropped link, which
/// doubles up to [`LONGEST_WAIT`].
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(30);

/// The key that stops attaching again.
const CTRL_C: u8 = 0x03;

/// How an attach on another machine ended.
pub(super) struct Ended {
    /// The attach's exit code, which this one exits with.
    pub code: i32,
    /// Whether it left its session running there.
    pub open: bool,
}

/// Attach this terminal to a session of `space`, which runs on `machine`,
/// the one `session` names when given, and end as the attach there does.
/// A signal that ends this process, such as SIGTERM, ends the attach,
/// leaving the terminal as it found it.
pub(super) async fn attach(
    ctx: &Context,
    space: SpaceId,
    machine: &str,
    session: Option<String>,
) -> Result<Ended> {
    let viewer = Viewer {
        session,
        again: None,
        terminal: Terminal {
            size: terminal_size(),
            term: std::env::var("TERM").ok(),
            apps: viewer_apps().await,
        },
    };
    let mut ended = Termination::watch()?;
    let mut raw = RawTerminal::enter()?;
    let attached = tokio::select! {
        attached = keep_attached(ctx, &space, machine, viewer) => attached?,
        signal = ended.signalled() => bail!("stopped by {signal}"),
    };
    raw.left_by_its_client();
    Ok(attached)
}

/// Attach to `space` on `machine` as `viewer` asks, again after each
/// dropped link, until the attach there exits.
async fn keep_attached(
    ctx: &Context,
    space: &SpaceId,
    machine: &str,
    mut viewer: Viewer,
) -> Result<Ended> {
    let mut keys = typed_keys();
    let mut resizes = signal(SignalKind::window_change()).context("watch the terminal's size")?;
    let mut reported = Reported::default();
    let mut wait = FIRST_WAIT;
    loop {
        let space = SpaceSelector::id(space);
        // The terminal is raw, so Ctrl-C is a key, which stops a request
        // whose answer does not come.
        let asked = crate::client::attaching::attach(&ctx.paths, space, viewer.clone());
        let attached = tokio::select! {
            attached = asked => attached,
            () = ctrl_c(&mut keys) => bail!("stopped attaching to machine {machine}"),
        };
        let (screen, input) = match attached {
            Ok(attached) => attached,
            Err(error) if viewer.again.is_some() && lost(&error) => {
                stop_on_ctrl_c(&mut keys, wait, machine).await?;
                wait = (wait * 2).min(LONGEST_WAIT);
                continue;
            }
            Err(error) => return Err(error),
        };
        wait = FIRST_WAIT;
        let shown = Shown {
            keys: &mut keys,
            resizes: &mut resizes,
            reported: &mut reported,
        };
        if let Some(code) = shown.show(screen, input).await? {
            let open = reported.left_open;
            return Ok(Ended { code, open });
        }
        let Some(session) = &reported.chosen else {
            bail!("lost the connection to machine {machine} before it chose a session");
        };
        lost_notice(machine, &session.name);
        viewer.again = Some(session.place.clone());
        stop_on_ctrl_c(&mut keys, wait, machine).await?;
    }
}

/// The signals that would end this process while its terminal is raw,
/// caught so that it restores the terminal first. In raw mode Ctrl-C is a
/// key the attach sends on; these come from elsewhere.
struct Termination(Vec<(&'static str, Signal)>);

impl Termination {
    fn watch() -> Result<Self> {
        let kinds = [
            ("SIGTERM", SignalKind::terminate()),
            ("SIGHUP", SignalKind::hangup()),
            ("SIGINT", SignalKind::interrupt()),
        ];
        let watched = kinds
            .into_iter()
            .map(|(name, kind)| Ok((name, signal(kind)?)));
        Ok(Self(
            watched
                .collect::<std::io::Result<_>>()
                .context("watch for signals")?,
        ))
    }

    /// The name of the first signal that comes.
    async fn signalled(&mut self) -> &'static str {
        let arrivals = self.0.iter_mut().map(|(name, signal)| {
            Box::pin(async move {
                signal.recv().await;
                *name
            })
        });
        futures_util::future::select_all(arrivals).await.0
    }
}

/// What a connection to the attach's machine carries while it lasts.
struct Shown<'a> {
    keys: &'a mut mpsc::UnboundedReceiver<Vec<u8>>,
    resizes: &'a mut Signal,
    reported: &'a mut Reported,
}

/// What the attach on the other machine said of its session.
#[derive(Default)]
struct Reported {
    /// The session it chose, once it says.
    chosen: Option<ChosenSession>,
    /// Whether it left that session running, once it says so.
    left_open: bool,
}

impl Shown<'_> {
    /// Show what `screen` brings and send typed keys and new sizes on
    /// `input`, until the attach exits, giving its code, or the connection
    /// drops, giving none. Keys go on apart from the screen, so a paste the
    /// other machine takes slowly never stops what this terminal shows.
    async fn show(self, screen: Screen, input: Keys) -> Result<Option<i32>> {
        tokio::select! {
            shown = show_screen(screen, self.reported) => shown,
            typed = type_keys(input, self.keys, self.resizes) => typed.map(|()| None),
        }
    }
}

/// Show what `screen` brings, keeping what it says of the attach's session
/// in `reported`, until the attach exits, giving its code, or the
/// connection drops, giving none.
async fn show_screen(mut screen: Screen, reported: &mut Reported) -> Result<Option<i32>> {
    loop {
        match screen.next().await {
            Ok(Some(FromSession::Screen(bytes))) => show_bytes(&bytes)?,
            Ok(Some(FromSession::Session(session))) => reported.chosen = Some(session),
            Ok(Some(FromSession::Left(open))) => reported.left_open = open,
            Ok(Some(FromSession::Exited(code))) => return Ok(Some(code)),
            Ok(None) | Err(_) => return Ok(None),
        }
    }
}

/// Send the keys typed here and each new size of this terminal on
/// `input`, until the connection drops; fails once the terminal's input
/// ends.
async fn type_keys(
    mut input: Keys,
    keys: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    resizes: &mut Signal,
) -> Result<()> {
    // The size may have changed while the attach was asked for.
    let mut size = terminal_size();
    loop {
        let sent = match size.take() {
            Some(size) => input.send(&ToSession::Resize(size)).await,
            None => tokio::select! {
                keys = keys.recv() => match keys {
                    Some(keys) => input.send(&ToSession::Keys(keys)).await,
                    None => bail!("the terminal's input ended"),
                },
                _ = resizes.recv() => {
                    size = terminal_size();
                    continue;
                }
            },
        };
        if sent.is_err() {
            return Ok(());
        }
    }
}

/// The keys typed on this terminal, as it sends them. A thread reads them,
/// since a blocking read would hold the runtime open after the attach.
fn typed_keys() -> mpsc::UnboundedReceiver<Vec<u8>> {
    let (sender, receiver) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin().lock();
        let mut buffer = [0; 4096];
        while let Ok(read @ 1..) = stdin.read(&mut buffer) {
            if sender.send(buffer[..read].to_vec()).is_err() {
                return;
            }
        }
    });
    receiver
}

/// Wait `wait` before attaching again, failing once Ctrl-C is typed;
/// other keys go nowhere meanwhile.
async fn stop_on_ctrl_c(
    keys: &mut mpsc::UnboundedReceiver<Vec<u8>>,
    wait: Duration,
    machine: &str,
) -> Result<()> {
    tokio::select! {
        () = tokio::time::sleep(wait) => Ok(()),
        () = ctrl_c(keys) => bail!("stopped attaching again to machine {machine}"),
    }
}

/// Resolves once Ctrl-C is typed, or the terminal's input ends; the keys
/// typed before it go nowhere.
async fn ctrl_c(keys: &mut mpsc::UnboundedReceiver<Vec<u8>>) {
    while let Some(typed) = keys.recv().await {
        if typed.contains(&CTRL_C) {
            return;
        }
    }
}

/// Whether `error`, from asking to attach again, means the machine, the
/// hub or this daemon cannot be reached yet.
fn lost(error: &anyhow::Error) -> bool {
    connection::machine_unreachable(error)
        || connection::is_unreachable(error)
        || connection::stream_closed(error)
}

/// Say on its own line that the link to `machine` dropped while `session`
/// keeps running there.
/// The host's client there can no longer switch off the input modes it
/// turned on here, such as mouse tracking, so this terminal does.
fn lost_notice(machine: &str, session: &str) {
    show_bytes(
        format!(
            "{INPUT_MODES_RESET}\r\nshoal: lost the connection to machine {machine}; {session} keeps running there. Reconnecting, Ctrl-C to stop\r\n"
        )
        .as_bytes(),
    )
    .ok();
}

fn show_bytes(bytes: &[u8]) -> Result<()> {
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(bytes)?;
    Ok(stdout.flush()?)
}

/// This terminal's size; none off a terminal.
fn terminal_size() -> Option<TerminalSize> {
    let (rows, columns) = shoal_core::execution::terminal_size(std::io::stdout())?;
    Some(TerminalSize { rows, columns })
}

/// This machine's apps that show the sessions of a host's servers on
/// several machines, with the label each reports.
async fn viewer_apps() -> Vec<ViewerApp> {
    let mut apps = Vec::new();
    for host in crate::terminal::hosts() {
        if let Some(label) = host.viewer_app().await {
            apps.push(ViewerApp {
                host: host.name(),
                label,
            });
        }
    }
    apps
}

/// This terminal in raw mode, which it leaves however the attach ends,
/// switching off the emulator's input modes too unless the host's client
/// that turned them on left it.
struct RawTerminal {
    previous: libc::termios,
    left_by_client: bool,
}

impl RawTerminal {
    fn enter() -> Result<Self> {
        let mut settings = std::mem::MaybeUninit::uninit();
        // SAFETY: tcgetattr initializes settings on success.
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, settings.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error()).context("read the terminal's settings");
        }
        // SAFETY: tcgetattr succeeded.
        let previous = unsafe { settings.assume_init() };
        let mut raw = previous;
        // SAFETY: cfmakeraw and tcsetattr change only the settings given
        // and this terminal's.
        let entered = unsafe {
            libc::cfmakeraw(&mut raw);
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw)
        };
        if entered != 0 {
            return Err(std::io::Error::last_os_error()).context("enter raw mode");
        }
        Ok(Self {
            previous,
            left_by_client: false,
        })
    }

    /// The host's client left the terminal as it found it, as when the
    /// attach there ended.
    fn left_by_its_client(&mut self) {
        self.left_by_client = true;
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        if !self.left_by_client {
            let _ = show_bytes(INPUT_MODES_RESET.as_bytes());
        }
        // SAFETY: restores the settings read from this terminal.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.previous) };
    }
}
