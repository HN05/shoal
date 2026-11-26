//! The start of a workspace's worker that the agent queue takes.
use crate::space_names::SpaceSelector;

/// The worker of an existing workspace for the daemon to queue and start
/// detached: what its launch asks for, what it waits for and the PR it
/// links as it starts.
pub struct WorkerStart {
    pub workspace: SpaceSelector,
    pub launch: crate::protocol::operations::LaunchRequest,
    pub blockers: Vec<crate::progress::Blocker>,
    pub pr: Option<crate::forge::items::reference::ItemInput>,
}
