//! Where the agent of a start runs when it runs in its caller, and whether
//! it skips the queue there because the user waits for it.
use super::detached::leaves_caller;
use crate::{
    cli::context::Context,
    config::{Effective, modes::ModeName},
};
use shoal_protocol::launches::{Launch, StartTarget};
use shoal_workers::hosting;

#[derive(Debug, Clone, Copy, Default)]
pub(in crate::cli::commands) struct StartOptions {
    /// Run in this terminal instead of a new tab or detached session.
    pub here: bool,
    /// Focus the new tab or session whatever the `focus` setting says.
    pub focus: bool,
    /// Wait for a new workspace's preparation before returning.
    pub wait: bool,
}

impl StartOptions {
    pub(in crate::cli::commands) fn of(args: &crate::cli::args::create::StartArgs) -> Self {
        Self {
            here: args.here,
            focus: args.focus,
            wait: args.wait,
        }
    }
}

pub(in crate::cli::commands) use shoal_workers::starts::Attach;

/// `None` leaves the agent's start to the daemon. An `issue_linked` workspace's
/// own issue prompts the agent where the launch names nothing to start on.
pub(in crate::cli::commands) fn attach(
    ctx: &Context,
    (launch, issue_linked): (&Launch, bool),
    agent: Option<&crate::agent::Agent>,
    settings: &Effective,
    options: &StartOptions,
) -> Option<Attach> {
    if launch.machine.is_some() {
        return None;
    }
    let own_way = !crate::env::is_scoped()
        && agent.is_some_and(|agent| shoal_workers::launch::runs_its_own_way(agent, settings));
    if leaves_caller(ctx, options.here) {
        return own_way.then_some(Attach::Here);
    }
    // Without a host that opens places beside it, a terminal runs
    // every start's agent here.
    if options.here || !hosting::opens_beside(ctx.caller(), settings) {
        return Some(Attach::Here);
    }
    let requested = options.focus.then_some(crate::terminal::Placement::Focus);
    let waits_for_user = agent.is_none() || waits(launch, issue_linked);
    let placement =
        crate::terminal::Placement::of(settings, &mode(launch), requested, waits_for_user);
    // The queue starts only an agent of its own in the background; a
    // shell, or an agent that runs its own way, takes a place from here.
    let queued = agent.is_some() && !own_way;
    match (placement, queued) {
        (crate::terminal::Placement::Focus, _) => Some(Attach::Beside { focused: true }),
        (crate::terminal::Placement::Background, false) => Some(Attach::Beside { focused: false }),
        (crate::terminal::Placement::Background, true) => None,
    }
}

/// The user's waiting never lets a scoped caller's start skip the queue.
pub(in crate::cli::commands) fn in_caller(
    ctx: &Context,
    (launch, issue_linked): (&mut Launch, bool),
    settings: &Effective,
    options: &StartOptions,
) {
    launch.here = true;
    let setting = settings.focus(&mode(launch));
    let waits_for_user = ctx.on_terminal() && waits(launch, issue_linked);
    let now = starts_now(launch, setting, options, waits_for_user);
    launch.now |= !crate::env::is_scoped() && now;
}

/// The user waits for one `--focus` focuses, or for one that waits for input on
/// a terminal unless the `focus` setting sends it to the background. The
/// setting alone cannot switch the limit off. A start that waits for work
/// queues.
fn starts_now(
    launch: &Launch,
    setting: Option<bool>,
    options: &StartOptions,
    waits_for_user: bool,
) -> bool {
    options.focus || (launch.blockers.is_empty() && waits_for_user && setting != Some(false))
}

fn mode(launch: &Launch) -> ModeName {
    (launch.mode.clone()).unwrap_or(ModeName::DEFAULT)
}

/// Whether the agent of `launch` waits for the user's input: it starts on
/// no issue, task or prompt, nor on the issue of an `issue_linked`
/// workspace.
fn waits(launch: &Launch, issue_linked: bool) -> bool {
    let on_issue = issue_linked
        || launch.issue.is_some()
        || matches!(
            launch.target,
            Some(StartTarget::Issue(_) | StartTarget::Number(_))
        );
    !on_issue && launch.task.is_empty() && launch.prompt.is_none()
}

#[cfg(test)]
mod tests;
