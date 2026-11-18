//! Which requests a daemon forwards through the hub when the space they act
//! on belongs to another joined machine. One that reads or changes Shoal's
//! state goes to the space's machine, which decides it as its own caller's;
//! one that needs the space's files or processes stays on that machine.
use super::{
    AgentMethod, ApprovalMethod, AssignmentMethod, CiMethod, HoldMethod, ItemMethod, Operation,
    QuestionMethod, QueueMethod, RemovalMethod, WorkspaceMethod,
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
}

impl Operation {
    pub fn forwarding(&self) -> Forwarding {
        if self.waits_for_machine() {
            Forwarding::Queued
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
        )
    }
}
