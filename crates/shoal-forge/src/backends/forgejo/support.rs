//! What Forgejo, and Gitea, support, release by release.
use anyhow::Result;

use super::version;
use crate::{
    capability::{self, Support, Supports},
    transport::account::Account,
};
use shoal_forge_types::{ForgeRepo, backends::forgejo::Forgejo, capability::Capability};

/// Why an older Forgejo, or Gitea, runs no jobs on Shoal's runner.
const RUNNER_RELEASE: &str = "that needs Forgejo 15 or later";

impl Supports for Forgejo {
    /// Shoal does not sign in to the web UI, where Forgejo resolves threads,
    /// and before release 17 reruns jobs, which [`Self::server_support`]
    /// asks the server about.
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RevertPrs => Support::Unsupported(Some(capability::GIT_REVERT)),
            Capability::ResolveThreads => Support::Unsupported(Some("reply in the thread instead")),
            Capability::RerunJobs => Support::Unsupported(Some(
                "that needs Forgejo 17 or later; rerun them from the web UI, push a fix, or start a new run with shoal ci run",
            )),
            // Forgejo approves runs only through a web UI route.
            Capability::ApproveRuns => Support::Unsupported(Some("approve them in the web UI")),
            Capability::RunJobs => Support::Unsupported(Some(RUNNER_RELEASE)),
            Capability::MeaningSearch => Support::Unsupported(Some(capability::KEYWORD_SEARCH)),
            Capability::AutoMergeAtHead => Support::Unsupported(Some(
                "Forgejo checks the head only when it sets an automatic merge, which then merges the head the PR has once checks pass",
            )),
            Capability::CancelJobs
            | Capability::StartRuns
            | Capability::ReadRuns
            | Capability::ReadJobLogs
            | Capability::ReopenPrs
            | Capability::Labels
            | Capability::IssueAssignees
            | Capability::PrAssignees
            | Capability::IssueMilestones
            | Capability::PrMilestones
            | Capability::DraftPrs
            | Capability::Attachments
            | Capability::Downloads
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::AutoMerge
            | Capability::MergeAtHead
            | Capability::UpdateByMerge
            | Capability::UpdateByRebase
            | Capability::ForkPrs
            | Capability::Forks
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges
            | Capability::Reactions => Support::Supported,
        }
    }

    /// Forgejo reruns runs and jobs through its API from release 17 and
    /// hands a runner a named job from release 15; Gitea does neither.
    async fn server_support(
        &self,
        repo: &ForgeRepo,
        account: &Account,
        capability: Capability,
    ) -> Result<Support> {
        let supported = match capability {
            Capability::RerunJobs => version::reruns(repo, account).await?,
            Capability::RunJobs => version::runs_jobs(repo, account).await?,
            _ => false,
        };
        if supported {
            return Ok(Support::Supported);
        }
        Ok(self.support(capability))
    }
}
