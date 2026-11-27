//! How a restored agent's session ended, and what its restore tells it.
use std::path::Path;

use super::{Record, Stop};
use crate::state::AgentState;

/// Operations on the client type [`Stop`] that only the `shoal` command uses.
pub trait StopExt {
    /// The stop of the agent `record`, saved at `path`, keeps.
    fn of(record: &Record, path: &Path) -> Self;

    /// The sentence telling the restored agent about the stop.
    fn describe(&self) -> String;
}

impl StopExt for Stop {
    fn of(record: &Record, path: &Path) -> Self {
        Self {
            reason: record.stop_reason.clone(),
            at: saved_at(path),
            oom_killed: record.oom_kills > 0,
        }
    }

    fn describe(&self) -> String {
        let at = self
            .at
            .map(|at| format!(" at {}", at.local_minutes()))
            .unwrap_or_default();
        let reason = self
            .reason
            .as_deref()
            .map(|reason| format!(" ({reason})"))
            .unwrap_or_default();
        let stopped = format!(
            "This session stopped{at}{reason}; its subagents and background commands ended."
        );
        match self.oom_killed {
            true => format!("{stopped} {OOM_ADVICE}"),
            false => stopped,
        }
    }
}

/// When the file at `path` was last saved, which is when a saved agent
/// stopped.
pub(crate) fn saved_at(path: &Path) -> Option<crate::time::UnixTime> {
    let modified = std::fs::metadata(path).and_then(|metadata| metadata.modified());
    Some(crate::time::UnixTime::from_system_time(modified.ok()?))
}

/// How a restored agent's session ended, which decides whether it is told
/// to continue.
#[derive(Debug, Clone, Copy, Default)]
pub struct Ending<'a> {
    /// Why and when Shoal stopped it; none for one that exited on its own
    /// or whose end nothing records.
    pub stop: Option<&'a Stop>,
    /// What it was doing then, as its hooks last reported it.
    pub turn: Option<AgentState>,
}

impl Ending<'_> {
    /// Whether it ended in the middle of a turn: one whose turn is unknown
    /// counts as such.
    pub fn mid_turn(self) -> bool {
        !idle(self.turn)
    }
}

/// What the OOM killer's victim is told, so that it uses less memory.
const OOM_ADVICE: &str = "The kernel's OOM killer ended it, so go easier on memory, such as by \
    running builds and tests through `shoal run`, one at a time.";

/// What a restored session is told, if anything: when it ended mid-turn,
/// why and when it stopped when known and to continue, then `handoff`;
/// otherwise `handoff` alone, so that it waits for what comes next, after
/// why and when it stopped when the OOM killer ended it.
pub fn resumed_prompt(ending: Ending<'_>, handoff: Option<&str>) -> Option<String> {
    let mut prompt = (ending.stop)
        .filter(|stop| ending.mid_turn() || stop.oom_killed)
        .map(|stop| stop.describe());
    if ending.mid_turn() {
        let continued = "Continue working on the unfinished assignment from where you left off.";
        prompt = Some(prompt.map_or_else(
            || continued.to_owned(),
            |told| format!("{told} {continued}"),
        ));
    }
    let Some(handoff) = handoff else {
        return prompt;
    };
    Some(match prompt {
        Some(prompt) => format!("{prompt}\n\n{handoff}"),
        None => handoff.to_owned(),
    })
}

/// Whether an agent that reported `turn` last was between turns, with no
/// work in flight.
pub(super) fn idle(turn: Option<AgentState>) -> bool {
    turn == Some(AgentState::Idle)
}
