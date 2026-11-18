//! Starts that leave the caller, as scoped callers' and ones without a
//! terminal do unless `--here` runs them in place: the daemon carries them
//! out from one launch, outside the caller, so interrupting the caller
//! leaves their agents running, unless the user's agent runs its own way.
use anyhow::{Result, ensure};

use super::AddOptions;
use crate::{
    cli::{agents, context::Context},
    config::Effective,
};

/// Whether a start not run `here` leaves this process: for a scoped
/// caller, or for one without a terminal.
pub(in crate::cli::commands) fn leaves_caller(ctx: &Context, here: bool) -> bool {
    !here && (crate::env::is_scoped() || !ctx.on_terminal())
}

/// Refuse, before any work is created, a detached start of `agent` that
/// `--focus` asks to focus, which nothing shows. The daemon starts the
/// agent from the background, which some host must take, unless the user's
/// agent runs its own way.
pub(super) fn ensure_detachable(
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
pub(super) fn unplaceable(
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
