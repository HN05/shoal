use crate::capability::{Support, Supports};
use shoal_forge_types::{backends::github::GitHub, capability::Capability};

impl Supports for GitHub {
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::AutoMergeAtHead => Support::Unsupported(Some(
                "GitHub keeps an automatic merge set after a push from an account with write access, and documents no check of the expected head after it sets one",
            )),
            Capability::RunJobs
            | Capability::ResolveThreads
            | Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::StartRuns
            | Capability::ReadRuns
            | Capability::ReadJobLogs
            | Capability::ApproveRuns
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
            | Capability::Reactions
            | Capability::RevertPrs
            | Capability::MeaningSearch => Support::Supported,
        }
    }
}
