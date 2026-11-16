//! Triggers: `[triggers.NAME]` config that starts agents when an event
//! source fires, and the runtime state the daemon keeps for each.
use serde::{Deserialize, Serialize};
use shoal_ids::space_ids::SpaceId;
use shoal_states::states;

use crate::time::UnixTime;

states!(
    /// Where a trigger's firings come from.
    SourceKind: Variants {
        /// Clock times or an interval.
        Timer => "timer",
    }
);

states!(
    /// What made a trigger fire.
    RunOrigin: Variants {
        /// Its source.
        Source => "source",
        /// `shoal trigger run`.
        Manual => "manual",
    }
);

states!(
    /// What became of a firing that reached a run.
    RunOutcome: Variants {
        /// It launched, starting the space it names.
        Started => "started",
        /// It waits for the agent of the trigger's last space to stop.
        Queued => "queued",
        /// It did not launch: its overlap skipped it, or it came too late
        /// to catch up.
        Skipped => "skipped",
        /// Its key matched a run's within the key window.
        Duplicate => "duplicate",
        /// It could not launch, for the reason it records.
        Failed => "failed",
    }
);

/// One run of a trigger, as its history keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerRun {
    /// When it fired, or the scheduled time it skipped.
    pub at: UnixTime,
    pub origin: RunOrigin,
    pub outcome: RunOutcome,
    /// Why it skipped or failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// The space it started, or the one it waits for while queued.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub space_id: Option<SpaceId>,
}
