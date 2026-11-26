//! Rex as a terminal host: a block per place, in a window of its own or
//! splitting the caller's block. Rex sets no environment for a new terminal,
//! so the block runs Shoal's launch step and then the user's shell.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result, bail};
use futures_util::future::BoxFuture;

use super::{
    Demo, Elsewhere, Host, PlaceState, Session, Start, Watched,
    host::{Demos, Places, Sessions, Types, Watches},
    typed::{self, Typed},
};
use crate::{
    config::repo::Placement,
    sessions::{
        HostName, Origin,
        rex::{Block, Rex as Server, Shell},
    },
};

/// Rex serves the blocks it runs Shoal in, and runs windows for a terminal
/// no host serves when it is the default host.
pub(super) struct Rex;

impl Host for Rex {
    fn name(&self) -> HostName {
        HostName::Rex
    }

    /// A Rex block, which names its session and itself.
    fn serves(&self) -> bool {
        in_rex()
    }

    fn ensure_installed(&self) -> Result<()> {
        Server::here().map(drop)
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

    fn viewer_app(&self) -> BoxFuture<'_, Option<String>> {
        Box::pin(async {
            let server = Server::here().ok()?;
            server.mac_app_label().await.ok().flatten()
        })
    }

    fn types(&self) -> Option<&dyn Types> {
        Some(self)
    }
}

impl Places for Rex {
    fn group<'a>(
        &'a self,
        near: Option<&'a Origin>,
        label: &'a str,
        cwd: &'a Path,
    ) -> BoxFuture<'a, Result<Option<Origin>>> {
        Box::pin(async move { session(near, label, cwd).await.map(Some) })
    }

    /// A block in a window of its own, or splitting the caller's block.
    fn open<'a>(&'a self, start: &'a Start<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(open_block(start))
    }

    /// A block back at its shell prompt, as it is after the wrapper exits.
    fn run_in_place<'a>(
        &'a self,
        state: &'a Path,
        place: &'a Origin,
        argv: &'a [OsString],
        env: &'a [(&'a str, String)],
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(run_at_prompt(state, place, argv, env))
    }

    fn show<'a>(&'a self, place: &'a Origin, name: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let server = Server::recorded(&place.data)?;
            let block = server
                .block(place.id())
                .await?
                .context("the Rex block is gone")?;
            reveal(&server, &block, place.id(), name).await
        })
    }

    fn show_elsewhere<'a>(
        &'a self,
        place: &'a Origin,
        name: &'a str,
        viewer: &'a Elsewhere<'a>,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(reveal_elsewhere(place, name, viewer))
    }

    fn background_places(&self) -> &'static str {
        "Rex blocks"
    }

    fn background_place(&self, _name: &str) -> String {
        "in a background Rex block".into()
    }

    fn origin(&self) -> Option<Origin> {
        origin()
    }

    /// Every block is in a session, which Shoal reads from the block.
    fn in_group(&self, origin: &Origin) -> bool {
        origin.place.is_some() || origin.group.is_some()
    }

    fn place_variables(&self) -> &'static [&'static str] {
        &["REX_BLOCK"]
    }

    fn server_variables(&self) -> &'static [&'static str] {
        &["REX_SERVER", "REX_BIN_DIR"]
    }

    fn names_places(&self) -> bool {
        true
    }

    fn rename_place<'a>(
        &'a self,
        place: &'a Origin,
        expected: Option<&'a str>,
        label: &'a str,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(rename_block(place, expected, label))
    }
}

/// A block carries no ownership, so only the daemon's record closes it: Rex
/// watches its blocks but closes none itself.
impl Watches for Rex {
    /// Rex reads no agent state, so a block is only open or gone.
    fn place_state<'a>(&'a self, place: &'a Watched) -> BoxFuture<'a, Result<PlaceState>> {
        Box::pin(async move {
            Ok(match Server::here()?.block(&place.id).await? {
                Some(_) => PlaceState::Open(None),
                None => PlaceState::Gone,
            })
        })
    }
}

