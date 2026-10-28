//! Herdr pane ownership and verified closes, independent of the daemon's
//! lifecycle policy. Server addresses keep identically named panes apart.
mod native;
mod records;

use anyhow::Result;
use futures_util::future::BoxFuture;
use std::path::Path;

use super::{Host, Owner, Session};
use records::Place;

pub(super) struct Herdr;

impl Host for Herdr {
    fn name(&self) -> &'static str {
        "herdr"
    }

    fn sessions<'a>(&'a self, state: &'a Path, _: bool) -> BoxFuture<'a, Result<Vec<Session>>> {
        Box::pin(async move {
            Ok(records::list(state)?
                .into_iter()
                .map(|place| Session {
                    name: place.key(),
                    workspace: place.workspace,
                })
                .collect())
        })
    }

    fn close<'a>(
        &'a self,
        state: &'a Path,
        session: &'a str,
        owner: &'a Owner,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let _lock = records::lock(state).await?;
            if let Some(place) = records::owned(state, session, owner)? {
                close_owned(state, &place).await?;
            }
            Ok(())
        })
    }
}

/// Replace workspace ownership, or clear it when the pane becomes a chat.
/// No default/focused server is ever selected for cleanup.
pub(crate) async fn label_current(state: &Path, workspace: Option<&str>) -> Result<()> {
    if let Some(pane) = native::current_pane()? {
        record(state, &pane, workspace, None).await?;
    }
    Ok(())
}

pub(crate) async fn record_demo(
    state: &Path,
    pane: &str,
    workspace: &str,
    launch: &str,
) -> Result<()> {
    record(state, pane, Some(workspace), Some(launch)).await
}

async fn record(
    state: &Path,
    pane: &str,
    workspace: Option<&str>,
    demo: Option<&str>,
) -> Result<()> {
    let place = Place::current_server(pane, workspace, demo)?;
    let _lock = records::lock(state).await?;
    let _server = super::locking::for_socket(Path::new(&place.socket)).await?;
    if workspace.is_none() {
        native::release(&place).await?;
        return records::forget(state, &place.key());
    }
    records::write(state, &native::claim(place).await?)
}

pub(crate) async fn close_demo(state: &Path, pane: &str, launch: &str) -> Result<()> {
    Herdr.close(state, pane, &Owner::Demo(launch.into())).await
}

async fn close_owned(state: &Path, place: &Place) -> Result<()> {
    let _server = super::locking::for_socket(Path::new(&place.socket)).await?;
    if native::owns(place).await? {
        native::close_pane(place).await?;
    }
    records::forget(state, &place.key())
}

#[cfg(test)]
mod tests;
