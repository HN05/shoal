//! The prompts Shoal submits to agents.
use crate::terminal::WAKE_PROMPT;

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
