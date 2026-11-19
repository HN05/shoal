//! Newline-delimited JSON over the daemon's Unix socket. Bump [`VERSION`]
//! whenever a request, response, or event changes shape or spelling.
mod agents;
mod alerts;
mod approvals;
mod apps;
mod assignments;
mod branches;
mod chats;
mod ci;
mod config;
mod containers;
mod daemon;
mod demos;
mod executions;
mod files;
mod forwarding;
mod frames;
mod holds;
mod hub;
mod items;
mod launches;
pub mod operations;
mod ports;
mod questions;
mod queue;
mod reads;
mod recovery;
mod removal;
mod repositories;
mod request;
mod resources;
mod response;
mod reviews;
mod secrets;
mod simulators;
mod status;
mod subscriptions;
pub mod timing;
mod triggers;
mod version;
mod workspaces;
mod wrapper;

pub use agents::{AgentMethod, SubagentChange, TurnEnd, WakeEnd, WokenTurn};
pub use alerts::{AlertMethod, AlertTarget, AlertsFor};
pub use approvals::ApprovalMethod;
pub use apps::AppMethod;
pub use assignments::{AssignmentMethod, SavedAgent};
pub use branches::BranchMethod;
pub use chats::{ChatMethod, ChatRoot};
pub use ci::CiMethod;
pub use config::{ConfigTarget, RepositoryLayers};
pub use containers::ContainerMethod;
pub use daemon::{DaemonMethod, Exit, ExitRequest, ExitWhen, Exited, Work, WorkPlace};
pub use demos::DemoMethod;
pub use executions::{ExecutionMethod, SessionSignal};
pub use files::FileMethod;
pub use forwarding::Forwarding;
pub use frames::{
    FrameTooLarge, MAX_FRAME, Responses, frame_pages, read, read_buffered, read_response,
    text_pieces, write,
};
pub use holds::HoldMethod;
pub use hub::{HubMethod, JoinVia};
pub use items::{DownloadPiece, ItemContext, ItemMethod, RepoTarget, ViewPiece};
pub use launches::{DetachedRun, LaunchMethod, Launched, Started};
pub use operations::OperationMethod;
pub use ports::PortMethod;
pub use questions::QuestionMethod;
pub use queue::{QueueMethod, RetryStart};
pub use recovery::{RecoveryMethod, SavedPiece};
pub use removal::{QueuedLaunches, RemovalMethod, UntrackedProcesses};
pub use repositories::RepositoryMethod;
pub use request::{Lifetime, Method, Operation, Reattachment, Request, name_of};
pub use resources::ResourceMethod;
pub use response::{Body, ErrorCode, RemoteError, Response};
pub use reviews::{ReviewMethod, ReviewPiece};
pub use secrets::SecretMethod;
pub use simulators::SimulatorMethod;
pub use status::{AccessStall, DaemonStatus, PRIVACY_PROMPT};
pub use subscriptions::SubscriptionMethod;
pub use triggers::TriggerMethod;
pub use version::VERSION;
pub use workspaces::{CommitStart, CopySource, CreationOptions, PullHead, WorkspaceMethod};
pub use wrapper::{
    Control, ExecutionEvent, RESTORE_WAITS, RESUME_NARROWING, Reattach, default_resumes_when,
};

#[cfg(test)]
mod tests;
