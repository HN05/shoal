//! Newline-delimited JSON over the daemon's Unix socket. Bump [`VERSION`]
//! whenever a request, response, or event changes shape or spelling.
mod agents;
mod approvals;
mod assignments;
mod chats;
mod ci;
mod config;
mod containers;
mod daemon;
mod demos;
mod executions;
mod files;
mod frames;
mod holds;
mod items;
mod launches;
mod notifications;
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

pub use agents::{AgentMethod, TurnEnd, WakeEnd, WokenTurn};
pub use approvals::ApprovalMethod;
pub use assignments::{AssignmentMethod, SavedAgent};
pub use chats::{ChatMethod, ChatRoot, ChatStart};
pub use ci::CiMethod;
pub use config::{ConfigTarget, RepositoryLayers};
pub use containers::ContainerMethod;
pub use daemon::{DaemonMethod, Exit, ExitRequest, ExitWhen, Exited, Work, WorkPlace};
pub use demos::DemoMethod;
pub use executions::{ExecutionMethod, SessionSignal};
pub use files::FileMethod;
pub use frames::{
    FrameTooLarge, MAX_FRAME, Responses, frame_pages, read, read_buffered, read_response,
    text_pieces, write,
};
pub use holds::HoldMethod;
pub use items::{DownloadPiece, ItemContext, ItemMethod, RepoTarget, ViewPiece};
pub use launches::LaunchMethod;
pub use notifications::NotificationMethod;
pub use operations::OperationMethod;
pub use ports::PortMethod;
pub use questions::QuestionMethod;
pub use queue::QueueMethod;
pub use recovery::{RecoveryMethod, SavedPiece};
pub use removal::{QueuedLaunches, RemovalMethod, UntrackedProcesses};
pub use repositories::RepositoryMethod;
pub use request::{Lifetime, Method, Operation, Reattachment, Request};
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
    Control, ExecutionEvent, Protection, RESTORE_WAITS, RESUME_NARROWING, Reattach,
    default_resumes_when,
};

#[cfg(test)]
mod tests;
