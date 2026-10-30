//! Operations from the CLI: the daemon starts the work in a worker of its
//! own. Failures come back typed, as the errors the command handled before.
mod worker;

use crate::{
    execution::SetupVerificationFailure,
    protocol::{RemoteError, operations::Failure},
};
pub(super) use worker::{Reporter, run as worker};

/// A failure the operation reported that no other error type carries.
#[derive(Debug)]
pub(super) struct Failed(pub Failure);

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.0 {
            Failure::SetupFailed { message }
            | Failure::HookFailed { message }
            | Failure::Refused { message, .. }
            | Failure::Failed { message } => f.write_str(message),
            Failure::SetupUnverified { exit_code } => SetupVerificationFailure {
                exit_code: *exit_code,
            }
            .fmt(f),
        }
    }
}

impl std::error::Error for Failed {}

/// `error` as the failure an operation reports.
fn failure(error: anyhow::Error) -> Failure {
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
