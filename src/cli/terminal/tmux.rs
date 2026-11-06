//! tmux windows and panes: Shoal opens a pane per place, in a window of its
//! own or splitting the caller's pane, that runs its command and then a
//! shell, as a Herdr tab keeps one. The tmux server keeps panes running without a client, so
//! agents run in place in their pane and outlive the terminal that showed
//! them. A session is a group and a pane a place; tmux renders panes through
//! its own terminal emulator, so output reaches the user as tmux redraws it.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;

use super::{
    Demo, Host, PlaceState, Session, Start, Watched,
    host::{Demos, Places, Sessions, Types, Watches},
    typed::Typed,
};
use crate::{
    tools::ensure_program,
    config::repo::Placement,
    sessions::{
        HostName, Origin,
        tmux::{Opened, Shell, Tmux as Server, parse_tmux_var, var},
    },
    tools::Tool,
};

/// The variable naming the pane a process runs in.
const PANE_VAR: &str = "TMUX_PANE";
/// Lines of scrollback a demo's screen read returns.
const SCREEN_LINES: u32 = 500;

/// tmux serves the panes it runs Shoal in, where config opts in, and runs
/// windows for a terminal no host serves when it is the default host.
pub(super) struct Tmux;

impl Host for Tmux {
    fn name(&self) -> HostName {
        HostName::Tmux
    }

    /// A tmux pane.
    fn serves(&self) -> bool {
        in_tmux()
    }

    fn ensure_installed(&self) -> Result<()> {
        ensure_program("background launch", Tool::Tmux.program())
    }

    fn places(&self) -> Option<&dyn Places> {
        Some(self)
    }

    fn watches(&self) -> Option<&dyn Watches> {
        Some(self)
    }

    fn sessions(&self) -> Option<&dyn Sessions> {
        Some(self)
    }

    fn demos(&self) -> Option<&dyn Demos> {
        Some(self)
    }

    fn types(&self) -> Option<&dyn Types> {
        Some(self)
    }
}

impl Places for Tmux {
    fn group<'a>(
        &'a self,
        near: Option<&'a Origin>,
        label: &'a str,
        _cwd: &'a Path,
    ) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async move { session(near, label).await.map(Some) })
    }

    /// A pane in a window of its own, or splitting the caller's pane.
    fn open<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(open_pane(start))
    }

    /// A pane back at its shell prompt, as it is after the wrapper exits.
    fn run_in_place<'a>(
        &'a self,
        state: &'a Path,
        place: &'a Origin,
        argv: &'a [OsString],
        env: &'a [(&'a str, String)],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(run_at_prompt(state, place, argv, env))
    }

    /// Its window, current in its session and shown by this terminal's
    /// client; from a terminal outside tmux, by attaching a client to it.
    fn show<'a>(&'a self, place: &'a Origin, _name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(reveal(place))
    }

    fn background_places(&self) -> &'static str {
        "tmux windows"
    }

    fn background_place(&self, _name: &str) -> String {
        "in a background tmux window".into()
    }

    fn origin(&self) -> Option<Origin> {
        origin()
    }

    fn place_variables(&self) -> &'static [&'static str] {
        &[PANE_VAR]
    }

    /// The caller's, so a daemon started inside tmux does not send a start
    /// queued outside it to the daemon's server; a start queued from a pane
    /// reaches its origin's server through the socket the origin records.
    fn server_variables(&self) -> &'static [&'static str] {
        crate::sessions::tmux::SERVER_VARS
    }

    fn names_places(&self) -> bool {
        true
    }

    /// A place recorded without its server instance could name a later
    /// pane, so it keeps its name.
    fn rename_place<'a>(
        &'a self,
        place: &'a Origin,
        expected: Option<&'a str>,
        label: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let Ok(instance) = instance_of(place) else {
                return Ok(());
            };
            let server = Server::recorded(&place.data);
            rename_alone(&server, place.id(), instance, expected, label).await
        })
    }
}

/// A pane carries no ownership, so only the daemon's record closes it: tmux
/// watches its panes but closes none itself.
impl Watches for Tmux {
    /// tmux reads no agent state, so a pane is only open or gone. A pane
    /// under its ID on a later server is gone.
    fn place_state<'a>(&'a self, place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(async move {
            let pane = Server::here()
                .pane(&place.id, place.instance.as_deref())
                .await?;
            Ok(match pane {
                Some(_) => PlaceState::Open(None),
                None => PlaceState::Gone,
            })
        })
    }

    /// tmux numbers panes afresh when its server starts, so a pane is told
    /// apart by its server's process and start time.
    fn place_instance<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move {
            let pane = Server::here().pane(place, None).await?;
            Ok(pane.map(|pane| pane.instance))
        })
    }
}

