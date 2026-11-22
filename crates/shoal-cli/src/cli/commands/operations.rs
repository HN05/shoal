//! Operations from the CLI: the daemon starts the work in a worker of its
//! own, and the command follows it, showing what its processes write as it
//! showed them when it ran them itself. Failures come back typed, as the
//! errors the command handled before.
use std::io::Stderr;

use anyhow::Result;
use shoal_workers::following::{self, Display, Lines, Shown};

use crate::{
    cli::{context::Context, progress::Spinner},
    client::operations::Followed,
    protocol::operations::{OperationRef, OperationSpec, Outcome, Step},
};

/// Start `spec` in this process's environment, without its secrets, and
/// follow it to its end, returning what it did.
pub(super) async fn run<T>(ctx: &Context, spec: OperationSpec) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    following::run(&ctx.paths, ctx.json, &mut steps(ctx), spec).await
}

/// Follow `operation`, which the daemon started for a request of this
/// command, to its end, returning what it did.
pub(super) async fn follow_started<T>(ctx: &Context, operation: OperationRef) -> Result<T>
where
    T: TryFrom<Outcome, Error = anyhow::Error>,
{
    following::follow_started(&ctx.paths, ctx.json, &mut steps(ctx), operation).await
}

/// Show the output of `operation` that `next` reports, its lines where
/// `lines` says, below a spinner naming its step, until it ends, returning
/// how it did, as [`following::show`] follows it.
pub(super) async fn show<E>(
    ctx: &Context,
    (operation, lines): (&OperationRef, Lines),
    next: impl AsyncFnMut() -> Result<Followed<E>>,
) -> Result<E> {
    let shown = Shown {
        operation,
        lines,
        json: ctx.json,
    };
    following::show(&ctx.paths, shown, &mut steps(ctx), next).await
}

/// The spinner of an operation this command follows.
pub(super) fn steps(ctx: &Context) -> Steps {
    Steps(Spinner::stderr(ctx.json, doing(None)))
}

/// A spinner naming what the operation does.
pub(super) struct Steps(Spinner<Stderr>);

impl Display for Steps {
    fn step(&mut self, step: Step) {
        self.0.set_message(doing(Some(step)));
    }

    fn clear(&mut self) {
        self.0.clear();
    }

    fn tick(&mut self) -> impl Future<Output = ()> {
        self.0.tick()
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
