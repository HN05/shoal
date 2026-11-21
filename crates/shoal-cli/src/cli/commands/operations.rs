//! Operations from the CLI: the daemon starts the work in a worker of its
//! own, and the command follows it, showing what its processes write as it
//! showed them when it ran them itself. Failures come back typed, as the
//! errors the command handled before.
use std::io::Write;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};

use crate::{
    cli::{context::Context, progress::Spinner},
    client::connection::CommandPaths,
    client::{
        operations::{Followed, Following},
        preparation::{Preparing, Unprepared},
    },
    protocol::operations::{
        Ended, OperationRef, OperationSpec, Outcome, Prepared, Progress, Step, Stream,
    },
};

/// Start `spec` in this process's environment, without its secrets, and
/// follow it to its end, returning what it did.
pub(super) async fn run<T>(ctx: &Context, spec: OperationSpec) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    let id =
        crate::client::operations::start(&ctx.paths, spec, crate::env::without_secrets()).await?;
    follow_started(ctx, OperationRef::here(id)).await
}

/// Follow `operation`, which the daemon started for a request of this
/// command, to its end, returning what it did.
pub(super) async fn follow_started<T>(ctx: &Context, operation: OperationRef) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    T::try_from(follow(ctx, operation).await?)
}

/// Follow `operation` to its end, as [`show`] shows it, returning how it
/// did.
async fn follow(ctx: &Context, operation: OperationRef) -> Result<Outcome> {
    let mut following = Following::open(&ctx.paths, operation.clone(), 0).await?;
    let shown = (&operation, Lines::Command);
    match show(ctx, shown, async || following.next().await).await? {
        Ended::Succeeded(outcome) => Ok(*outcome),
        Ended::Failed(failure) => Err(crate::operations::error(failure)),
    }
}

/// Follow a workspace's preparation to its end, as [`show`] shows it, after
/// what assigning its issue on the forge warned of, returning the prepared
/// workspace or how preparing it failed.
pub(super) async fn follow_preparation(
    ctx: &Context,
    mut preparing: Preparing<'_, CommandPaths>,
) -> Result<Result<Prepared, Unprepared>> {
    super::links::print_warnings(preparing.warnings());
    let operation = preparing.operation().clone();
    let shown = (&operation, Lines::Command);
    show(ctx, shown, async || preparing.next().await).await
}

/// Where an operation's output lines go.
#[derive(Clone, Copy)]
pub(super) enum Lines {
    /// Where the command showed them, keeping stdout for the result with
    /// `--json`.
    Command,
    /// To standard error, since standard output carries what the command
    /// writes there.
    Aside,
}

/// Show the output of `operation` that `next` reports, its lines where
/// `lines` says, below a spinner naming its step, until it ends, returning
/// how it did. Each report runs
/// to its end while the spinner ticks, so a slow one is never restarted. An
/// interrupt or termination cancels the operation, as it stopped the work
/// when the command ran it; a second one stops following and leaves it
/// running.
pub(super) async fn show<E>(
    ctx: &Context,
    (operation, lines): (&OperationRef, Lines),
    mut next: impl AsyncFnMut() -> Result<Followed<E>>,
) -> Result<E> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut spinner = Spinner::stderr(ctx.json, doing(None));
    let mut cancelled = false;
    loop {
        let mut reported = std::pin::pin!(next());
        let followed = loop {
            tokio::select! {
                followed = &mut reported => break followed?,
                _ = interrupt.recv() => {
                    spinner.clear();
                    cancelled = cancel(ctx, operation, cancelled).await?;
                }
                _ = terminate.recv() => {
                    spinner.clear();
                    cancelled = cancel(ctx, operation, cancelled).await?;
                }
                () = spinner.tick() => {}
            }
        };
        match followed {
            Followed::Event(event) => show_progress(ctx, &mut spinner, (event.progress, lines)),
            Followed::Ended(ended) => return Ok(ended),
        }
    }
}

/// Cancel `operation`, or, once `cancelled`, stop following it.
async fn cancel(ctx: &Context, operation: &OperationRef, cancelled: bool) -> Result<bool> {
    anyhow::ensure!(
        !cancelled,
        "stopped following operation {}, which continues",
        operation.id
    );
    crate::client::operations::cancel(&ctx.paths, operation.clone()).await?;
    eprintln!("Cancelling; interrupt again to stop following and leave it running");
    Ok(true)
}

/// Show an output line where `lines` says, and a step as the spinner's
/// message.
fn show_progress(
    ctx: &Context,
    spinner: &mut Spinner<impl Write>,
    (progress, lines): (Progress, Lines),
) {
    let (stream, line) = match progress {
        Progress::Step { step } => return spinner.set_message(doing(Some(step))),
        Progress::Output { stream, line } => (stream, line),
    };
    spinner.clear();
    match (stream, lines) {
        (Stream::Stdout, Lines::Command) if !ctx.json => outln!("{line}"),
        _ => eprintln!("{line}"),
    }
}

/// What an operation does after reporting `step`, or before its first.
fn doing(step: Option<Step>) -> &'static str {
    match step {
        None => "Starting",
        Some(Step::WorktreeStarted) => "Creating worktree",
        Some(Step::SetupStarted) => "Running setup",
        Some(Step::Ready | Step::PrLinked | Step::PostSetupEnded) => "Preparing workspace",
        Some(Step::PostSetupStarted) => "Running post-setup hook",
        Some(Step::PushStarted) => "Pushing",
        Some(Step::MergeStarted) => "Merging",
        Some(Step::UnmergeStarted) => "Undoing merge",
        Some(Step::AgentQueued | Step::Pushed) => "Finishing",
        Some(Step::MovePacking) => "Stopping agents and packing",
        Some(Step::MoveDelivering) => "Delivering",
        Some(Step::MoveEnding) => "Ending the moved spaces here",
    }
}