/// Agents in Rex run in place in their block.
impl Sessions for Rex {
    /// The block, under the name it shows, while it is open.
    fn session_at<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<Option<Session>>> {
        Box::pin(session_at(place))
    }
}

impl Demos for Rex {
    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(start_demo(demo))
    }

    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Server::recorded(&place.data)?.capture(place.id()).await })
    }
}

impl Types for Rex {
    fn send_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Server::recorded(&place.data)?.send(place.id(), keys).await })
    }
}

pub(super) fn options(config: &crate::config::Rex) -> super::Options {
    super::Options {
        enabled: config.enabled,
        opens_places: config.new_window,
        opens_splits: config.placement == Placement::Split,
        name_template: config.window_name.clone(),
        group_template: Some(config.session_name.clone()),
        close_when_done: config.close_when_done,
        nested_zmx: config.nested_zmx,
    }
}

/// The value of variable `name`, when set and not empty.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn in_rex() -> bool {
    var("REX_BLOCK").is_some()
}

/// The block this process runs in, with how to reach its server. Rex names
/// the block alone, which keeps its ID when it moves to another session, so
/// a block's origin names no session: Shoal reads it from the block.
fn origin() -> Option<Origin> {
    if !in_rex() {
        return None;
    }
    let server = Server::here().ok()?;
    Some(Origin {
        host: HostName::Rex,
        group: None,
        place: var("REX_BLOCK"),
        data: server.data(),
        owned: false,
        instance: None,
        nested: None,
    })
}

/// The session to open a block in beside `origin`: the one its block is in
/// now, or else the one it names.
async fn session_of(server: &Server, origin: &Origin) -> Result<String> {
    match &origin.place {
        Some(block) => Ok(server
            .block(block)
            .await?
            .context("the Rex block is gone")?
            .session),
        None => origin
            .group
            .clone()
            .context("the Rex origin names no session"),
    }
}

/// The origin of `block`, which Shoal opened beside `origin`, named as a
/// process in it names its own.
fn opened(origin: &Origin, block: String) -> Origin {
    Origin {
        group: None,
        place: Some(block),
        owned: true,
        ..origin.clone()
    }
}

fn server_of(origin: Option<&Origin>) -> Result<Server> {
    match origin {
        Some(origin) => Server::recorded(&origin.data),
        None => Server::here(),
    }
}

/// The session labelled `label` on the server of `near`, created at `cwd`
/// when there is none.
async fn session(near: Option<&Origin>, label: &str, cwd: &Path) -> Result<Origin> {
    let server = server_of(near)?;
    server.ensure_supported().await?;
    let session = server.session(label, cwd).await?;
    Ok(Origin {
        host: HostName::Rex,
        group: Some(session),
        place: None,
        data: server.data(),
        owned: false,
        instance: None,
        nested: None,
    })
}

/// Open a block for `start` in its origin's session, in a window of its own
/// or splitting the caller's block, running its command and then a shell,
/// which follows the shell directive once at its prompt.
async fn open_block(start: &Start<'_>) -> Result<Origin> {
    let origin = start
        .origin
        .context("Shoal has no Rex session to open a window in")?;
    let server = Server::recorded(&origin.data)?;
    server.ensure_supported().await?;
    let session = session_of(&server, origin).await?;
    let typed = Typed::new(start.state, start.argv, start.env)?;
    let block = server
        .open(&Shell {
            session: &session,
            label: start.label,
            cwd: start.cwd,
            focus: start.focus,
            command: Some(typed.command()),
            beside: start.split,
        })
        .await?;
    typed.hand_over()?;
    Ok(opened(origin, block.block))
}

/// Bring `id`, `name`'s block, forward: in Rex, its window becomes the
/// active one where this terminal shows it; elsewhere the Mac app selects
/// it, since Rex has no terminal client to attach.
async fn reveal(server: &Server, block: &Block, id: &str, name: &str) -> Result<()> {
    server.focus(&block.session, id).await?;
    if in_rex() {
        return Ok(());
    }
    if server.show_in_app(block).await? {
        outln!("Opened {name} in the Rex app");
    } else {
        outln!("Opened {name} in a Rex window; open Rex to see it");
    }
    Ok(())
}