/// Agents in tmux run in place in their pane.
impl Sessions for Tmux {
    /// The pane, under its window's name, while it is open on the server
    /// instance it was recorded on; none for a place recorded without one.
    fn session_at<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>> {
        Box::pin(async move {
            let Ok(instance) = instance_of(place) else {
                return Ok(None);
            };
            let pane = Server::recorded(&place.data)
                .pane(place.id(), Some(instance))
                .await?;
            Ok(pane.map(|pane| Session::open(pane.window_name, pane.clients, place)))
        })
    }
}

impl Demos for Tmux {
    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(start_demo(demo))
    }

    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            Server::recorded(&place.data)
                .capture(place.id(), instance_of(place)?, SCREEN_LINES)
                .await
        })
    }
}

impl Types for Tmux {
    /// A pane showing a mode, such as copy mode, leaves it first, so the
    /// keys reach its program rather than the mode.
    fn send_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let server = Server::recorded(&place.data);
            let pane = server
                .pane(place.id(), Some(instance_of(place)?))
                .await?
                .context("the tmux pane is gone")?;
            if pane.in_mode {
                server.leave_mode(&pane).await?;
            }
            server.send(&pane.id, &pane.instance, keys).await
        })
    }
}

/// What tmux's section sets: whether it serves tmux panes (`tmux.enabled`,
/// unset following whether `default_host` names tmux), whether it opens
/// places (`tmux.new_window`) and splits the caller's pane for them
/// (`tmux.placement`), their names (`tmux.window_name`), the session for a
/// terminal outside tmux (`tmux.session_name`), and `tmux.close_when_done`,
/// which overrides the top-level option.
pub(super) fn options(config: &crate::config::Tmux, default_host: HostName) -> super::Options {
    super::Options {
        enabled: config.enabled.unwrap_or(default_host == HostName::Tmux),
        opens_places: config.new_window,
        opens_splits: config.placement == Placement::Split,
        name_template: config.window_name.clone(),
        group_template: Some(config.session_name.clone()),
        close_when_done: config.close_when_done,
    }
}

/// Whether this process runs in a tmux pane.
fn in_tmux() -> bool {
    var("TMUX").is_some() && var(PANE_VAR).is_some()
}

/// The pane this process runs in, in the session it started in, with the
/// server's socket.
fn origin() -> Option<Origin> {
    let pane = var(PANE_VAR)?;
    let tmux = var("TMUX")?;
    let server = parse_tmux_var(&tmux)?;
    Some(Origin {
        host: HostName::Tmux,
        group: Some(server.session),
        place: Some(pane),
        data: Server::data(server.socket),
        owned: false,
        instance: None,
    })
}

/// The server of `origin`, or the one this process reaches.
fn server_of(origin: Option<&Origin>) -> Server {
    origin.map_or_else(Server::here, |origin| Server::recorded(&origin.data))
}

/// The session named `label` on the server of `near`, created when there
/// is none. Its first window, a shell Shoal does not record, starts in the
/// home directory, so it holds no workspace from cleanup.
async fn session(near: Option<&Origin>, label: &str) -> Result<Origin> {
    let server = server_of(near);
    server.ensure_supported().await?;
    let group = server.session(label, &crate::fsutil::home_dir()?).await?;
    Ok(Origin {
        host: HostName::Tmux,
        group: Some(group.session),
        place: None,
        data: Server::data(&group.socket),
        owned: false,
        instance: None,
    })
}

/// Open a pane for `start` in its origin's session, in a window of its own
/// or splitting the caller's pane, running its command and then the user's
/// shell, which follows the shell directive once at its prompt.
async fn open_pane(start: &Start<'_>) -> Result<Origin> {
    let origin = start
        .origin
        .context("Shoal has no tmux session to open a window in")?;
    let typed = typed(start.state, start.argv, start.env, &[], false)?;
    let place = open_place(
        origin,
        &Shell {
            session: session_of(origin)?,
            label: start.label,
            cwd: start.cwd,
            focus: start.focus,
            command: typed.command(),
            beside: start.split,
        },
    )
    .await?;
    typed.hand_over()?;
    Ok(place)
}

/// The session `origin` names.
fn session_of(origin: &Origin) -> Result<&str> {
    origin
        .group
        .as_deref()
        .context("the tmux origin names no session")
}

