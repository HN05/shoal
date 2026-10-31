//! The worker the daemon starts for an operation: it reads its order on
//! standard input, does the work with the caller's scope, and reports its
//! steps and its end. What it and its processes write reaches the daemon
//! as the operation's output.
use std::io::Write as _;

use anyhow::{Context as _, Result, ensure};
use tokio::{
    signal::unix::{SignalKind, signal},
    sync::watch,
};

use super::failure;
use crate::{
    cli::{
        client,
        commands::{items, workspaces},
        context::Context,
    },
    protocol::{
        OperationMethod,
        operations::{Ended, OperationSpec, Order, Outcome, Report, Step},
    },
};

/// Reports the steps of one operation, and whether it was cancelled.
pub(in crate::cli::commands) struct Reporter<'a> {
    ctx: &'a Context,
    id: &'a str,
    cancelled: watch::Receiver<bool>,
}

impl Reporter<'_> {
    /// Report `step`, which starts only while the operation is not cancelled.
    pub(in crate::cli::commands) async fn step(&self, step: Step) -> Result<()> {
        self.ensure_running()?;
        self.report(Report::Step(step)).await
    }

    /// Report `step`, a change already made, which a cancel arriving
    /// meanwhile does not take back.
    pub(in crate::cli::commands) async fn reached(&self, step: Step) -> Result<()> {
        self.report(Report::Step(step)).await
    }

    /// Refuse to go on once the operation is cancelled, as before a change
    /// that cannot be taken back.
    pub(in crate::cli::commands) fn ensure_running(&self) -> Result<()> {
        ensure!(!*self.cancelled.borrow(), "the operation was cancelled");
        Ok(())
    }

    /// Complete once the operation is cancelled.
    pub(in crate::cli::commands) async fn cancelled(&self) {
        let mut cancelled = self.cancelled.clone();
        let _ = cancelled.wait_for(|cancelled| *cancelled).await;
    }

    async fn report(&self, report: Report) -> Result<()> {
        let method = OperationMethod::ReportProgress {
            id: self.id.to_owned(),
            report,
        };
        client::request(&self.ctx.paths, method).await
    }
}

pub(in crate::cli::commands) async fn run(ctx: &Context) -> Result<i32> {
    let order: Order =
        serde_json::from_reader(std::io::stdin().lock()).context("read the operation's order")?;
    let reporter = Reporter {
        ctx,
        id: &order.id,
        cancelled: cancellation()?,
    };
    let ended = match work(ctx, order.spec, &reporter).await {
        Ok(outcome) => Ended::Succeeded(Box::new(outcome)),
        Err(error) => Ended::Failed(failure(error)),
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

async fn work(ctx: &Context, spec: OperationSpec, reporter: &Reporter<'_>) -> Result<Outcome> {
    match spec {
        OperationSpec::Prepare { workspace, steps } => {
            workspaces::prepare(ctx, workspace, steps, reporter)
                .await
                .map(Outcome::Prepared)
        }
        OperationSpec::OpenPull { workspace, options } => {
            items::push_and_open(ctx, workspace, options, reporter)
                .await
                .map(Outcome::Opened)
        }
        OperationSpec::Land { workspace, push } => {
            workspaces::land_branch(ctx, workspace, push, reporter)
                .await
                .map(Outcome::Landed)
        }
    }
}
