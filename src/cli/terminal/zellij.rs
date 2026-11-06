//! Zellij tabs and panes: Shoal opens a pane per place, in a tab of its own
//! or splitting the caller's pane, that runs its command and then a shell,
//! as a Herdr tab keeps one. Zellij drops tabs opened in a session no client
//! shows, so a place for such a session, or for a terminal outside Zellij,
//! opens as the first pane of a session of its own. Each session's server
//! keeps its panes running without a client, so agents run in place in their
//! pane and outlive the terminal that showed them. A session is a group and a
//! pane a place; Zellij renders panes through its own terminal emulator, so
//! output reaches the user as Zellij redraws it.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, ensure};
use futures_util::future::BoxFuture;

use super::{
    Demo, Host, PlaceState, Session, Start, Watched,
    host::{Demos, Places, Sessions, Types, Watches},
    typed::{self, Typed},
};
use crate::{
    cli::agents::ensure_program,
    config::repo::TabPlacement,
    sessions::{
        HostName, Origin,
        zellij::{
            Opened, PANE_VAR, PLACE_VARS, PaneRef, SERVER_VARS, SESSION_VAR, Shell,
            Zellij as Server, pane_of_number, parse_place, place_id, var,
        },
    },
    tools::Tool,
};

/// Lines of scrollback a demo's screen read returns.
const SCREEN_LINES: usize = 500;

/// Zellij serves the panes it runs Shoal in, where config opts in, and runs
/// sessions for a terminal no host serves when it is the default host.
pub(super) struct Zellij;

impl Host for Zellij {
    fn name(&self) -> HostName {
        HostName::Zellij
    }

    /// A Zellij pane.
    fn serves(&self) -> bool {
        var(SESSION_VAR).is_some() && var(PANE_VAR).is_some()
    }

    fn ensure_installed(&self) -> Result<()> {
        ensure_program("background launch", Tool::Zellij.program())
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

impl Places for Zellij {
    /// A pane in a tab of its own or splitting the caller's pane, in a
    /// session a client shows; otherwise a session of its own.
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

    /// Its tab, current in its session and shown by this terminal's
    /// client; from a terminal outside Zellij, by attaching a client to it.
    fn show<'a>(&'a self, place: &'a Origin, _name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(reveal(place))
    }

    fn background_places(&self) -> &'static str {
        "Zellij tabs"
    }

    fn background_place(&self, name: &str) -> String {
        format!("in the background in Zellij; attach with `shoal attach {name}`")
    }

    fn origin(&self) -> Option<Origin> {
        origin()
    }

    fn place_variables(&self) -> &'static [&'static str] {
        PLACE_VARS
    }

    /// The caller's, so a start queued from a terminal outside Zellij
    /// reaches the sessions it would.
    fn server_variables(&self) -> &'static [&'static str] {
        SERVER_VARS
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
            rename_alone(server, place.id(), instance, expected, label).await
        })
    }
}

/// A pane carries no ownership, so only the daemon's record closes it:
/// Zellij watches its panes but closes none itself.
impl Watches for Zellij {
    /// Zellij reads no agent state, so a pane is only open or gone. A pane
    /// under its ID on a later server is gone.
    fn place_state<'a>(&'a self, place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(async move {
            let Some(pane) = parse_place(&place.id) else {
                return Ok(PlaceState::Gone);
            };
            let server = Server::here().reaching(pane.session).await?;
            let pane = server.pane(pane, place.instance.as_deref()).await?;
            Ok(match pane {
                Some(_) => PlaceState::Open(None),
                None => PlaceState::Gone,
            })
        })
    }

    /// Zellij numbers panes afresh when a session's server starts, so a pane
    /// is told apart by its server's process and birth.
    fn place_instance<'a>(&'a self, place: &'a str) -> BoxFuture<'a, Result<Option<String>>> {
        Box::pin(async move {
            let pane = parse_place(place).context("invalid Zellij place")?;
            let server = Server::here().instance(pane.session).await?;
            Ok(server.map(|(instance, _)| instance))
        })
    }
}

/// Agents in Zellij run in place in their pane.
impl Sessions for Zellij {
    /// The pane, under its tab's name, while it is open on the server
    /// instance it was recorded on; none for a place recorded without one.
    fn session_at<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>> {
        Box::pin(async move {
            let (Ok(instance), Ok((server, pane))) = (instance_of(place), reach(place).await)
            else {
                return Ok(None);
            };
            let Some(open) = server.pane(pane, Some(instance)).await? else {
                return Ok(None);
            };
            let clients = clients_on_tab(&server, pane.session, open.tab).await?;
            Ok(Some(Session::open(open.tab_name, clients, place)))
        })
    }
}

impl Demos for Zellij {
    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(start_demo(demo))
    }

    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move {
            let (server, pane) = reach(place).await?;
            server
                .capture(pane, instance_of(place)?, SCREEN_LINES)
                .await
        })
    }
}

impl Types for Zellij {
    fn send_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let (server, pane) = reach(place).await?;
            server.write(pane, instance_of(place)?, keys).await
        })
    }
}

