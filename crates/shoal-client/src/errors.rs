//! The daemon's refusals, by code, and what can go wrong reaching it.
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{open_enum::open_enum, records::Approval};

open_enum! {
    /// Why the daemon refused a request. Clients act on the code; the
    /// message is for people.
    ErrorCode {
        /// The request is malformed, or names a method this daemon does not serve.
        InvalidRequest => "invalid_request",
        /// The daemon does not serve the client's major API version.
        UnsupportedVersion => "unsupported_version",
        /// The workspace, repository, link, question or approval does not exist.
        NotFound => "not_found",
        /// A workspace, an unfinished rename or a running chat has the name or branch.
        NameTaken => "name_taken",
        /// The caller's workspace scope does not allow the request.
        ScopeDenied => "scope_denied",
        /// The request waits for the user's approval, which `approval` names.
        ApprovalPending => "approval_pending",
        /// The user denied the approval `approval` names.
        ApprovalDenied => "approval_denied",
        /// A resource the request needs is taken.
        Busy => "busy",
        /// The workspace is not ready, or its worktree is not verified as its own.
        NotReady => "not_ready",
        /// Removal would delete unsaved work the request did not allow.
        UnsavedWork => "unsaved_work",
        /// The workspace's setup command failed; the workspace is kept as failed.
        SetupFailed => "setup_failed",
        /// The post-setup hook failed; the ready workspace is kept.
        HookFailed => "hook_failed",
        /// Pushing the workspace's branch failed.
        PushFailed => "push_failed",
        /// Any other failure; the message says what.
        OperationFailed => "operation_failed",
    }
}

/// A refusal from the daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    /// The approval request of an `approval_pending` or `approval_denied` refusal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Box<Approval>>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl fmt::Display) -> Self {
        Self {
            code,
            message: message.to_string(),
            approval: None,
        }
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

/// Why a call or subscription failed.
#[derive(Debug)]
pub enum Error {
    /// The daemon refused the request.
    Api(ApiError),
    /// The socket could not be reached, read or written.
    Io(std::io::Error),
    /// The daemon closed the connection, as it does when it stops or restarts.
    Closed,
    /// The daemon sent something this version cannot read.
    Protocol(String),
}

impl Error {
    /// The daemon's error code, when it refused the request.
    pub fn code(&self) -> Option<&ErrorCode> {
        match self {
            Self::Api(error) => Some(&error.code),
            _ => None,
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api(error) => error.fmt(f),
            Self::Io(error) => write!(f, "cannot reach the Shoal daemon: {error}"),
            Self::Closed => f.write_str("the Shoal daemon closed the connection"),
            Self::Protocol(message) => {
                write!(f, "unreadable reply from the Shoal daemon: {message}")
            }
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Api(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Closed | Self::Protocol(_) => None,
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset => Self::Closed,
            _ => Self::Io(error),
        }
    }
}

impl From<ApiError> for Error {
    fn from(error: ApiError) -> Self {
        Self::Api(error)
    }
}
