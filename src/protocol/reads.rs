//! Requests that only read state, which the daemon cancels when their client
//! hangs up. Cancelling one that changes state could leave it half done, so
//! those finish instead.
use super::{
    AgentMethod, ApprovalMethod, ChatMethod, CiMethod, DaemonMethod, DemoMethod, ExecutionMethod,
    HoldMethod, ItemMethod, NotificationMethod, Operation, PortMethod, QuestionMethod, QueueMethod,
    RecoveryMethod, RemovalMethod, RepositoryMethod, ResourceMethod, SecretMethod, SimulatorMethod,
    WorkspaceMethod,
};

impl Operation {
    pub fn reads_only(&self) -> bool {
        matches!(
            self,
            Self::Daemon(DaemonMethod::Status)
                | Self::Agent(AgentMethod::AgentMessages { .. } | AgentMethod::AgentUpdates { .. })
                | Self::Approval(
                    ApprovalMethod::ListApprovals { .. } | ApprovalMethod::ShowApproval { .. }
                )
                | Self::Chat(
                    ChatMethod::CheckChatName { .. }
                        | ChatMethod::FreeChatName { .. }
                        | ChatMethod::ChatUpdates { .. }
                )
                | Self::Ci(
                    CiMethod::CheckLog { .. }
                        | CiMethod::ViewCiRun { .. }
                        | CiMethod::CiRunJobs { .. }
                        | CiMethod::CiRunJobLog { .. }
                        | CiMethod::ListCiRuns { .. }
                )
                | Self::Demo(DemoMethod::ListDemos { .. })
                | Self::Execution(ExecutionMethod::CheckLanding | ExecutionMethod::ListAgents)
                | Self::Hold(HoldMethod::HoldList { .. })
                | Self::Item(ItemMethod::ListItems { .. } | ItemMethod::ForgeType { .. })
                | Self::Notification(NotificationMethod::ListNotifications { .. })
                | Self::Port(PortMethod::PortOverview { .. } | PortMethod::PortCatalog { .. })
                | Self::Question(
                    QuestionMethod::ListQuestions { .. } | QuestionMethod::ShowQuestion { .. }
                )
                | Self::Queue(QueueMethod::ListQueue | QueueMethod::QueuedPlan { .. })
                | Self::Recovery(RecoveryMethod::Checkpoints { .. })
                | Self::Removal(
                    RemovalMethod::CheckRemoval { .. } | RemovalMethod::RemovedWorkspaces
                )
                | Self::Repository(
                    RepositoryMethod::ListRepositories
                        | RepositoryMethod::RepositoryConfig { .. }
                        | RepositoryMethod::PlanRepositoryMoves { .. }
                        | RepositoryMethod::ListBranches { .. }
                        | RepositoryMethod::KeptBranches { .. }
                        | RepositoryMethod::AgentEnvironments { .. }
                )
                | Self::Resource(
                    ResourceMethod::ResourceOverview { .. }
                        | ResourceMethod::ResourceCatalog { .. }
                )
                | Self::Secret(SecretMethod::SecretCatalog { .. })
                | Self::Simulator(
                    SimulatorMethod::SimCatalog { .. }
                        | SimulatorMethod::SimOverview { .. }
                        | SimulatorMethod::SimList { .. }
                        | SimulatorMethod::SimHistory { .. }
                )
                | Self::Workspace(
                    WorkspaceMethod::CheckCreation { .. }
                        | WorkspaceMethod::CheckDependencies { .. }
                        | WorkspaceMethod::ListWorkspaces
                        | WorkspaceMethod::UntrackedWorktrees
                        | WorkspaceMethod::FindWorkspaces { .. }
                        | WorkspaceMethod::InspectWorkspace { .. }
                        | WorkspaceMethod::WorkspaceStatus { .. }
                        | WorkspaceMethod::DiffBase { .. }
                        | WorkspaceMethod::Conflicts { .. }
                        | WorkspaceMethod::LayeredConfig { .. }
                )
        )
    }
}
