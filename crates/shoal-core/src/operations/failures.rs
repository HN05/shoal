//! An operation's typed failures, as the worker reports them and as the
//! errors its commands handled before.
use crate::{
    client::approvals::Undecided,
    execution::SetupVerificationFailure,
    protocol::{RemoteError, operations::Failure},
};

/// A failure the operation reported that no other error type carries.
#[derive(Debug)]
pub struct Failed(pub Failure);

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Failure::SetupFailed { message }
            | Failure::SetupNotStarted { message }
            | Failure::HookFailed { message }
            | Failure::PushFailed { message }
            | Failure::Refused { message, .. }
            | Failure::Failed { message } => f.write_str(message),
            Failure::SetupUnverified { exit_code } => SetupVerificationFailure {
                exit_code: *exit_code,
            }
            .fmt(f),
            Failure::Approval { request } => Undecided(request.clone()).fmt(f),
        }
    }
}

impl std::error::Error for Failed {}

/// `failure` as the error its command handles.
pub fn error(failure: Failure) -> anyhow::Error {
    match failure {
        Failure::SetupUnverified { exit_code } => SetupVerificationFailure { exit_code }.into(),
        Failure::Approval { request } => Undecided(request).into(),
        Failure::Refused { code, message } => RemoteError { code, message }.into(),
        failure => Failed(failure).into(),
    }
}

/// `error` as the failure an operation reports.
pub(super) fn failure(error: anyhow::Error) -> Failure {
    let error = match error.downcast::<Failed>() {
        Ok(Failed(failure)) => return failure,
        Err(error) => error,
    };
    let error = match error.downcast::<SetupVerificationFailure>() {
        Ok(failure) => {
            return Failure::SetupUnverified {
                exit_code: failure.exit_code,
            };
        }
        Err(error) => error,
    };
    let error = match error.downcast::<Undecided>() {
        Ok(Undecided(request)) => return Failure::Approval { request },
        Err(error) => error,
    };
    if let Some(RemoteError { code, message }) = error.downcast_ref::<RemoteError>() {
        return Failure::Refused {
            code: code.clone(),
            message: message.clone(),
        };
    }
    Failure::Failed {
        message: format!("{error:#}"),
    }
}

#[cfg(test)]
mod tests;
