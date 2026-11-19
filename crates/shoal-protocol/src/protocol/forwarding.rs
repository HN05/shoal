//! Which requests a daemon forwards through the hub when the space they act
//! on belongs to another joined machine. One that reads or changes Shoal's
//! state goes to the space's machine, which decides it as its own caller's,
//! and one whose answer goes on, as a follow's, comes back on a stream, and
//! one whose connection carries bytes both ways, as a demo's, on a duplex
//! stream; one that needs the space's files or processes stays on that
//! machine.
use crate::{approvals::ApprovalRequest, item_numbers::ItemNumber, questions::Question};

use super::{
    AgentMethod, ApprovalMethod, AssignmentMethod, BranchMethod, CiMethod, DemoMethod, HoldMethod,
    ItemMethod, LaunchMethod, Operation, QuestionMethod, QueueMethod, RemovalMethod,
    RepositoryMethod, WorkspaceMethod,
};

/// What a daemon does with a request for another machine's space.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Forwarding {
    /// It needs the space's files or processes, so only the space's own
    /// machine runs it.
    Never,
    /// It goes to the space's machine, and fails at once while that machine
    /// is offline.
    Online,
    /// It goes to the space's machine, and waits at the hub while that
    /// machine is offline, as messages, answers, decisions and stops can.
    Queued,
    /// It goes to the space's machine, whose answer comes back on a stream
    /// as that machine writes it, as a follow's does; it fails at once while
    /// that machine is offline.
    Streamed,
    /// It goes to the space's machine, whose answer comes back on a stream
    /// as for [`Self::Streamed`], and after the answer's first frame the
    /// caller's bytes go there on the same stream, as a demo's and carried
    /// commits' do; it fails at once while that machine is offline.
    Duplex,
}

impl Operation {
    /// Leave out what belongs to the caller's machine before the request
    /// goes to another: an approval's environment, where the owning
    /// machine pushes in its daemon's own, and a launch's directory and
    /// terminal, where that machine starts the agent in a place of its own,
    /// marking the launch as one another machine forwarded.
    pub fn leave_caller_machine(&mut self) {
        match self {
            Self::Approval(ApprovalMethod::DecideApproval { env, .. }) => *env = None,
            Self::Launch(LaunchMethod::Launch { launch }) => {
                launch.directory = None;
                launch.env = None;
                launch.setup_env = None;
                launch.unplaceable = None;
                launch.forwarded.get_or_insert_default();
            }
            _ => {}
        }
    }

    /// Name the question or approval request a request answers, decides or
    /// passes on as `questions` and `approvals` hold it, where the caller
    /// named it by number alone, so that a later item reusing its number,
    /// as while its machine is offline, is refused instead.
    /// Whether the request answers, decides or passes on an item it names
    /// by number alone, which another item may take while it waits.
    pub fn names_unpinned_item(&self) -> bool {
        match self {
            Self::Question(
                QuestionMethod::AnswerQuestion { shown, .. }
                | QuestionMethod::AcceptQuestionRecommendation { shown, .. }
                | QuestionMethod::WithdrawQuestion { shown, .. },
            ) => shown.is_none(),
            Self::Approval(
                ApprovalMethod::DecideApproval { shown, .. }
                | ApprovalMethod::EscalateApproval { shown, .. },
            ) => shown.is_none(),
            _ => false,
        }
    }

    pub fn pin_item(&mut self, questions: &[Question], approvals: &[ApprovalRequest]) {
        let question =
            |number: &ItemNumber| questions.iter().find(|q| &q.number == number).cloned();
        let approval = |number: &ItemNumber| {
            (approvals.iter().find(|a| &a.number == number)).map(|a| Box::new(a.clone()))
        };
        match self {
            Self::Question(
                QuestionMethod::AnswerQuestion { number, shown, .. }
                | QuestionMethod::AcceptQuestionRecommendation { number, shown }
                | QuestionMethod::WithdrawQuestion { number, shown, .. },
            ) if shown.is_none() => *shown = question(number),
            Self::Approval(
                ApprovalMethod::DecideApproval { number, shown, .. }
                | ApprovalMethod::EscalateApproval { number, shown, .. },
            ) if shown.is_none() => *shown = approval(number),
            _ => {}
        }
    }