/// What Zellij's section sets: whether it serves Zellij panes
/// (`zellij.enabled`, unset following whether `default_host` names Zellij),
/// whether it opens places (`zellij.new_tab`) and splits the caller's pane
/// for them (`zellij.placement`), their names (`zellij.tab_name`), and
/// `zellij.close_when_done`, which overrides the top-level option. Zellij
/// keeps no configured group: a place outside a session a client shows gets
/// a session of its own.
pub(super) fn options(config: &crate::config::Zellij, default_host: HostName) -> super::Options {
    super::Options {
        enabled: config.enabled.unwrap_or(default_host == HostName::Zellij),
        opens_places: config.new_tab,
        opens_splits: config.placement == TabPlacement::Split,
        name_template: config.tab_name.clone(),
        group_template: None,
        close_when_done: config.close_when_done,
    }
}

/// The pane this process runs in, in its session, with the socket
/// directory the caller chose.
fn origin() -> Option<Origin> {
    let session = var(SESSION_VAR)?;
    let pane = pane_of_number(&var(PANE_VAR)?)?;
    let data = Server::here()
        .socket_dir()
        .map(Server::data)
        .unwrap_or_default();
    Some(Origin {
        host: HostName::Zellij,
        place: Some(place_id(&session, &pane)),
        group: Some(session),
        data,
        owned: false,
        instance: None,
    })
}

/// The sessions of `origin`'s session, or the ones this process reaches.
async fn server_for(origin: Option<&Origin>) -> Result<Server> {
    let Some(origin) = origin else {
        return Ok(Server::here());
    };
    let server = Server::recorded(&origin.data);
    match &origin.group {
        Some(session) => server.reaching(session).await,
        None => Ok(server),
    }
}

/// The sessions holding the pane `place` names, by the socket directory
/// its record names or where its session's server listens, and the pane.
async fn reach(place: &Origin) -> Result<(Server, PaneRef<'_>)> {
    let pane = pane_of(place)?;
    let server = Server::recorded(&place.data).reaching(pane.session).await?;
    Ok((server, pane))
}

/// The pane `place` names.
fn pane_of(place: &Origin) -> Result<PaneRef<'_>> {
    parse_place(place.id()).with_context(|| format!("invalid Zellij place {}", place.id()))
}

/// The server instance `place` records, without which its pane ID could
/// name a later pane.
fn instance_of(place: &Origin) -> Result<&str> {
    place
        .instance
        .as_deref()
        .context("the Zellij pane records no server instance")
}

/// Open a pane for `start`, running its command and then the user's
/// shell, which follows the shell directive once at its prompt.
async fn open_pane(start: &Start<'_>) -> Result<Origin> {
    let typed = typed(start.state, start.argv, start.env, &[], false)?;
    let shell = Shell {
        label: start.label,
        cwd: start.cwd,
        focus: start.focus,
        command: typed.command(),
    };
    let place = open_place(start.origin, start.split, &shell).await?;
    typed.hand_over()?;
    Ok(place)
}

/// Open `shell` in the session of `origin` while a client shows it,
/// splitting `split` there when given and otherwise in a tab of its own;
/// without such a session, in a session of its own.
async fn open_place(
    origin: Option<&Origin>,
    split: Option<&str>,
    shell: &Shell<'_>,
) -> Result<Origin> {
    let server = server_for(origin).await?;
    server.ensure_supported().await?;
    let opened = match shown_session(&server, origin).await? {
        Some(session) => match split.and_then(parse_place) {
            Some(beside) => server.open_split(beside, shell).await?,
            None => server.open_tab(session, shell).await?,
        },
        None => server.open_session(shell).await?,
    };
    Ok(opened_place(opened))
}

/// The session `origin` names while a client shows it.
async fn shown_session<'a>(server: &Server, origin: Option<&'a Origin>) -> Result<Option<&'a str>> {
    let Some(session) = origin.and_then(|origin| origin.group.as_deref()) else {
        return Ok(None);
    };
    Ok(server.shown(session).await?.then_some(session))
}

/// The place `opened` is, owned by Shoal, in its session.
fn opened_place(opened: Opened) -> Origin {
    Origin {
        host: HostName::Zellij,
        place: Some(place_id(&opened.session, &opened.pane)),
        data: Server::data(&opened.socket_dir),
        group: Some(opened.session),
        owned: true,
        instance: Some(opened.instance),
    }
}

/// The launch step for `argv` with `env`, run without the variables naming
/// other hosts' places and `clear`, which the session's environment may
/// carry into the pane.
fn typed(
    state: &Path,
    argv: &[OsString],
    env: &[(&str, String)],
    clear: &[&str],
    ends_shell: bool,
) -> Result<Typed> {
    let mut cleared = super::hosts::foreign_place_variables(HostName::Zellij);
    cleared.extend(clear);
    Typed::clearing(state, argv, env, &cleared, ends_shell)
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
    let (Ok(instance), Ok((server, pane))) = (instance_of(place), reach(place).await) else {
        return Ok(false);
    };
    let Some(open) = server.pane(pane, Some(instance)).await? else {
        return Ok(false);
    };
    if !at_prompt(open.exited, open.command.as_deref()) {
        return Ok(false);
    }
    let typed = typed(state, argv, env, &[], false)?;
    server.write(pane, instance, typed.text()).await?;
    typed.hand_over()?;
    Ok(true)
}

