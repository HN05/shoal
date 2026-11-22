//! What GitLab supports.
use crate::capability::{self, Support, Supports};
use shoal_forge_types::{backends::gitlab::GitLab, capability::Capability};

impl Supports for GitLab {
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RunJobs => Support::Unsupported(None),
            Capability::MeaningSearch => Support::Unsupported(Some(capability::KEYWORD_SEARCH)),
            Capability::RevertPrs => Support::Unsupported(Some(capability::GIT_REVERT)),
            Capability::ForkPrs => Support::Unsupported(Some(
                "GitLab opens a fork's merge request in the fork project, so open it on GitLab",
            )),
            Capability::Forks => Support::Unsupported(Some(
                "Shoal cannot open merge requests from a GitLab fork, so it does not make one",
            )),
            Capability::RebaseMerge => Support::Unsupported(Some(
                "GitLab merges with the project's merge method, so use merge or squash",
            )),
            Capability::UpdateByMerge => Support::Unsupported(Some(
                "GitLab only rebases a merge request's branch, so use rebase",
            )),
            // GitLab holds no pipeline for approval.
            Capability::ApproveRuns => Support::Unsupported(None),
            Capability::ResolveThreads
            | Capability::RerunJobs
            | Capability::CancelJobs
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
            | Capability::SquashMerge
            | Capability::AutoMerge
            | Capability::MergeAtHead
            | Capability::AutoMergeAtHead
            | Capability::UpdateByRebase
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges
            | Capability::Reactions => Support::Supported,
        }
    }
}
