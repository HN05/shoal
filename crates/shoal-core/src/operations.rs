//! The worker the daemon starts for an operation: it reads its order on
//! standard input, does the work with the caller's scope, and reports its
//! steps and its end. What it and its processes write reaches the daemon
//! as the operation's output.
mod failures;
mod merge;
mod open_pull;
mod prepare;
mod push;
mod unmerge;

use std::io::Write as _;

use anyhow::{Context as _, Result, ensure};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
};

use crate::{
    paths::CommandPaths,
    protocol::operations::{Ended, OperationSpec, Order, Outcome, Report, Step},
};
pub use failures::{Failed, error};

/// Reports the steps of one operation, and whether it was cancelled.
struct Reporter<'a> {
    paths: &'a CommandPaths,
    id: &'a str,
    cancelled: watch::Receiver<bool>,
}

impl Reporter<'_> {
    /// Report `step`, which starts only while the operation is not cancelled.
    async fn step(&self, step: Step) -> Result<()> {
        self.ensure_running()?;
        self.report(Report::Step(step)).await
    }

    /// Report `step`, a change already made, which a cancel arriving
    /// meanwhile does not take back.
    async fn reached(&self, step: Step) -> Result<()> {
        self.report(Report::Step(step)).await
    }

    /// Refuse to go on once the operation is cancelled, as before a change
    /// that cannot be taken back.
    fn ensure_running(&self) -> Result<()> {
        ensure!(!*self.cancelled.borrow(), "the operation was cancelled");
        Ok(())
    }

    /// Complete once the operation is cancelled.
    async fn cancelled(&self) {
        let mut cancelled = self.cancelled.clone();
        let _ = cancelled.wait_for(|cancelled| *cancelled).await;
    }

    async fn report(&self, report: Report) -> Result<()> {
        crate::client::operations::report(self.paths, self.id.to_owned(), report).await
    }
}

/// Do the operation whose order arrives on standard input, and report its end.
pub async fn run(paths: &CommandPaths) -> Result<i32> {
    let order: Order =
        serde_json::from_reader(std::io::stdin().lock()).context("read the operation's order")?;
    let reporter = Reporter {
        paths,
        id: &order.id,
        cancelled: cancellation()?,
    };
    let ended = match work(paths, order.spec, &reporter).await {
        Ok(outcome) => Ended::Succeeded(Box::new(outcome)),
        Err(error) => Ended::Failed(failures::failure(error)),
    };
    reporter.report(Report::Ended(ended)).await?;
    Ok(0)
}

/// Cancel this process's work once the daemon cancels the operation, which
/// it does with SIGTERM, or the worker is interrupted: tracked commands
/// stop, and the steps between them stop here.
fn cancellation() -> Result<watch::Receiver<bool>> {
    let mut terminate = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    tokio::spawn(async move {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
        }
        crate::execution::cancel();
        // The operation shows this as its output, once nothing starts anew.
        let _ = writeln!(std::io::stderr(), "Operation cancelled; stopping its work");
    });
    Ok(crate::execution::cancellation())
}

async fn work(
    paths: &CommandPaths,
    spec: OperationSpec,
    reporter: &Reporter<'_>,
) -> Result<Outcome> {
    match spec {
        OperationSpec::Prepare { workspace, steps } => {
            prepare::prepare(paths, workspace, steps, reporter)
                .await
                .map(Outcome::Prepared)
        }
        OperationSpec::OpenPull { workspace, options } => {
            open_pull::push_and_open(paths, workspace, options, reporter)
                .await
                .map(Outcome::Opened)
        }
        OperationSpec::Merge { workspace, options } => {
            merge::merge_branch(paths, workspace, options, reporter)
                .await
                .map(Outcome::Merged)
        }
        OperationSpec::Unmerge { workspace } => unmerge::unmerge_branch(paths, workspace, reporter)
            .await
            .map(Outcome::Unmerged),
        OperationSpec::Push {
            workspace,
            approval,
            push,
        } => push::push_approved(paths, workspace, approval, push, reporter)
            .await
            .map(Outcome::Decided),
    }
}
