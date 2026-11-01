//! Typing into the places hosts keep, such as a prompt for the agent there.
use anyhow::{Context as _, Result};

use super::{
    here::{current_origin, with_instance},
    hosts::named,
    launch::keeps_agents_in_sessions,
};
use crate::{config::Effective, sessions::Origin};

/// What an agent's wrapper submits to wake the agent with new updates,
/// which the prompt's hook supplies.
pub(crate) const WAKE_PROMPT: &str =
    "Shoal: new messages or activity on this workspace's issues and PRs.";

/// What `shoal resume` submits to an agent whose turn an interrupt ended.
pub(in crate::cli) const CONTINUE_PROMPT: &str = "Continue where you left off.";

/// Whether Shoal, rather than the user, submitted `prompt`: one of its
/// prompts, which a draft the user left in the input precedes when Shoal
/// types it there.
pub(in crate::cli) fn submitted_by_shoal(prompt: &str) -> bool {
    let prompt = prompt.trim_end();
    [WAKE_PROMPT, CONTINUE_PROMPT]
        .iter()
        .any(|shoal| prompt.ends_with(shoal))
}

/// Submit `text` as a prompt in `place`, as if pasted and sent: bracketed
/// paste keeps it one prompt, and Enter in the same write submits it rather
/// than ending the paste.
pub(in crate::cli) async fn submit_prompt(place: &Origin, text: &str) -> Result<()> {
    send_keys(place, &format!("\x1b[200~{text}\x1b[201~\r")).await
}

/// Type `keys` into `place`, as they are, through the host that keeps it.
pub(in crate::cli) async fn send_keys(place: &Origin, keys: &str) -> Result<()> {
    let host = named(place.host);
    let types = host
        .types()
        .with_context(|| format!("Shoal cannot type into places of {}", host.name()))?;
    types.send_keys(place, keys).await
}

/// The session this process runs in, where an agent's wrapper can type a
/// prompt for it: the place the daemon records for its execution, in a host
/// that types into its places.
#[derive(Clone)]
pub(crate) struct PromptSession {
    place: Origin,
}

impl PromptSession {
    pub(crate) fn current() -> Option<Self> {
        let place = current_origin().filter(|place| place.place.is_some())?;
        named(place.host).types()?;
        Some(Self { place })
    }

    /// The session with what tells its place apart from a later place under
    /// its ID, read while this process still runs in it, so its prompts
    /// never reach a place a restarted host gave the ID.
    pub(crate) async fn pinned(mut self) -> Result<Self> {
        self.place = with_instance(named(self.place.host), self.place).await?;
        Ok(self)
    }

    /// Submit `text` as a prompt in the session.
    pub(crate) async fn submit(&self, text: &str) -> Result<()> {
        submit_prompt(&self.place, text).await
    }
}

/// Whether an agent launched here runs in a session its wrapper can type a
/// prompt into: a new one, or the session this process already runs in.
pub(in crate::cli) fn agents_can_be_prompted(settings: &Effective) -> bool {
    keeps_agents_in_sessions(settings) || PromptSession::current().is_some()
}
