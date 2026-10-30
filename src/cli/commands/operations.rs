//! Operations from the CLI: the daemon starts the work in a worker of its
//! own, and the command follows it, showing what its processes write as it
//! showed them when it ran them itself. Failures come back typed, as the
//! errors the command handled before.
mod worker;

use anyhow::{Context as _, Result};
use tokio::{
    io::AsyncBufReadExt,
    signal::unix::{SignalKind, signal},
};

use crate::{
    cli::{client, context::Context},
    daemon::approvals::Undecided,
    execution::SetupVerificationFailure,
    protocol::{
        Body, OperationMethod, RemoteError, Response,
        operations::{Ended, Failure, OperationSpec, Outcome, Progress, Started, Stream},
    },
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

/// Start `spec` in this process's environment, without its secrets, and
/// follow it to its end, returning what it did.
pub(super) async fn run<T>(ctx: &Context, spec: OperationSpec) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    let method = OperationMethod::StartOperation {
        spec,
        env: Some(crate::env::without_secrets()),
    };
    let Started { id } = client::request(&ctx.paths, method).await?;
    T::try_from(follow(ctx, id).await?)
}

/// Show operation `id`'s output until it ends, returning how it did. An
/// interrupt or termination cancels it, as it stopped the work when the
/// command ran it; a second one stops following and leaves it running.
async fn follow(ctx: &Context, id: String) -> Result<Outcome> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let method = OperationMethod::FollowOperation {
        id: id.clone(),
        after: 0,
    };
    let (mut stream, body) = client::open_buffered(&ctx.paths, method).await?;
    <()>::try_from(body)?;
    let mut cancelled = false;
    // Kept across signals, which can arrive partway through a frame.
    let mut frame = Vec::new();
    loop {
        tokio::select! {
            read = stream.read_until(b'\n', &mut frame) => {
                read?;
            }
            _ = interrupt.recv() => {
                cancelled = cancel(ctx, &id, cancelled).await?;
                continue;
            }
            _ = terminate.recv() => {
                cancelled = cancel(ctx, &id, cancelled).await?;
                continue;
            }
        }
        let response = response(std::mem::take(&mut frame))?;
        match response.body.into_result()? {
            Body::OperationEvent(event) => show(ctx, event.progress),
            Body::OperationEnded(Ended::Succeeded(outcome)) => return Ok(*outcome),
            Body::OperationEnded(Ended::Failed(failure)) => return Err(error(failure)),
            body => return Err(body.unexpected("OperationEvent")),
        }
    }
}

/// The response a frame holds; one without its line ending means the daemon
/// went away.
fn response(frame: Vec<u8>) -> Result<Response> {
    anyhow::ensure!(
        frame.last() == Some(&b'\n'),
        "lost the daemon while following the operation, which continues"
    );
    serde_json::from_slice(&frame).context("invalid protocol message")
}

/// Cancel operation `id`, or, once `cancelled`, stop following it.
async fn cancel(ctx: &Context, id: &str, cancelled: bool) -> Result<bool> {
    anyhow::ensure!(
        !cancelled,
        "stopped following operation {id}, which continues"
    );
    let id = id.to_owned();
    client::request::<()>(&ctx.paths, OperationMethod::CancelOperation { id }).await?;
    eprintln!("Cancelling; interrupt again to stop following and leave it running");
    Ok(true)
}

/// Show an output line where the command showed it, keeping stdout for
/// the result with `--json`; steps show nothing.
fn show(ctx: &Context, progress: Progress) {
    match progress {
        Progress::Output {
            stream: Stream::Stdout,
            line,
        } if !ctx.json => outln!("{line}"),
        Progress::Output { line, .. } => eprintln!("{line}"),
        Progress::Step { .. } => {}
    }
}

/// `failure` as the error its command handles.
fn error(failure: Failure) -> anyhow::Error {
    match failure {
        Failure::SetupUnverified { exit_code } => SetupVerificationFailure { exit_code }.into(),
        Failure::Approval { request } => Undecided(request).into(),
        Failure::Refused { code, message } => RemoteError { code, message }.into(),
        failure => Failed(failure).into(),
    }
}

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
