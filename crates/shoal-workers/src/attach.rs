//! `shoal attach`'s steps, which a local attach and the worker the daemon
//! runs for a user on another machine both take: finding a space's agent
//! sessions, waking an agent that sleeps or is stopped there, choosing a
//! session and showing it through the host that keeps it. The caller
//! passes in what it does its own way through [`Attaching`].
mod elsewhere;
mod restores;
mod sessions;
mod target;

use anyhow::Result;
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

use crate::{client::connection::CommandPaths, model::QueueEntry, terminal::Session};

pub use elsewhere::for_viewer;
pub use restores::entry_session;
use restores::{POLL, WAIT};
pub use target::Target;

/// What a caller of attach does its own way: picking a session, resuming a
/// stopped agent, and telling the user what starts.
pub trait Attaching: Sync {
    /// The one of `sessions`, several an attach that named none offers,
    /// that the user picks.
    fn pick_session<'a>(&self, sessions: &[&'a Session]) -> Result<&'a Session>;

    /// Resume the stopped agent of the space `space` at once, as
    /// `shoal resume --now` does; whether the resume succeeded, which
    /// reported why it did not.
    fn resume_now(&self, space: &SpaceId) -> impl Future<Output = Result<bool>>;

    /// Tell the user that the queued restore `entry` starts.
    fn resuming(&self, entry: &QueueEntry) -> Result<()>;
}

/// Attach this terminal to a session of `target`: the one `session` names,
/// else its only one or one `attaching` picks, waking its agent first as
/// [`Target::awake_session`] says. Returns whether the session still runs
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
