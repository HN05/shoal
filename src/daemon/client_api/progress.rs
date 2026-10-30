//! An operation's progress, result and failure as the client API's.
use shoal_client::{
    ApiError, ErrorCode, Landed, OpenedPr, OperationEvent, OperationResult, OutputStream, Progress,
    Step, wire::Reply,
};

use super::{Session, agents, errors, records};
use crate::{
    daemon::links::proposals::OpenOutcome,
    model::LandedBranch,
    protocol::{
        Body,
        operations::{self as internal, Ended, Failure, Outcome, Prepared},
    },
};

pub(super) fn event(event: internal::OperationEvent) -> OperationEvent {
    OperationEvent {
        index: event.index,
        progress: match event.progress {
            internal::Progress::Step { step } => Progress::Step {
                step: Step::from(step.as_str()),
            },
            internal::Progress::Output { stream, line } => Progress::Output {
                stream: OutputStream::from(stream.as_str()),
                line,
            },
        },
    }
}

/// The reply that ends following an operation that ended as `ended`.
pub(super) async fn ended(session: &Session, ended: Ended) -> Reply {
    let result = match ended {
        Ended::Succeeded(outcome) => result(session, *outcome).await,
        Ended::Failed(failure) => Err(error(failure)),
    };
    match result.and_then(|result| serde_json::to_value(result).map_err(unreadable)) {
        Ok(result) => Reply::Result(result),
        Err(error) => Reply::Error(error),
    }
}

async fn result(session: &Session, outcome: Outcome) -> Result<OperationResult, ApiError> {
    Ok(match outcome {
        Outcome::Prepared(Prepared { workspace, agent }) => {
            let manager = &session.server.manager;
            let agents = agents::load(manager, std::slice::from_ref(&*workspace))
                .await
                .map_err(|error| errors::failed(&error))?;
            OperationResult::Prepared {
                workspace: Box::new(records::workspace(&workspace, &agents)),
                agent: agent.map(records::queued_agent),
            }
        }
        Outcome::Opened(OpenOutcome::Opened(opened)) => OperationResult::PrOpened(OpenedPr {
            url: opened.item.url,
            number: opened.item.number,
            created: opened.created,
            linked: opened.linked,
            warnings: opened.warnings,
        }),
        Outcome::Opened(OpenOutcome::Proposed(proposed)) => OperationResult::PrProposed {
            approval: Box::new(records::approval(*proposed.proposed)),
        },
        Outcome::Landed(landed) => OperationResult::Landed(self::landed(landed)),
    })
}

fn landed(landed: LandedBranch) -> Landed {
    Landed {
        branch: landed.branch,
        default_branch: landed.default_branch,
        previous_commit: landed.previous_commit,
        commit: landed.commit,
        updated: landed.updated,
        fast_forward: landed.fast_forward,
        pushed_to: landed.pushed_to,
    }
}

/// `failure` as the typed error a client acts on.
fn error(failure: Failure) -> ApiError {
    match failure {
        Failure::SetupFailed { message } => ApiError::new(ErrorCode::SetupFailed, message),
        Failure::SetupUnverified { exit_code } => ApiError::new(
            ErrorCode::SetupFailed,
            format!(
                "setup command exited with status {exit_code}, but Shoal could not verify that all processes stopped; stop them, then retry setup"
            ),
        ),
        Failure::HookFailed { message } => ApiError::new(ErrorCode::HookFailed, message),
        Failure::PushFailed { message } => ApiError::new(ErrorCode::PushFailed, message),
        Failure::Approval { request } => match errors::granted(Body::ApprovalRequest(request)) {
            Err(error) => error,
            Ok(_) => ApiError::new(ErrorCode::OperationFailed, "the approval was granted"),
        },
        Failure::Refused { code, message } => match errors::answered(Body::error(code, message)) {
            Err(error) => error,
            Ok(_) => ApiError::new(ErrorCode::OperationFailed, "the operation was refused"),
        },
        Failure::Failed { message } => ApiError::new(ErrorCode::OperationFailed, message),
    }
}

fn unreadable(error: serde_json::Error) -> ApiError {
    errors::unexpected(error.into())
}
