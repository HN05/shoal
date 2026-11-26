use crate::capability::{self, Support, Supports};
use shoal_forge_types::{backends::bitbucket::Bitbucket, capability::Capability};

impl Supports for Bitbucket {
    /// Shoal starts Pipelines but reads none of their jobs.
    fn support(&self, capability: Capability) -> Support {
        match capability {
            Capability::RunJobs => Support::Unsupported(None),
            Capability::MeaningSearch => Support::Unsupported(Some(capability::KEYWORD_SEARCH)),
            Capability::ReopenPrs => Support::Unsupported(Some(
                "Bitbucket cannot reopen a declined pull request; open a new one",
            )),
            Capability::RevertPrs => Support::Unsupported(Some(capability::GIT_REVERT)),
            Capability::Labels => Support::Unsupported(Some("Bitbucket has no labels")),
            Capability::PrAssignees => {
                Support::Unsupported(Some("Bitbucket pull requests have no assignees"))
            }
            Capability::PrMilestones => {
                Support::Unsupported(Some("Bitbucket pull requests have no milestones"))
            }
            Capability::Attachments => {
                Support::Unsupported(Some("Bitbucket has no API for attaching files"))
            }
            Capability::RerunJobs
            | Capability::CancelJobs
            | Capability::ReadJobLogs
            | Capability::ReadRuns
            | Capability::ApproveRuns => Support::Unsupported(None),
            Capability::Downloads => Support::Unsupported(Some(
                "Bitbucket serves images and attached files only to a signed-in browser",
            )),
            Capability::Reactions => Support::Unsupported(Some("Bitbucket has no reactions")),
            Capability::AutoMerge => {
                Support::Unsupported(Some("Bitbucket's API cannot merge a pull request later"))
            }
            Capability::MergeAtHead | Capability::AutoMergeAtHead => Support::Unsupported(Some(
                "Bitbucket merges a pull request at whatever head it has when it merges",
            )),
            Capability::UpdateByMerge | Capability::UpdateByRebase => Support::Unsupported(Some(
                "Bitbucket has no API for updating a PR's branch, so update it with Git and push it",
            )),
            Capability::ResolveThreads
            | Capability::IssueAssignees
            | Capability::IssueMilestones
            | Capability::DraftPrs
            | Capability::MergeCommit
            | Capability::RebaseMerge
            | Capability::SquashMerge
            | Capability::ForkPrs
            | Capability::StartRuns
            | Capability::LineComments
            | Capability::ApprovePrs
            | Capability::RequestChanges => Support::Supported,
            Capability::Forks => Support::Unsupported(Some(
                "fork it on Bitbucket, add the fork as a remote and set remote.pushDefault to it",
            )),
        }
    }
}