/// Bring `place`, `name`'s block, forward in the Rex app of a user on
/// another machine, which connects to this machine's server as a client
/// of its own: refused where that user has no Rex app, or one that does not
/// connect here, naming the address to add.
async fn reveal_elsewhere(place: &Origin, name: &str, viewer: &Elsewhere<'_>) -> Result<()> {
    let machine = viewer.machine;
    let Some(label) = viewer.app else {
        bail!(
            "{name} runs in a Rex block on machine {machine}; open Rex on your machine, connected to that machine's Rex, to see it"
        );
    };
    let server = Server::recorded(&place.data)?;
    let block = (server.block(place.id()).await?).context("the Rex block is gone")?;
    server.focus(&block.session, place.id()).await?;
    if server.show_in_app_labelled(label, &block).await? {
        outln!("Opened {name} in the Rex app on {label}");
        return Ok(());
    }
    match server.address().await? {
        Some(address) => bail!(
            "the Rex app on {label} is not connected to machine {machine}'s Rex; add {address} in Rex, then attach again"
        ),
        None => bail!(
            "the Rex app on {label} is not connected to machine {machine}'s Rex, which serves no address other machines reach; set up Tailscale in Rex on {machine}"
        ),
    }
}

/// Type `argv` into the block of `place` once its shell is back at its
/// prompt. A block whose `rex` is gone, as after Rex was uninstalled, is
/// not, so a restore falls back to another place.
async fn run_at_prompt(
    state: &Path,
    place: &Origin,
    argv: &[OsString],
    env: &[(&str, String)],
) -> Result<bool> {
    let Ok(server) = Server::recorded(&place.data) else {
        return Ok(false);
    };
    if !server.at_prompt(place.id()).await {
        return Ok(false);
    }
    let typed = Typed::new(state, argv, env)?;
    server.send(place.id(), typed.text()).await?;
    typed.hand_over()?;
    Ok(true)
}

/// Open a background window for the demo and type its worker there, which
/// needs the block it runs in, once the shell is at its prompt.
async fn start_demo(demo: &Demo<'_>) -> Result<Origin> {
    let origin = demo
        .origin
        .context("Shoal has no Rex session to open a window in")?;
    let server = Server::recorded(&origin.data)?;
    server.ensure_supported().await?;
    let session = session_of(&server, origin).await?;
    let block = server
        .open(&Shell {
            session: &session,
            label: demo.label,
            cwd: demo.cwd,
            focus: false,
            command: None,
            beside: None,
        })
        .await?;
    let place = opened(origin, block.block);
    // The demo's shell ends with it, so no later command reuses its block.
    let typed = Typed::ending_shell(demo.state, &(demo.worker)(&place)?, demo.env)?;
    typed::await_prompt(|| server.canonical(place.id())).await;
    server.send(place.id(), typed.text()).await?;
    typed.hand_over()?;
    Ok(place)
}

/// The session at `place`, named as its block is now, while the block is
/// open.
async fn session_at(place: &Origin) -> Result<Option<Session>> {
    let server = Server::recorded(&place.data)?;
    let Some(block) = server.block(place.id()).await? else {
        return Ok(None);
    };
    let name = server.name(place.id(), &block).await?;
    let name = name.unwrap_or_else(|| place.id().to_owned());
    let clients = server.showing(&block.session).await?;
    Ok(Some(Session::open(name, clients, place)))
}

/// Name the block at `place` `label`, on the server `place` records, and,
/// with `expected`, only while it shows that; a block that is gone leaves
/// nothing to rename.
async fn rename_block(place: &Origin, expected: Option<&str>, label: &str) -> Result<()> {
    let server = Server::recorded(&place.data)?;
    let Some(block) = server.block(place.id()).await? else {
        return Ok(());
    };
    if let Some(expected) = expected
        && server.name(place.id(), &block).await?.as_deref() != Some(expected)
    {
        return Ok(());
    }
    server.rename(place.id(), &block, label).await
}

#[cfg(test)]
mod tests;
