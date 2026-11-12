//! Picking the one stopped agent or last session of a workspace to restore,
//! and how the picker names each.
use anyhow::Result;

use crate::{
    cli::{context::Context, ui},
    execution::recovery::SavedSession,
};

/// The agent to restore of `agents`: the only one, or one picked on a
/// terminal by its `label`.
pub(super) fn select_agent(
    ctx: &Context,
    agents: &[SavedSession],
    label: fn(&SavedSession) -> String,
) -> Result<SavedSession> {
    match agents {
        [agent] => Ok(agent.clone()),
        agents if ctx.interactive() => {
            let labels = agents.iter().map(label).collect::<Vec<_>>();
            let choices = agents
                .iter()
                .cloned()
                .zip(labels.iter().map(String::as_str))
                .collect::<Vec<_>>();
            ui::pick_choice(ctx, "Resume agent> ", &choices)
        }
        _ => anyhow::bail!("multiple stopped agents; select one with --execution <id>"),
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
