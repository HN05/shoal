//! Operations from the CLI: the daemon starts the work in a worker of its
//! own, and the command follows it, showing what its processes write as it
//! showed them when it ran them itself. Failures come back typed, as the
//! errors the command handled before.
use std::io::Write;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};

use crate::{
    cli::{context::Context, progress::Spinner},
    client::operations::{Followed, Following},
    client::preparation::{Preparing, Unprepared},
    paths::CommandPaths,
    protocol::operations::{Ended, OperationSpec, Outcome, Prepared, Progress, Step, Stream},
};

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

/// Follow operation `id` to its end, as [`show`] shows it, returning how it
/// did.
async fn follow(ctx: &Context, id: String) -> Result<Outcome> {
    let mut following = Following::open(&ctx.paths, id.clone(), 0).await?;
    match show(ctx, &id, async || following.next().await).await? {
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
    let id = preparing.operation().to_owned();
    show(ctx, &id, async || preparing.next().await).await
}

/// Show the output of operation `id` that `next` reports, below a spinner
/// naming its step, until it ends, returning how it did. Each report runs
/// to its end while the spinner ticks, so a slow one is never restarted. An
/// interrupt or termination cancels the operation, as it stopped the work
/// when the command ran it; a second one stops following and leaves it
/// running.
async fn show<E>(
    ctx: &Context,
    id: &str,
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
                    cancelled = cancel(ctx, id, cancelled).await?;
                }
                _ = terminate.recv() => {
                    spinner.clear();
                    cancelled = cancel(ctx, id, cancelled).await?;
                }
                () = spinner.tick() => {}
            }
        };
        match followed {
            Followed::Event(event) => show_progress(ctx, &mut spinner, event.progress),
            Followed::Ended(ended) => return Ok(ended),
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
/// the result with `--json`, and a step as the spinner's message.
fn show_progress(ctx: &Context, spinner: &mut Spinner<impl Write>, progress: Progress) {
    let (stream, line) = match progress {
        Progress::Step { step } => return spinner.set_message(doing(Some(step))),
        Progress::Output { stream, line } => (stream, line),
    };
    spinner.clear();
    match stream {
        Stream::Stdout if !ctx.json => outln!("{line}"),
        _ => eprintln!("{line}"),
    }
}

/// What an operation does after reporting `step`, or before its first.
fn doing(step: Option<Step>) -> &'static str {
    match step {
        None => "Starting",
        Some(Step::SetupStarted) => "Running setup",
        Some(Step::Ready | Step::PrLinked | Step::PostSetupEnded) => "Preparing workspace",
        Some(Step::PostSetupStarted) => "Running post-setup hook",
        Some(Step::PushStarted) => "Pushing",
        Some(Step::MergeStarted) => "Merging",
        Some(Step::UnmergeStarted) => "Undoing merge",
        Some(Step::AgentQueued | Step::Pushed) => "Finishing",
    }
}
