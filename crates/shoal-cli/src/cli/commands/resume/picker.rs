//! Picking the one stopped agent or last session of a workspace to restore,
//! and how the picker names each.
use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::client::execution_ids::ExecutionId;
use crate::{
    cli::{context::Context, ui},
    execution::recovery::{LastSession, LastSessionExt, Record, RecordExt, saved_at},
};

/// The execution ID and saved file of the agent to restore of `agents`: the
/// only one, or one picked on a terminal by its `label`.
pub(super) fn select_agent(
    ctx: &Context,
    agents: &[(ExecutionId, PathBuf)],
    label: fn(&Path) -> String,
) -> Result<(ExecutionId, PathBuf)> {
    match agents {
        [agent] => Ok(agent.clone()),
        agents if ctx.interactive() => {
            let labels = agents
                .iter()
                .map(|(_, path)| label(path))
                .collect::<Vec<_>>();
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
pub(super) fn agent_label(path: &Path) -> String {
    let record = Record::load(path).ok().flatten();
    let mut label = record
        .as_ref()
        .map_or_else(|| "agent".to_owned(), |record| record.agent.to_string());
    label.push_str(&stop_time(path));
    let idle = record.as_ref().is_some_and(Record::between_turns);
    if let Some(reason) = record.and_then(|record| record.stop_reason) {
        label.push_str(&format!(" ({reason})"));
    }
    label.push_str(if idle { ", idle" } else { ", interrupted" });
    label
}

/// How the agent picker names a last session: its agent and when it stopped.
pub(super) fn session_label(path: &Path) -> String {
    let agent = LastSession::load(path).ok().flatten();
    let mut label = agent.map_or_else(|| "agent".to_owned(), |last| last.agent.into());
    label.push_str(&stop_time(path));
    label
}

/// `, stopped <time>` for the agent saved at `path`, empty when unknown.
fn stop_time(path: &Path) -> String {
    saved_at(path)
        .map(|stopped| {
            let time = stopped.local_minutes();
            format!(", stopped {time}")
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