/// Whether a pane that runs `command` in its foreground, and has not
/// `exited`, is at the prompt of the user's shell: Zellij names the process
/// in the foreground but not its process, so the shell is told by its name,
/// run with no arguments but those of an interactive or login shell. A shell
/// running a script or a command is busy.
fn at_prompt(exited: bool, command: Option<&str>) -> bool {
    const PROMPT_FLAGS: &[&str] = &["-l", "--login", "-i"];
    let shell = var("SHELL").unwrap_or_else(|| "/bin/sh".into());
    let program = |word: &str| -> Option<String> {
        let name = Path::new(word.trim_start_matches('-')).file_name()?;
        Some(name.to_string_lossy().into_owned())
    };
    let Some(mut words) = command.map(str::split_whitespace) else {
        return false;
    };
    !exited
        && words
            .next()
            .and_then(program)
            .is_some_and(|name| Some(name) == program(&shell))
        && words.all(|word| PROMPT_FLAGS.contains(&word))
}

/// Open a background pane for the demo with the user's shell, then type its
/// worker, which needs the pane it runs in, at the shell's prompt; the pane
/// closes with the demo.
async fn start_demo(demo: &Demo<'_>) -> Result<Origin> {
    let shell = Shell {
        label: demo.label,
        cwd: demo.cwd,
        focus: false,
        command: &[],
    };
    let place = open_place(demo.origin, None, &shell).await?;
    let worker = (demo.worker)(&place)?;
    let typed = typed(
        demo.state,
        &worker,
        demo.env,
        super::demos::NOT_INHERITED_BY_DEMOS,
        true,
    )?;
    let server = Server::recorded(&place.data);
    let (pane, instance) = (pane_of(&place)?, instance_of(&place)?);
    await_prompt(&server, pane, instance).await;
    server.write(pane, instance, typed.text()).await?;
    typed.hand_over()?;
    Ok(place)
}

/// Wait until the shell of `pane` draws its prompt: Zellij shows no
/// terminal mode, so the first text on the pane's screen stands for it.
async fn await_prompt(server: &Server, pane: PaneRef<'_>, instance: &str) {
    typed::await_prompt(|| async {
        let screen = server.capture(pane, instance, 1).await.ok()?;
        Some(screen.trim().is_empty())
    })
    .await;
}

/// Bring the pane at `place` before the user: current in its tab and
/// session, then shown by this terminal's Zellij client, switching it to
/// the pane's session, or by a client this terminal attaches from outside
/// Zellij. A client switches only between sessions of one socket
/// directory, so a pane in another is refused inside Zellij.
async fn reveal(place: &Origin) -> Result<()> {
    let (server, pane) = reach(place).await?;
    let open = server
        .pane(pane, Some(instance_of(place)?))
        .await?
        .context("the Zellij pane is gone")?;
    let Some(here) = var(SESSION_VAR) else {
        server.select(pane.session, &open).await?;
        return server.attach(pane.session).await;
    };
    let caller = Server::here().reaching(&here).await?;
    ensure!(
        caller.same_sessions(&server),
        "the Zellij pane runs in another socket directory; attach from outside Zellij"
    );
    server.select(pane.session, &open).await?;
    if here == pane.session {
        return Ok(());
    }
    caller.switch_client(&here, pane.session, &open.id).await
}

/// How many clients attached to `session` show its tab `tab`.
async fn clients_on_tab(server: &Server, session: &str, tab: u64) -> Result<u32> {
    let clients = server.clients(session).await?;
    let panes = server.panes(session).await?;
    let shown = clients
        .iter()
        .filter(|client| {
            panes
                .iter()
                .any(|pane| pane.id == **client && pane.tab == tab)
        })
        .count();
    Ok(u32::try_from(shown).unwrap_or(u32::MAX))
}

/// Name the tab of `place` `label` while its pane is alone there, and,
/// with `expected`, while the tab shows that; a pane beside others leaves
/// the user's tab its name, and a pane that is gone, or under its ID on a
/// server instance other than `instance`, leaves nothing to rename.
async fn rename_alone(
    server: Server,
    place: &str,
    instance: &str,
    expected: Option<&str>,
    label: &str,
) -> Result<()> {
    let pane = parse_place(place).context("invalid Zellij place")?;
    let server = server.reaching(pane.session).await?;
    let Some(open) = server.pane(pane, Some(instance)).await? else {
        return Ok(());
    };
    let panes = server.panes(pane.session).await?;
    let alone = panes.iter().filter(|other| other.tab == open.tab).count() == 1;
    if !alone || expected.is_some_and(|expected| open.tab_name != expected) {
        return Ok(());
    }
    server.rename_tab(pane.session, &open, label).await
}

#[cfg(test)]
mod tests;
