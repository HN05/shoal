//! `shoal attach` and terminal `shoal demo` on another joined machine. That
//! machine shows the session or demo on a terminal of its own, which this one
//! shows in raw mode, sending every key and each new size, so the host's own
//! detach key detaches. After a dropped link it attaches again once the machine
//! answers, until Ctrl-C.
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
        connection::{self, CommandPaths},
    },
};

/// The first wait before attaching again after a dropped link, which
/// doubles up to [`LONGEST_WAIT`].
const FIRST_WAIT: Duration = Duration::from_secs(1);
const LONGEST_WAIT: Duration = Duration::from_secs(30);

const CTRL_C: u8 = 0x03;

pub(in crate::cli::commands) enum Viewed {
    /// A session of a space, as an attach there chooses it.
    Session { space: SpaceId, viewer: Viewer },
    /// A space's terminal demo.
    Demo {
        space: SpaceId,
        name: String,
        terminal: Terminal,
    },
}

impl Viewed {
    pub(in crate::cli::commands) async fn session(space: SpaceId, session: Option<String>) -> Self {
        let viewer = Viewer {
            session,
            again: None,
            terminal: this_terminal().await,
        };
        Self::Session { space, viewer }
    }

    pub(in crate::cli::commands) async fn demo(space: SpaceId, name: String) -> Self {
        let terminal = this_terminal().await;
        Self::Demo {
            space,
            name,
            terminal,
        }
    }

    async fn open(&self, paths: &CommandPaths) -> Result<(Screen, Keys)> {
        match self {
            Self::Session { space, viewer } => {
                let space = SpaceSelector::id(space);
                crate::client::attaching::attach(paths, space, viewer.clone()).await
            }
            Self::Demo {
                space,
                name,
                terminal,
            } => {
                let space = SpaceSelector::id(space);
                let (name, terminal) = (name.clone(), terminal.clone());
                let stream = crate::demos::open_terminal(paths, space, name, terminal).await?;
                Ok((stream.screen, stream.keys))
            }
        }
    }

    /// It neither wakes nor restores an agent a second time.
    fn again(&mut self, reported: &Reported, machine: &str) -> Result<String> {
        match self {
            Self::Session { viewer, .. } => {
                let Some(session) = &reported.chosen else {
                    bail!("lost the connection to machine {machine} before it chose a session");
                };
                viewer.again = Some(session.place.clone());
                Ok(session.name.clone())
            }
            Self::Demo { name, .. } => Ok(format!("demo {name}")),
        }
    }
}

pub(in crate::cli::commands) struct Ended {
    /// The attach's exit code, which this one exits with.
    pub code: i32,
    /// Whether it left its session running there.
    pub open: bool,
}

/// A signal that ends this process, such as SIGTERM, ends the attach, leaving
/// the terminal as it found it.
pub(in crate::cli::commands) async fn view(
    ctx: &Context,
    viewed: Viewed,
    machine: &str,
) -> Result<Ended> {
    let mut ended = Termination::watch()?;
    let mut raw = RawTerminal::enter()?;
    let attached = tokio::select! {
        attached = keep_attached(ctx, viewed, machine) => attached?,
        signal = ended.signalled() => bail!("stopped by {signal}"),
    };
    raw.left_by_its_client();
    Ok(attached)
}

async fn keep_attached(ctx: &Context, mut viewed: Viewed, machine: &str) -> Result<Ended> {
    let mut keys = typed_keys();
    let mut resizes = signal(SignalKind::window_change()).context("watch the terminal's size")?;
    let mut reported = Reported::default();
    let mut wait = FIRST_WAIT;
    let mut again = false;
    loop {
        // The terminal is raw, so Ctrl-C is a key, which stops a request
        // whose answer does not come.
        let attached = tokio::select! {
            attached = viewed.open(&ctx.paths) => attached,
            () = ctrl_c(&mut keys) => bail!("stopped attaching to machine {machine}"),
        };
        let (screen, input) = match attached {
            Ok(attached) => attached,
            Err(error) if again && lost(&error) => {
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
        lost_notice(machine, &viewed.again(&reported, machine)?);
        again = true;
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

struct Shown<'a> {
    keys: &'a mut mpsc::UnboundedReceiver<Vec<u8>>,
    resizes: &'a mut Signal,
    reported: &'a mut Reported,
}

#[derive(Default)]
struct Reported {
    /// The session it chose, once it says.
    chosen: Option<ChosenSession>,
    /// Whether it left that session running, once it says so.
    left_open: bool,
}

impl Shown<'_> {
    /// Keys go on apart from the screen, so a paste the other machine takes
    /// slowly never stops what this terminal shows.
    async fn show(self, screen: Screen, input: Keys) -> Result<Option<i32>> {
        tokio::select! {
            shown = show_screen(screen, self.reported) => shown,
            typed = type_keys(input, self.keys, self.resizes) => typed.map(|()| None),
        }
    }
}

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

fn lost(error: &anyhow::Error) -> bool {
    connection::machine_unreachable(error)
        || connection::is_unreachable(error)
        || connection::stream_closed(error)
}

/// The host's client there can no longer switch off the input modes it turned
/// on here, such as mouse tracking, so this terminal does.
fn lost_notice(machine: &str, running: &str) {
    show_bytes(
        format!(
            "{INPUT_MODES_RESET}\r\nshoal: lost the connection to machine {machine}; {running} keeps running there. Reconnecting, Ctrl-C to stop\r\n"
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

async fn this_terminal() -> Terminal {
    Terminal {
        size: terminal_size(),
        term: std::env::var("TERM").ok(),
        apps: viewer_apps().await,
    }
}

fn terminal_size() -> Option<TerminalSize> {
    let (rows, columns) = shoal_core::execution::terminal_size(std::io::stdout())?;
    Some(TerminalSize { rows, columns })
}

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
