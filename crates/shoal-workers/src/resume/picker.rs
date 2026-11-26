//! Picking the stopped agent or last session of a space to restore.
use anyhow::Result;
use shoal_core::execution::recovery::SavedSession;

use super::Resuming;

/// The agent to restore of `agents`: the only one, or one `resuming` picks
/// among several by its `label`.
pub(super) fn select_agent(
    resuming: &impl Resuming,
    agents: &[SavedSession],
    label: fn(&SavedSession) -> String,
) -> Result<SavedSession> {
    match agents {
        [agent] => Ok(agent.clone()),
        agents => {
            let labels: Vec<_> = agents.iter().map(label).collect();
            resuming.pick_agent(agents, &labels)
        }
    }
}

/// How the agent picker names a stopped agent: its name, when it stopped,
/// why when known, and whether it was interrupted mid-turn or idle.
pub(super) fn agent_label(agent: &SavedSession) -> String {
    let mut label = session_label(agent);
    if let Some(reason) = &agent.stop_reason {
        label.push_str(&format!(" ({reason})"));
    }
    label.push_str(if agent.idle {
        ", idle"
    } else {
        ", interrupted"
    });
    label
}

/// How the agent picker names a last session: its agent and when it stopped.
pub(super) fn session_label(session: &SavedSession) -> String {
    let mut label =
        (session.agent.as_ref()).map_or_else(|| "agent".to_owned(), ToString::to_string);
    if let Some(stopped) = session.stopped_at {
        label.push_str(&format!(", stopped {}", stopped.local_minutes()));
    }
    label
}

#[cfg(test)]
mod tests;
