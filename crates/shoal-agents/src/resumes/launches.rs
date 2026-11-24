//! A space whose agent's launch failed to start resumes by queuing that
//! launch again exactly as it was, with the permission change made first,
//! as the queue admits it or at once.
use anyhow::Result;

use crate::Agents;
use crate::daemon::Daemon;
use crate::model::{AgentResume, QueueEntry, ResumeOutcome, Space};
use crate::stopped_work::PermissionTargets;
use shoal_daemon_base::callers::Caller;
use shoal_protocol::protocol::RetryStart;
use shoal_protocol::space_names::SpaceSelector;

impl Agents {
    /// Queue the failed `launch` of `space` again for `caller`, as `resume`
    /// says.
    pub(super) async fn retry_launch(
        &self,
        daemon: &dyn Daemon,
        caller: Option<&Caller>,
        space: &Space,
        launch: QueueEntry,
        resume: &AgentResume,
    ) -> Result<ResumeOutcome> {
        let selector = SpaceSelector::id(&space.id);
        if let Some(change) = &resume.change {
            let targets = PermissionTargets {
                agents: &[],
                launches: std::slice::from_ref(&selector),
            };
            (self.change_stopped_permissions(daemon, targets, change, caller)).await?;
        }
        let start = match resume.now {
            true => RetryStart::Now,
            false => RetryStart::Queued,
        };
        let entry = self.retry_failed_launch(daemon, &launch.id, start).await?;
        // A resume the user makes is their interaction with its agent.
        (self.record_user_interaction(daemon, caller, &selector)).await;
        Ok(ResumeOutcome::Queued {
            entries: vec![entry],
            commands: Vec::new(),
        })
    }
}
