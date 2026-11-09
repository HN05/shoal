//! Newline-delimited JSON over the daemon's Unix socket. Bump [`VERSION`]
//! whenever a request, response, or event changes shape or spelling.
mod agents;
mod approvals;
mod assignments;
mod chats;
mod ci;
mod config;
mod daemon;
mod demos;
mod executions;
mod files;
mod frames;
mod holds;
mod items;
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
mod version;
mod workspaces;
mod wrapper;

pub use agents::{AgentMethod, TurnEnd, WakeEnd, WokenTurn};
pub use approvals::ApprovalMethod;
pub use assignments::AssignmentMethod;
pub use chats::{ChatMethod, ChatRoot, ChatScope};
pub use ci::CiMethod;
pub use config::ConfigTarget;
pub use daemon::{DaemonMethod, Exit, ExitRequest};
pub use demos::DemoMethod;
pub use executions::ExecutionMethod;
pub use files::FileMethod;
pub use frames::{
    FrameTooLarge, MAX_FRAME, Responses, frame_pages, read, read_buffered, read_response,
    text_pieces, write,
};
pub use holds::HoldMethod;
pub use items::{DownloadPiece, ItemContext, ItemMethod, ViewPiece};
pub use notifications::NotificationMethod;
pub use operations::OperationMethod;
pub use ports::PortMethod;
pub use questions::QuestionMethod;
pub use queue::QueueMethod;
pub use recovery::RecoveryMethod;
pub use removal::{QueuedLaunches, RemovalMethod};
pub use repositories::RepositoryMethod;
pub use request::{Lifetime, Method, Operation, Reattachment, Request};
pub use resources::ResourceMethod;
pub use response::{Body, ErrorCode, RemoteError, Response};
pub use reviews::{ReviewMethod, ReviewPiece};
pub use secrets::SecretMethod;
pub use simulators::SimulatorMethod;
pub use status::{AccessStall, DaemonStatus};
pub use subscriptions::SubscriptionMethod;
pub use version::VERSION;
pub use workspaces::{CommitStart, CopySource, CreationOptions, PullHead, WorkspaceMethod};
pub use wrapper::{
    Control, ExecutionEvent, Protection, RESTORE_WAITS, Reattach, default_resumes_when,
};

#[cfg(test)]
mod tests;
