//! The worker the daemon starts for an operation: it reads its order on
//! standard input, does the work with the caller's scope, and reports its
//! steps and its end. What it and its processes write reaches the daemon
//! as the operation's output.
use anyhow::{Context as _, Result};

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

/// Reports the steps of one operation.
pub(in crate::cli::commands) struct Reporter<'a> {
    ctx: &'a Context,
    id: &'a str,
}

impl Reporter<'_> {
    pub(in crate::cli::commands) async fn step(&self, step: Step) -> Result<()> {
        self.report(Report::Step(step)).await
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
    let reporter = Reporter { ctx, id: &order.id };
    let ended = match work(ctx, order.spec, &reporter).await {
        Ok(outcome) => Ended::Succeeded(Box::new(outcome)),
        Err(error) => Ended::Failed(failure(error)),
    };
    reporter.report(Report::Ended(ended)).await?;
    Ok(0)
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
    }
}
