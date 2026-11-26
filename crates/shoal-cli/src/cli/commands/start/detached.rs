//! Starts that leave the caller, as scoped callers' and ones without a terminal
//! do unless `--here`. The daemon carries them out, so interrupting the caller
//! leaves their agents running.
use anyhow::{Result, ensure};

use super::placement::{Attach, StartOptions};
use crate::{cli::context::Context, config::Effective};
use shoal_protocol::launches::Launch;

pub(in crate::cli::commands) fn leaves_caller(ctx: &Context, here: bool) -> bool {
    !here && (crate::env::is_scoped() || !ctx.on_terminal())
}

/// The launch carries it for the daemon to refuse the start with once it knows
/// it starts an agent, as `--focus` is refused here.
fn unplaceable(
    settings: &Effective,
    agent: Option<&crate::agent::Agent>,
    options: &StartOptions,
) -> Result<Option<String>> {
    if agent.is_some() {
        ensure_unfocused(options)?;
    }
    let refused = crate::terminal::queued::ensure_background(settings).err();
    Ok(refused.map(|error| format!("{error:#}")))
}

fn ensure_unfocused(options: &StartOptions) -> Result<()> {
    ensure!(
        !options.focus,
        "a call without a terminal starts its agent detached, with nothing to focus; leave out --focus, or pass --here to run it in place"
    );
    Ok(())
}

/// An agent run in this caller is needed before anything is created. The daemon
/// refuses an agent no host can place for a start that leaves this caller.
pub(in crate::cli::commands) fn place_agent(
    ctx: &Context,
    (launch, issue_linked): (&mut Launch, bool),
    (attach, agent): (Option<Attach>, Option<&crate::agent::Agent>),
    settings: &Effective,
    options: &StartOptions,
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