    pub fn forwarding(&self) -> Forwarding {
        if self.waits_for_machine() {
            Forwarding::Queued
        } else if matches!(
            self,
            Self::Workspace(WorkspaceMethod::FollowSession { .. } | WorkspaceMethod::Diff { .. })
        ) {
            Forwarding::Streamed
        } else if matches!(
            self,
            Self::Demo(DemoMethod::OpenDemoStream { .. })
                | Self::Repository(RepositoryMethod::ReceiveCommits { .. })
                | Self::Workspace(WorkspaceMethod::AttachSession { .. })
        ) {
            Forwarding::Duplex
        } else if self.forwards() {
            Forwarding::Online
        } else {
            Forwarding::Never
        }
    }

    /// The requests that may wait for an offline machine.
    fn waits_for_machine(&self) -> bool {
        matches!(
            self,
            Self::Agent(AgentMethod::SendAgentMessage { .. })
                | Self::Question(
                    QuestionMethod::AnswerQuestion { .. }
                        | QuestionMethod::AcceptQuestionRecommendation { .. }
                )
                | Self::Approval(ApprovalMethod::DecideApproval { .. })
                | Self::Removal(RemovalMethod::StopSpace { .. })
        )
    }

    /// The other requests that go to the space's machine.
    fn forwards(&self) -> bool {
        matches!(
            self,
            Self::Workspace(
                WorkspaceMethod::SpaceStatus { .. }
                    | WorkspaceMethod::SpaceLog { .. }
                    | WorkspaceMethod::RenameSpace { .. }
                    | WorkspaceMethod::Conflicts { .. }
            ) | Self::Agent(
                AgentMethod::SetProgress { .. }
                    | AgentMethod::Progress { .. }
                    | AgentMethod::Sleep { .. }
            ) | Self::Question(
                QuestionMethod::ListQuestions { .. }
                    | QuestionMethod::ShowQuestion { .. }
                    | QuestionMethod::WithdrawQuestion { .. }
            ) | Self::Approval(
                ApprovalMethod::ListApprovals { .. }
                    | ApprovalMethod::ShowApproval { .. }
                    | ApprovalMethod::EscalateApproval { .. }
                    | ApprovalMethod::WithdrawApproval { .. }
            ) | Self::Assignment(
                AssignmentMethod::MarkDone { .. }
                    | AssignmentMethod::UndoDone { .. }
                    | AssignmentMethod::ForgetStoppedWork { .. }
                    | AssignmentMethod::ChangeStoppedPermissions { .. }
            ) | Self::Removal(
                RemovalMethod::CheckStop { .. }
                    | RemovalMethod::CheckRemoval { .. }
                    | RemovalMethod::RemoveSpace { .. }
                    | RemovalMethod::StartRemoval { .. }
            ) | Self::Queue(QueueMethod::SetPriority { .. } | QueueMethod::QueueRestore { .. })
                | Self::Launch(LaunchMethod::Launch { .. })
                | Self::Demo(DemoMethod::ListDemos { .. })
                | Self::Hold(
                    HoldMethod::HoldAcquire { .. }
                        | HoldMethod::HoldRelease { .. }
                        | HoldMethod::HoldList { .. }
                )
                | Self::Item(
                    ItemMethod::LinkItem { .. }
                        | ItemMethod::UnlinkItems { .. }
                        | ItemMethod::ViewItems { .. }
                        | ItemMethod::ItemAction { .. }
                        | ItemMethod::SelectItems { .. }
                        | ItemMethod::OpenIssue { .. }
                        | ItemMethod::MergeWarning { .. }
                        | ItemMethod::MergePath { .. }
                        | ItemMethod::NumberKind { .. }
                )
                | Self::Ci(
                    CiMethod::CheckLog { .. }
                        | CiMethod::CiAction { .. }
                        | CiMethod::CiRunAction { .. }
                        | CiMethod::ViewCiRun { .. }
                        | CiMethod::CiRunJobs { .. }
                        | CiMethod::CiRunJobLog { .. }
                        | CiMethod::ListCiRuns { .. }
                        | CiMethod::StartCiRun { .. }
                )
                | Self::Branch(
                    BranchMethod::ListForgeBranches { .. }
                        | BranchMethod::DeleteForgeBranches { .. }
                )
        )
    }
}

#[cfg(test)]
mod tests;
