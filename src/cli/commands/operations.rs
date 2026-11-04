//! Operations from the CLI: the daemon starts the work in a worker of its
//! own, and the command follows it, showing what its processes write as it
//! showed them when it ran them itself. Failures come back typed, as the
//! errors the command handled before.
mod worker;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};

use crate::{
    cli::context::Context,
    client::approvals::Undecided,
    client::operations::{Followed, Following},
    execution::SetupVerificationFailure,
    protocol::{
        RemoteError,
        operations::{Ended, Failure, OperationSpec, Outcome, Progress, Stream},
    },
};
pub(super) use worker::{Reporter, run as worker};

/// A failure the operation reported that no other error type carries.
#[derive(Debug)]
pub(crate) struct Failed(pub Failure);

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
    let id =
        crate::client::operations::start(&ctx.paths, spec, crate::env::without_secrets()).await?;
    follow_started(ctx, id).await
}

/// Follow operation `id`, which the daemon started for a request of this
/// command, to its end, returning what it did.
pub(super) async fn follow_started<T>(ctx: &Context, id: String) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    T::try_from(follow(ctx, id).await?)
}

/// Show operation `id`'s output until it ends, returning how it did. An
/// interrupt or termination cancels it, as it stopped the work when the
/// command ran it; a second one stops following and leaves it running.
async fn follow(ctx: &Context, id: String) -> Result<Outcome> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut following = Following::open(&ctx.paths, id.clone(), 0).await?;
    let mut cancelled = false;
    loop {
        let followed = tokio::select! {
            followed = following.next() => followed?,
            _ = interrupt.recv() => {
                cancelled = cancel(ctx, &id, cancelled).await?;
                continue;
            }
            _ = terminate.recv() => {
                cancelled = cancel(ctx, &id, cancelled).await?;
                continue;
            }
        };
        match followed {
            Followed::Event(event) => show(ctx, event.progress),
            Followed::Ended(Ended::Succeeded(outcome)) => return Ok(*outcome),
            Followed::Ended(Ended::Failed(failure)) => return Err(error(failure)),
        }
    }
}

/// Cancel operation `id`, or, once `cancelled`, stop following it.
async fn cancel(ctx: &Context, id: &str, cancelled: bool) -> Result<bool> {
    anyhow::ensure!(
        !cancelled,
        "stopped following operation {id}, which continues"
    );
    let id = id.to_owned();
    crate::client::operations::cancel(&ctx.paths, id).await?;
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
