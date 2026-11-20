//! Requests that only read state, which the daemon cancels when their client
//! hangs up. Cancelling one that changes state could leave it half done, so
//! those finish instead, including reads that first finish or record work.
use super::{
    AgentMethod, AlertMethod, AppMethod, ApprovalMethod, BranchMethod, CiMethod, ContainerMethod,
    DaemonMethod, DemoMethod, ExecutionMethod, FileMethod, HoldMethod, HubMethod, ItemMethod,
    LaunchMethod, Operation, PortMethod, QueueMethod, RecoveryMethod, RemovalMethod,
    RepositoryMethod, ResourceMethod, SecretMethod, SimulatorMethod, ThreadMethod, TriggerMethod,
    WorkspaceMethod,
};

impl Operation {
    pub fn reads_only(&self) -> bool {
        matches!(
            self,
            Self::Daemon(DaemonMethod::Status)
                | Self::Agent(
                    AgentMethod::AgentUpdates { .. }
                        | AgentMethod::Progress { .. }
                        | AgentMethod::GitCredential { .. }
                )
                | Self::Approval(
                    ApprovalMethod::ListApprovals { .. } | ApprovalMethod::ShowApproval { .. }
                )
                | Self::Launch(
                    LaunchMethod::CheckLaunch { .. }
                        | LaunchMethod::CheckCreation { .. }
                        | LaunchMethod::LaunchModes { .. }
                )
                | Self::Ci(
                    CiMethod::CheckLog { .. }
                        | CiMethod::ViewCiRun { .. }
                        | CiMethod::CiRunJobs { .. }
                        | CiMethod::CiRunJobLog { .. }
                        | CiMethod::ListCiRuns { .. }
                )
                | Self::Branch(BranchMethod::ListForgeBranches { .. })
                | Self::Demo(DemoMethod::ListDemos { .. })
                | Self::Execution(
                    ExecutionMethod::CheckMerge
                        | ExecutionMethod::CheckUnmerge
                        | ExecutionMethod::ListAgents
                )
                | Self::File(FileMethod::ListFiles { .. })
                | Self::Workspace(WorkspaceMethod::SpaceLog { .. })
                | Self::Hold(HoldMethod::HoldList { .. })
                | Self::Item(
                    ItemMethod::ListItems { .. }
                        | ItemMethod::Labels { .. }
                        | ItemMethod::Reviewers { .. }
                        | ItemMethod::Capabilities { .. }
                        | ItemMethod::Reactions { .. }
                        | ItemMethod::ReviewThread { .. }
                        | ItemMethod::NumberKind { .. }
                        | ItemMethod::IssueDetails { .. }
                        | ItemMethod::IssueBlockers { .. }
                        | ItemMethod::PullRequest { .. }
                        | ItemMethod::MergePath { .. }
                        | ItemMethod::ViewItems { .. }
                        | ItemMethod::ForgeDownload { .. }
                )
                | Self::Alert(AlertMethod::ListAlerts { .. })
                | Self::Port(PortMethod::PortOverview { .. } | PortMethod::PortCatalog { .. })
                | Self::Thread(ThreadMethod::ListThreads { .. } | ThreadMethod::ShowThread { .. })
                | Self::Queue(QueueMethod::ListQueue | QueueMethod::QueuedPlan { .. })
                | Self::Recovery(
                    RecoveryMethod::Checkpoints { .. }
                        | RecoveryMethod::Saved { .. }
                        | RecoveryMethod::SavedSummaries
                )
                | Self::Removal(
                    RemovalMethod::CheckRemoval { .. }
                        | RemovalMethod::CheckStop { .. }
                        | RemovalMethod::RemovedSpaces
                        | RemovalMethod::RemovedSpace { .. }
                )
                | Self::Repository(
                    RepositoryMethod::ListRepositories
                        | RepositoryMethod::ListBranches { .. }
                        | RepositoryMethod::KeptBranches { .. }
                        | RepositoryMethod::AgentEnvironments { .. }
                )
                | Self::Resource(
                    ResourceMethod::ResourceOverview { .. }
                        | ResourceMethod::ResourceCatalog { .. }
                )
                | Self::Secret(SecretMethod::SecretCatalog { .. })
                | Self::Trigger(TriggerMethod::ListTriggers { .. })
                | Self::Hub(HubMethod::HubMembers | HubMethod::HubDirectory)
                | Self::App(
                    AppMethod::ReadAppConfig { .. }
                        | AppMethod::ReadAppEntry { .. }
                        | AppMethod::ListAppEntries { .. }
                        | AppMethod::WatchAppEntries { .. }
                )
                | Self::Container(
                    ContainerMethod::ContainerCatalog { .. }
                        | ContainerMethod::ContainerOverview { .. }
                )
                | Self::Simulator(
                    SimulatorMethod::SimCatalog { .. }
                        | SimulatorMethod::SimOverview { .. }
                        | SimulatorMethod::SimList { .. }
                        | SimulatorMethod::SimHistory { .. }
                )
                | Self::Workspace(
                    WorkspaceMethod::ListSpaces
                        | WorkspaceMethod::UntrackedWorktrees
                        | WorkspaceMethod::FindSpaces { .. }
                        | WorkspaceMethod::InspectWorkspace { .. }
                        | WorkspaceMethod::SpaceStatus { .. }
                        | WorkspaceMethod::TrackedConfig { .. }
                        | WorkspaceMethod::SpaceSessions { .. }
                        | WorkspaceMethod::FollowSession { .. }
                        | WorkspaceMethod::DiffBase { .. }
                        | WorkspaceMethod::Conflicts { .. }
                        | WorkspaceMethod::Diff { .. }
                        | WorkspaceMethod::LayeredConfig { .. }
                )
        )
    }
}

#[cfg(test)]
mod tests;
