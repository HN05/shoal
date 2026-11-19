//! Starts that leave the caller, as scoped callers' and ones without a
//! terminal do unless `--here` runs them in place: the daemon carries them
//! out from one launch, outside the caller, so interrupting the caller
//! leaves their agents running, unless the user's agent runs its own way.
use anyhow::{Result, ensure};

use super::placement::Attach;
use crate::cli::commands::workspaces::AddOptions;
use crate::{
    cli::{agents, context::Context},
    config::Effective,
};
use shoal_protocol::launches::Launch;

/// Whether a start not run `here` leaves this process: for a scoped
/// caller, or for one without a terminal.
pub(in crate::cli::commands) fn leaves_caller(ctx: &Context, here: bool) -> bool {
    !here && (crate::env::is_scoped() || !ctx.on_terminal())
}

/// Refuse, before any work is created, a detached start of `agent` that
/// `--focus` asks to focus, which nothing shows. The daemon starts the
/// agent from the background, which some host must take, unless the user's
/// agent runs its own way.
pub(in crate::cli::commands) fn ensure_detachable(
    settings: &Effective,
    agent: Option<&crate::agent::Agent>,
    options: &AddOptions,
) -> Result<()> {
    let Some(agent) = agent else {
        return Ok(());
    };
    ensure_unfocused(options)?;
    if !crate::env::is_scoped() && agents::runs_its_own_way(agent, settings) {
        return Ok(());
    }
    crate::terminal::queued::ensure_background(settings)
}

/// Why the daemon's queued start of an agent under `settings` could not
/// run from this terminal, which the launch carries for the daemon to
/// refuse it with once it knows it starts one, as `--focus`, which nothing
/// shows, is refused here.
fn unplaceable(
    settings: &Effective,
    agent: Option<&crate::agent::Agent>,
    options: &AddOptions,
) -> Result<Option<String>> {
    if agent.is_some() {
        ensure_unfocused(options)?;
    }
    let refused = crate::terminal::queued::ensure_background(settings).err();
    Ok(refused.map(|error| format!("{error:#}")))
}

/// Refuse `--focus` for an agent that starts detached, with nothing to
/// focus.
fn ensure_unfocused(options: &AddOptions) -> Result<()> {
    ensure!(
        !options.focus,
        "a call without a terminal starts its agent detached, with nothing to focus; leave out --focus, or pass --here to run it in place"
    );
    Ok(())
}

/// Ready `launch` for where its `agent` runs: in this caller as `attach`
/// says, which needs the agent here before anything is created, or else
/// from the daemon, which refuses one no host can place for a start that
/// leaves this caller, as `--focus` is refused there. An `issue_linked`
/// workspace's own issue prompts the agent where the launch names nothing
/// to start on.
pub(in crate::cli::commands) fn place_agent(
    ctx: &Context,
    (launch, issue_linked): (&mut Launch, bool),
    (attach, agent): (Option<Attach>, Option<&crate::agent::Agent>),
    settings: &Effective,
    options: &AddOptions,
) -> Result<()> {
    if leaves_caller(ctx, options.here) {
        let refused = unplaceable(settings, agent, options)?;
        launch.unplaceable = refused.filter(|_| attach.is_none());
    }
    if attach.is_some() {
        if let Some(agent) = agent {
            crate::agents::ensure_available(agent, settings)?;
        }
        super::placement::in_caller(ctx, (launch, issue_linked), settings, options);
    }
    Ok(())
}
