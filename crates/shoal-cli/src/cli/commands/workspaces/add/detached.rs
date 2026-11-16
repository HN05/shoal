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

/// Whether the daemon carries out a start not run `here` whose agent, if
/// any, is `agent`: one for a scoped caller, whose own processes its agent
/// must not be, or without a terminal to start it beside, unless the user's
/// agent runs its own way under `settings`, which starts in this process.
pub(in crate::cli::commands) fn daemon_starts(
    ctx: &Context,
    here: bool,
    agent: Option<&crate::agent::Agent>,
    settings: &Effective,
) -> bool {
    let own_way = !crate::env::is_scoped()
        && agent.is_some_and(|agent| agents::runs_its_own_way(agent, settings));
    leaves_caller(ctx, here) && !own_way
}

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
