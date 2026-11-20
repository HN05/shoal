//! `shoal attach`'s steps, which a local attach and the worker the daemon
//! runs for a user on another machine both take: finding a space's agent
//! sessions, waking an agent that sleeps or is stopped there, as
//! [`crate::resume`] resumes one, choosing a session and showing it
//! through the host that keeps it. The caller passes in what it does its
//! own way through [`Attaching`].
mod elsewhere;
mod restores;
mod sessions;
mod target;

use anyhow::Result;
use shoal_protocol::space_names::SpaceSelector;

use crate::{
    client::connection::CommandPaths,
    model::QueueEntry,
    resume::{Resuming, Starter},
    terminal::Session,
};

pub use elsewhere::for_viewer;
pub use restores::entry_session;
use restores::{POLL, WAIT};
pub use target::Target;

/// What a caller of attach does its own way: picking a session, what a
/// resume of a stopped agent does its own way and what starts that agent,
/// and telling the user what starts.
pub trait Attaching: Resuming {
    /// The one of `sessions`, several an attach that named none offers,
    /// that the user picks.
    fn pick_session<'a>(&self, sessions: &[&'a Session]) -> Result<&'a Session>;

    /// What starts a stopped agent the attach resumes.
    fn starter(&self) -> Starter;

    /// Tell the user that the queued restore or launch `entry` starts.
    fn resuming(&self, entry: &QueueEntry) -> Result<()>;
}

/// Attach this terminal to a session of `target`: the one `session` names,
/// else its only one or one `attaching` picks, once the agent there runs.
/// With no session named and none showing an agent, an agent that sleeps
/// wakes first, and a stopped one when no session is left; a session where
/// Shoal stopped the agent wakes it. Returns whether the session still runs
/// once this terminal left it.
pub async fn here(
    paths: &CommandPaths,
    target: Target,
    session: Option<String>,
    attaching: &impl Attaching,
) -> Result<bool> {
    report_attach(paths, &target).await?;
    let steps = sessions::Steps { paths, attaching };
    let (target, session) = steps.awake_session(target, session, None).await?;
    crate::terminal::attach(&target.number().to_string(), &session).await
}

/// Tell the daemon of the attach: attaching is the user's interaction with
/// the space's agent.
async fn report_attach(paths: &CommandPaths, target: &Target) -> Result<()> {
    crate::client::agents::report_attach(paths, SpaceSelector::id(target.id())).await
}
