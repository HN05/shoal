//! CI jobs a machine's sleep or shutdown cut off, which it hands the hub
//! to rerun once their runs finish, so they need not wait for it to wake.
use serde::{Deserialize, Serialize};

use crate::{
    forge::{items::reference::ItemRef, numbers::JobId},
    time::UnixTime,
};

/// A CI job a machine cut off and recorded to rerun: the job `job`, named
/// `name`, of the run `run`, stopped at `stopped_at` by what `cut_off_by`
/// names, such as the machine's sleep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CutOffJob {
    pub run: ItemRef,
    pub job: JobId,
    pub name: String,
    pub cut_off_by: String,
    pub stopped_at: UnixTime,
}

impl CutOffJob {
    /// This job with its stop on the hub's clock, as `clocks` carry it
    /// over.
    pub fn on_hub_clock(self, clocks: super::sleep::Clocks) -> Self {
        Self {
            stopped_at: clocks.on_hub(self.stopped_at),
            ..self
        }
    }
}

/// What became of a machine's try to rerun a [`CutOffJob`] as the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CutOffRerun {
    /// The forge reruns it.
    Rerun,
    /// Its run has yet to finish.
    Unfinished,
    /// It passed, or its run no longer has it, so it needs no rerun.
    Settled,
    /// The machine has no login of the user's for the run's forge.
    NoLogin,
}