/// Open `shell` on the server of `origin`, as a place in its group.
async fn open_place(origin: &Origin, shell: &Shell<'_>) -> Result<Origin> {
    let server = Server::recorded(&origin.data);
    server.ensure_supported().await?;
    Ok(opened_place(origin, server.open(shell).await?))
}

/// The place `opened` is, in the group of `origin`.
fn opened_place(origin: &Origin, opened: Opened) -> Origin {
    Origin {
        place: Some(opened.pane),
        data: Server::data(&opened.socket),
        owned: true,
        instance: Some(opened.instance),
        ..origin.clone()
    }
}

/// The launch step for `argv` with `env`, run without the variables naming
/// other hosts' places and `clear`, which the server's environment may
/// carry into the pane.
fn typed(
    state: &Path,
    argv: &[OsString],
    env: &[(&str, String)],
    clear: &[&str],
    ends_shell: bool,
) -> Result<Typed> {
    let mut cleared = super::hosts::foreign_place_variables(HostName::Tmux);
    cleared.extend(clear);
    Typed::clearing(state, argv, env, &cleared, ends_shell)
}

/// The server instance `place` records, without which its pane ID could
/// name a later pane.
fn instance_of(place: &Origin) -> Result<&str> {
    place
        .instance
        .as_deref()
        .context("the tmux pane records no server instance")
}

/// Type `argv` with `env` into the pane of `place` once its shell is back
/// at its prompt. A pane that is gone, under its ID on a later server, or
/// recorded without its server, is not.
async fn run_at_prompt(
    state: &Path,
    place: &Origin,
    argv: &[OsString],
    env: &[(&str, String)],
) -> Result<bool> {
    let Ok(instance) = instance_of(place) else {
        return Ok(false);
    };
    let server = Server::recorded(&place.data);
    let Some(pane) = server.pane(place.id(), Some(instance)).await? else {
        return Ok(false);
    };
    if pane.in_mode || !server.at_prompt(&pane).await {
        return Ok(false);
    }
    let typed = typed(state, argv, env, &[], false)?;
    server.send(&pane.id, &pane.instance, typed.text()).await?;
    typed.hand_over()?;
    Ok(true)
}

/// Open a background window for the demo, then run its worker, which needs
/// the pane it runs in, in place of the window's shell; the window closes
/// with the demo.
async fn start_demo(demo: &Demo<'_>) -> Result<Origin> {
    let origin = demo
        .origin
        .context("Shoal has no tmux session to open a window in")?;
    let shell = Shell {
        session: session_of(origin)?,
        label: demo.label,
        cwd: demo.cwd,
        focus: false,
        command: &[],
        beside: None,
    };
    let place = open_place(origin, &shell).await?;
    let worker = (demo.worker)(&place)?;
    let typed = typed(
        demo.state,
        &worker,
        demo.env,
        super::demos::NOT_INHERITED_BY_DEMOS,
        true,
    )?;
    Server::recorded(&place.data)
        .respawn(place.id(), instance_of(&place)?, typed.command())
        .await?;
    typed.hand_over()?;
    Ok(place)
}

/// Bring the pane at `place` before the user: current in its window and
/// session, then shown by this terminal's tmux client when it runs on the
/// same server, or by a client this terminal attaches from outside tmux.
async fn reveal(place: &Origin) -> Result<()> {
    let server = Server::recorded(&place.data);
    let pane = server
        .pane(place.id(), Some(instance_of(place)?))
        .await?
        .context("the tmux pane is gone")?;
    server.select(&pane).await?;
    if var("TMUX").is_none() {
        return server.attach(&pane.id).await;
    }
    if Server::here() != server {
        bail!("the tmux window runs on another tmux server; attach from outside tmux");
    }
    server.switch_client(&pane.id).await
}

/// Name the window of `pane` `label` while the pane is alone there, and,
/// with `expected`, while the window shows that; a pane beside others
/// leaves the user's window its name, and a pane that is gone, or under its
/// ID on a server instance other than `instance`, leaves nothing to rename.
async fn rename_alone(
    server: &Server,
    pane: &str,
    instance: &str,
    expected: Option<&str>,
    label: &str,
) -> Result<()> {
    let Some(pane) = server.pane(pane, Some(instance)).await? else {
        return Ok(());
    };
    if pane.window_panes != 1 || expected.is_some_and(|expected| pane.window_name != expected) {
        return Ok(());
    }
    server.rename_window(&pane, label).await
}

#[cfg(test)]
mod tests;
