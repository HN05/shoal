//! Rex windows and blocks: Shoal opens a shell block per place, in a window
//! of its own or splitting the caller's block, and types its command into
//! the shell, since Rex sets no environment for a new terminal. Rex keeps
//! sessions running without a connected app, so agents run in place in
//! their block and outlive the Mac app or the phone.
use std::{ffi::OsStr, ffi::OsString, path::Path};

use anyhow::{Context as _, Result};
use futures_util::future::BoxFuture;

use super::{
    Demo, Host, PlaceState, Session, Start, Watched,
    host::{Demos, Places, Prompts, Sessions, Watches},
    typed::Typed,
};
use crate::{
    config::repo::RexPlacement,
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

    fn prompts(&self) -> Option<&dyn Prompts> {
        Some(self)
    }

    fn demos(&self) -> Option<&dyn Demos> {
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

    fn background_places(&self) -> &'static str {
        "Rex blocks"
    }

    fn background_note(&self, name: &str) -> String {
        format!("Started {name} in a background Rex block")
    }

    fn origin(&self) -> Option<Origin> {
        origin()
    }

    fn place_variables(&self) -> &'static [&'static str] {
        &["REX_SESSION", "REX_BLOCK"]
    }

    fn names_places(&self) -> bool {
        true
    }

    fn rename_place<'a>(&'a self, place: &'a str, label: &'a str) -> BoxFuture<'a, ()> {
        Box::pin(async move {
            if let Err(error) = rename_block(place, label).await {
                eprintln!("warning: cannot rename Rex block {place}: {error:#}");
            }
        })
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

    fn attaches_places(&self) -> bool {
        true
    }
}

impl Prompts for Rex {
    fn prompt_session(&self) -> Option<OsString> {
        in_rex()
            .then(|| var("REX_BLOCK").map(OsString::from))
            .flatten()
    }

    /// Paste `text` as one prompt and submit it with Enter.
    fn submit_prompt<'a>(&'a self, session: &'a OsStr, text: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let block = session.to_str().context("the Rex block ID is not UTF-8")?;
            Server::here()?
                .send(block, &format!("\x1b[200~{text}\x1b[201~\r"))
                .await
                .context("type into the Rex block")
        })
    }
}

impl Demos for Rex {
    fn start_demo<'a>(&'a self, demo: &'a Demo<'a>) -> BoxFuture<'a, Result<Origin>> {
        Box::pin(start_demo(demo))
    }

    fn demo_screen<'a>(&'a self, place: &'a Origin) -> BoxFuture<'a, Result<String>> {
        Box::pin(async move { Server::recorded(&place.data)?.capture(place.id()).await })
    }

    fn send_demo_keys<'a>(&'a self, place: &'a Origin, keys: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move { Server::recorded(&place.data)?.send(place.id(), keys).await })
    }
}

/// What Rex's section sets: `rex.enabled`, whether it opens places
/// (`rex.new_window`) and splits the caller's block for them
/// (`rex.placement`), their names (`rex.window_name`), the session for a
/// terminal outside Rex (`rex.session_name`), and `rex.close_when_done`,
/// which overrides the top-level option.
pub(super) fn options(config: &crate::config::Rex) -> super::Options {
    super::Options {
        enabled: config.enabled,
        opens_places: config.new_window,
        opens_splits: config.placement == RexPlacement::Split,
        name_template: config.window_name.clone(),
        group_template: Some(config.session_name.clone()),
        close_when_done: config.close_when_done,
    }
}

/// The value of variable `name`, when set and not empty.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Whether this process runs in a Rex block.
fn in_rex() -> bool {
    var("REX_SESSION").is_some() && var("REX_BLOCK").is_some()
}

/// The block this process runs in, in its session, with how to reach its
/// server.
fn origin() -> Option<Origin> {
    if !in_rex() {
        return None;
    }
    let server = Server::here().ok()?;
    Some(Origin {
        host: HostName::Rex,
        group: var("REX_SESSION"),
        place: var("REX_BLOCK"),
        data: server.data(),
        owned: false,
        instance: None,
    })
}

/// The server of `origin`, or the one this process reaches.
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
    })
}

/// Open a block for `start` in its origin's session, in a window of its own
/// or splitting the caller's block, and type its command there.
async fn open_block(start: &Start<'_>) -> Result<Origin> {
    let origin = start
        .origin
        .context("Shoal has no Rex session to open a window in")?;
    let server = Server::recorded(&origin.data)?;
    server.ensure_supported().await?;
    let session = origin
        .group
        .as_deref()
        .context("the Rex origin names no session")?;
    let typed = Typed::new(start.state, start.argv, start.env)?;
    let opened = server
        .open(&Shell {
            session,
            label: start.label,
            cwd: start.cwd,
            focus: start.focus,
            input: Some(typed.text()),
            beside: start.split,
        })
        .await?;
    typed.hand_over()?;
    Ok(Origin {
        place: Some(opened.block),
        owned: true,
        ..origin.clone()
    })
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
/// needs the block it runs in.
async fn start_demo(demo: &Demo<'_>) -> Result<Origin> {
    let origin = demo
        .origin
        .context("Shoal has no Rex session to open a window in")?;
    let server = Server::recorded(&origin.data)?;
    server.ensure_supported().await?;
    let session = origin
        .group
        .as_deref()
        .context("the Rex origin names no session")?;
    let opened = server
        .open(&Shell {
            session,
            label: demo.label,
            cwd: demo.cwd,
            focus: false,
            input: None,
            beside: None,
        })
        .await?;
    let place = Origin {
        place: Some(opened.block),
        owned: true,
        ..origin.clone()
    };
    // The demo's shell ends with it, so no later command reuses its block.
    let typed = Typed::ending_shell(demo.state, &(demo.worker)(&place)?, demo.env)?;
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

async fn rename_block(place: &str, label: &str) -> Result<()> {
    let server = Server::here()?;
    let block = server.block(place).await?.context("the block is gone")?;
    server.rename(place, &block, label).await
}

#[cfg(test)]
mod tests;
