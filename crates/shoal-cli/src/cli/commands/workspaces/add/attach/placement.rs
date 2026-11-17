//! Where the agent of a start runs when it runs in its caller, and whether
//! it skips the queue there because the user waits for it.
use super::super::{AddOptions, detached::leaves_caller};
use crate::{
    cli::{agents, context::Context, terminal},
    config::{Effective, modes::ModeName},
};
use shoal_protocol::launches::{Launch, StartTarget};

/// Where the agent of a start runs when it runs in its caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::cli::commands::workspaces::add) enum Attach {
    /// In this terminal, or in this process for an agent that runs its own
    /// way, which leaves it at once.
    Here,
    /// In a new place beside this terminal, `focused` or in the background,
    /// else here.
    Beside { focused: bool },
}

impl Attach {
    /// Where the agent of `launch`, if it names one, runs in its caller as
    /// `options` and `settings` say: `--here` runs here, as the user's
    /// agent that runs its own way does without a terminal; on a terminal a
    /// focused start runs in a focused place, as a shell or an agent that
    /// runs its own way takes a place beside it, or here where no host
    /// opens places. `None` leaves the agent's start to the daemon. An
    /// `issue_linked` workspace's own issue prompts the agent where the
    /// launch names nothing to start on.
    pub(in crate::cli::commands::workspaces::add) fn of(
        ctx: &Context,
        (launch, issue_linked): (&Launch, bool),
        agent: Option<&crate::agent::Agent>,
        settings: &Effective,
        options: &AddOptions,
    ) -> Option<Self> {
        let own_way = !crate::env::is_scoped()
            && agent.is_some_and(|agent| agents::runs_its_own_way(agent, settings));
        if leaves_caller(ctx, options.here) {
            return own_way.then_some(Self::Here);
        }
        // Without a host that opens places beside it, a terminal runs
        // every start's agent here.
        if options.here || !terminal::opens_beside(ctx, settings) {
            return Some(Self::Here);
        }
        let requested = options.focus.then_some(terminal::Placement::Focus);
        let waits_for_user = agent.is_none() || waits(launch, issue_linked);
        let placement = terminal::Placement::of(settings, &mode(launch), requested, waits_for_user);
        // The queue starts only an agent of its own in the background; a
        // shell, or an agent that runs its own way, takes a place from here.
        let queued = agent.is_some() && !own_way;
        match (placement, queued) {
            (terminal::Placement::Focus, _) => Some(Self::Beside { focused: true }),
            (terminal::Placement::Background, false) => Some(Self::Beside { focused: false }),
            (terminal::Placement::Background, true) => None,
        }
    }
}

/// `launch` with its agent run in this caller, skipping the queue where
/// the user waits for it under `settings`, unless a scoped caller makes it;
/// an `issue_linked` workspace's own issue prompts the agent where the
/// launch names nothing to start on.
pub(in crate::cli::commands::workspaces::add) fn in_caller(
    ctx: &Context,
    (launch, issue_linked): (&mut Launch, bool),
    settings: &Effective,
    options: &AddOptions,
) {
    launch.here = true;
    let setting = settings.focus(&mode(launch));
    let waits_for_user = ctx.on_terminal() && waits(launch, issue_linked);
    let now = starts_now(launch, setting, options, waits_for_user);
    launch.now |= !crate::env::is_scoped() && now;
}

/// Whether the agent of `launch`, which runs in its caller, skips the
/// queue: the user waits for one `--focus` focuses, or one that
/// `waits_for_user` on a terminal and the `focus` `setting` does not send
/// to the background; the setting alone does not, so it cannot switch the
/// limit off. A start that waits for work queues.
fn starts_now(
    launch: &Launch,
    setting: Option<bool>,
    options: &AddOptions,
    waits_for_user: bool,
) -> bool {
    options.focus || (launch.blockers.is_empty() && waits_for_user && setting != Some(false))
}

/// The mode `launch` starts its agent in.
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
