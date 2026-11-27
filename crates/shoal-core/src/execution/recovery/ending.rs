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
        format!("This session stopped{at}{reason}; its subagents and background commands ended.")
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

/// The stop reason of an agent the OOM killer ended, which its restore tells
/// it whenever it stopped.
pub const OOM_REASON: &str = "the OOM killer ended it";

/// What an agent the OOM killer ended is told to do about it.
const OOM_ADVICE: &str = "Use less memory, such as by running builds through `shoal run`.";

/// What a restored session is told, if anything: when it ended mid-turn,
/// why and when it stopped when known and to continue, then `handoff`;
/// otherwise `handoff` alone, so that it waits for what comes next. One the
/// OOM killer ended is told of it either way.
pub fn resumed_prompt(ending: Ending<'_>, handoff: Option<&str>) -> Option<String> {
    let oom = (ending.stop).filter(|stop| stop.reason.as_deref() == Some(OOM_REASON));
    let told = match (ending.mid_turn(), ending.stop) {
        (true, stop) => {
            let stop = stop.map(|stop| format!("{} ", stop.describe()));
            let advice = oom.map(|_| format!("{OOM_ADVICE} "));
            let next = "Continue working on the unfinished assignment from where you left off.";
            Some(format!(
                "{}{}{next}",
                stop.unwrap_or_default(),
                advice.unwrap_or_default()
            ))
        }
        (false, _) => oom.map(|stop| format!("{} {OOM_ADVICE}", stop.describe())),
    };
    match (told, handoff) {
        (Some(told), Some(handoff)) => Some(format!("{told}\n\n{handoff}")),
        (told, handoff) => told.or_else(|| handoff.map(str::to_owned)),
    }
}

/// Whether an agent that reported `turn` last was between turns, with no
/// work in flight.
pub(super) fn idle(turn: Option<AgentState>) -> bool {
    turn == Some(AgentState::Idle)
}
