//! The daemon's failures and refusals as client API errors.
use shoal_client::{ApiError, ErrorCode};

use super::records;
use crate::{
    daemon::{approvals::DecisionStatus, refusal::Reason},
    protocol::{self, Body},
};

/// A failed operation, coded by its refusal's reason.
pub(super) fn failed(error: &anyhow::Error) -> ApiError {
    let code = match Reason::of(error) {
        Some(Reason::NotFound) => ErrorCode::NotFound,
        Some(Reason::NameTaken) => ErrorCode::NameTaken,
        Some(Reason::NotReady) => ErrorCode::NotReady,
        Some(Reason::UnsavedWork) => ErrorCode::UnsavedWork,
        None => ErrorCode::OperationFailed,
    };
    ApiError::new(code, format!("{error:#}"))
}

/// A request the caller's scope refused, or whose target does not exist.
pub(super) fn scope_refused(error: &anyhow::Error) -> ApiError {
    match Reason::of(error) {
        Some(Reason::NotFound) => failed(error),
        _ => ApiError::new(ErrorCode::ScopeDenied, format!("{error:#}")),
    }
}

pub(super) fn invalid(message: impl std::fmt::Display) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest, message)
}

/// `body`, unless it is an error or a busy resource.
pub(super) fn answered(body: Body) -> Result<Body, ApiError> {
    match body {
        Body::Error { code, message } => Err(ApiError::new(
            match code {
                protocol::ErrorCode::InvalidRequest => ErrorCode::InvalidRequest,
                protocol::ErrorCode::ScopeDenied => ErrorCode::ScopeDenied,
                protocol::ErrorCode::NotFound => ErrorCode::NotFound,
                protocol::ErrorCode::NameTaken => ErrorCode::NameTaken,
                protocol::ErrorCode::NotReady => ErrorCode::NotReady,
                protocol::ErrorCode::UnsavedWork => ErrorCode::UnsavedWork,
                _ => ErrorCode::OperationFailed,
            },
            message,
        )),
        Body::Busy { message } => Err(ApiError::new(ErrorCode::Busy, message)),
        body => Ok(body),
    }
}

/// The granted allocation `body` holds, or the approval it waits on or
/// was denied.
pub(super) fn granted(body: Body) -> Result<Body, ApiError> {
    let Body::ApprovalRequest(request) = body else {
        return Ok(body);
    };
    let (code, message) = match request.status {
        DecisionStatus::Denied => (
            ErrorCode::ApprovalDenied,
            format!("approval request {} was denied", request.id),
        ),
        _ => (
            ErrorCode::ApprovalPending,
            format!("approval request {} waits for the user", request.id),
        ),
    };
    Err(ApiError {
        approval: Some(Box::new(records::approval(*request))),
        ..ApiError::new(code, message)
    })
}

/// A reply this layer cannot read, as an operation failure.
pub(super) fn unexpected(error: anyhow::Error) -> ApiError {
    ApiError::new(ErrorCode::OperationFailed, format!("{error:#}"))
}
