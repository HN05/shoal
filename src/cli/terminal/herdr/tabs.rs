//! The tab holding a Herdr place. A place is a pane, while Herdr closes and
//! renames tabs; a watcher of an earlier release names the tab itself. These
//! reach the server this process runs under, as the watcher it serves does.
use anyhow::{Context as _, Result};
use tokio::process::Command;

use super::{AgentStatus, TabInfo, pane, query};
use crate::{cli::terminal::PlaceState, subprocess::Run};

/// The tab holding a place, with the agent status Herdr shows for the place.
struct Holding {
    tab: String,
    status: AgentStatus,
}

/// The tab holding `place`, or `None` once Herdr reports the place gone.
async fn holding(place: &str) -> Result<Option<Holding>> {
    if let Some(pane) = pane(Command::new("herdr"), place).await? {
        return Ok(Some(Holding {
            tab: pane.tab_id,
            status: pane.agent_status,
        }));
    }
    let tab = query::<TabInfo>(Command::new("herdr"), "tab", place).await?;
    Ok(tab.map(|info| Holding {
        tab: place.to_owned(),
        status: info.result.tab.agent_status,
    }))
}

/// Whether `place` is open, with its agent's turn as Herdr shows it.
pub(super) async fn state(place: &str) -> Result<PlaceState> {
    Ok(match holding(place).await? {
        Some(holding) => PlaceState::Open(holding.status.turn()),
        None => PlaceState::Gone,
    })
}

/// Whether `place` and `here` are in one tab, so closing the tab of `place`
/// ends `here`. A place that is gone has no tab left to close; a failed query
/// proves nothing, so it counts as one tab.
pub(super) async fn shared(place: &str, here: &str) -> bool {
    if place == here {
        return true;
    }
    match (holding(place).await, holding(here).await) {
        (Ok(Some(place)), Ok(Some(here))) => place.tab == here.tab,
        (Ok(None), _) | (_, Ok(None)) => false,
        _ => true,
    }
}

/// Close the tab holding `place`; a place that is gone leaves nothing to
/// close.
pub(super) async fn close(place: &str) -> Result<()> {
    let Some(holding) = holding(place).await? else {
        return Ok(());
    };
    run(["tab", "close", &holding.tab])
        .await
        .with_context(|| format!("cannot close Herdr tab {}", holding.tab))
}

/// Show `label` as the name of the tab holding `place`.
pub(super) async fn rename(place: &str, label: &str) {
    let renamed = async {
        let holding = holding(place).await?.context("the pane is gone")?;
        run(["tab", "rename", &holding.tab, label]).await
    };
    if let Err(error) = renamed.await {
        eprintln!("warning: cannot rename the Herdr tab of {place}: {error:#}");
    }
}

async fn run<const N: usize>(args: [&str; N]) -> Result<()> {
    let mut command = Command::new("herdr");
    command.args(args);
    Run::new(command).checked().await?;
    Ok(())
}
