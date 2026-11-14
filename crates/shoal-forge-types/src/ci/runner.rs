//! The CI jobs waiting for a runner and the run each belongs to, which Shoal
//! runs on this machine with the forge's own runner.

use crate::items::{link::LinkKind, reference::ItemRef};
use shoal_refs::CommitId;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{
    ForgeRepo,
    numbers::{JobId, PullNumber, RunId},
};

/// A job waiting in the forge's queue for a runner with its labels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitingJob {
    /// What claims this attempt of the job and no other.
    pub handle: String,
    /// The job's ID, which grows with each job the forge queues.
    pub id: JobId,
    /// The API ID of the run it belongs to.
    pub run: RunId,
    pub name: String,
    /// The labels it runs on.
    pub labels: Vec<String>,
}

/// What started a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// PR `number`, at its head when the run started.
    Pull { number: PullNumber },
    /// A push to, or a run started on, `branch`.
    Branch(String),
    /// Anything else, such as a tag or a schedule.
    Other,
}

/// The run a job belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobRun {
    pub url: ItemRef,
    /// Its workflow file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    /// The commit it runs on.
    pub commit: CommitId,
    pub trigger: Trigger,
    /// The ref the repository's remote serves the commit under, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
}

impl JobRun {
    /// The PR a PR event started it for.
    pub fn pull(&self) -> Option<PullNumber> {
        match self.trigger {
            Trigger::Pull { number } => Some(number),
            Trigger::Branch(_) | Trigger::Other => None,
        }
    }

    /// Whether its PR, whose head is `head` now, moved past its commit.
    pub fn superseded_by(&self, head: &CommitId) -> bool {
        self.commit != *head
    }
}

impl ForgeRepo {
    /// The web URL of the run with API ID `id`, which a forge whose runner
    /// takes another run's job serves the run's page under.
    pub fn run_page(&self, id: RunId) -> Result<ItemRef> {
        ItemRef::numbered(self, LinkKind::Ci, id.get())
    }

    /// The web URL of the forge instance the repository is on.
    pub fn instance_url(&self) -> String {
        format!("{}://{}", self.web_scheme, self.host)
    }
}
