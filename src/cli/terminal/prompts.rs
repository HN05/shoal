//! The prompts Shoal submits to agents, and whether an agent launched here
//! can take one.
use super::launch::keeps_agents_in_sessions;
use crate::{
    config::Effective,
    terminal::{PromptSession, WAKE_PROMPT},
};

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

/// Whether an agent launched here runs in a session its wrapper can type a
/// prompt into: a new one, or the session this process already runs in.
pub(in crate::cli) fn agents_can_be_prompted(settings: &Effective) -> bool {
    keeps_agents_in_sessions(settings) || PromptSession::current().is_some()
}
