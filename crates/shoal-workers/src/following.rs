//! Following an operation the daemon runs for this process to its end. An
//! interrupt or termination cancels it.
use std::future::Future;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};

use crate::{
    client::{
        connection::CommandPaths,
        operations::{Followed, Following},
    },
    protocol::operations::{Ended, OperationRef, OperationSpec, Outcome, Progress, Step, Stream},
};

/// What shows an operation's steps while it is followed, such as a
/// spinner on a terminal.
pub trait Display {
    /// Show that the operation reached `step`.
    fn step(&mut self, step: Step);

    /// Erase what the display drew, before a line of the process's own.
    fn clear(&mut self);

    /// Draw the display's next frame once it is due; never completes for a
    /// display that draws none.
    fn tick(&mut self) -> impl Future<Output = ()>;
}

/// A display that shows nothing, for a process without a user to see it.
pub struct Quiet;

impl Display for Quiet {
    fn step(&mut self, _: Step) {}

    fn clear(&mut self) {}

    fn tick(&mut self) -> impl Future<Output = ()> {
        std::future::pending()
    }
}

/// Where an operation's output lines go.
#[derive(Clone, Copy)]
pub enum Lines {
    /// Where the process shows them, keeping stdout for the result with
    /// JSON output.
    Command,
    /// To standard error, since standard output carries what the process
    /// writes there.
    Aside,
}

/// The operation a process follows, and how it shows its lines.
pub struct Shown<'a> {
    pub operation: &'a OperationRef,
    pub lines: Lines,
    /// Whether the process prints JSON, which its lines leave to stdout.
    pub json: bool,
}

/// Start `spec` in this process's environment, without its secrets, or on
/// the machine of the space it runs on, and follow it to its end below
/// `display`, returning what it did. `json` says whether the process
/// prints JSON.
pub async fn run<T>(
    paths: &CommandPaths,
    json: bool,
    display: &mut impl Display,
    spec: OperationSpec,
) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    let env = crate::env::without_secrets();
    let space = spec.machine_space().cloned();
    let id = crate::client::operations::start(paths, spec, env).await?;
    follow_started(paths, json, display, OperationRef { id, space }).await
}

/// Follow `operation`, which the daemon started for a request of this
/// process, to its end below `display`, returning what it did.
pub async fn follow_started<T>(
    paths: &CommandPaths,
    json: bool,
    display: &mut impl Display,
    operation: OperationRef,
) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    let mut following = Following::open(paths, operation.clone(), 0).await?;
    let shown = Shown {
        operation: &operation,
        lines: Lines::Command,
        json,
    };
    match show(paths, shown, display, async || following.next().await).await? {
        Ended::Succeeded(outcome) => T::try_from(*outcome),
        Ended::Failed(failure) => Err(crate::operations::error(failure)),
    }
}

/// Show the output of the operation that `next` reports, its lines where
/// `shown` says, below `display`, until it ends, returning how it did. Each
/// report runs to its end while the display ticks, so a slow one is never
/// restarted. An interrupt or termination cancels the operation; a second
/// one stops following and leaves it running.
pub async fn show<E>(
    paths: &CommandPaths,
    shown: Shown<'_>,
    display: &mut impl Display,
    mut next: impl AsyncFnMut() -> Result<Followed<E>>,
) -> Result<E> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut cancelled = false;
    loop {
        let mut reported = std::pin::pin!(next());
        let followed = loop {
            tokio::select! {
                followed = &mut reported => break followed?,
                _ = interrupt.recv() => {
                    display.clear();
                    cancelled = cancel(paths, shown.operation, cancelled).await?;
                }
                _ = terminate.recv() => {
                    display.clear();
                    cancelled = cancel(paths, shown.operation, cancelled).await?;
                }
                () = display.tick() => {}
            }
        };
        match followed {
            Followed::Event(event) => show_progress(&shown, display, event.progress),
            Followed::Ended(ended) => return Ok(ended),
        }
    }
}

/// Cancel `operation`, or, once `cancelled`, stop following it.
async fn cancel(paths: &CommandPaths, operation: &OperationRef, cancelled: bool) -> Result<bool> {
    anyhow::ensure!(
        !cancelled,
        "stopped following operation {}, which continues",
        operation.id
    );
    crate::client::operations::cancel(paths, operation.clone()).await?;
    eprintln!("Cancelling; interrupt again to stop following and leave it running");
    Ok(true)
}

/// Show an output line where `shown` says, and a step on `display`.
fn show_progress(shown: &Shown<'_>, display: &mut impl Display, progress: Progress) {
    let (stream, line) = match progress {
        Progress::Step { step } => return display.step(step),
        Progress::Output { stream, line } => (stream, line),
    };
    display.clear();
    match (stream, shown.lines) {
        (Stream::Stdout, Lines::Command) if !shown.json => shoal_core::outln!("{line}"),
        _ => eprintln!("{line}"),
    }
}
