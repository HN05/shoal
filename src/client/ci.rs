//! CI on a space's forge: listing, starting, rerunning, cancelling and
//! approving runs and checks, and reading check logs.
use std::collections::BTreeMap;

use anyhow::Result;

use crate::{
    client::connection::{Daemon, pages, request},
    forge::{
        ci::{
            CiActed, CiAction,
            approval::RunsApproved,
            checks::CheckLog,
            run::StartedRun,
            run_action::{Run, RunActed},
            run_view::RunFilter,
        },
        items::link::Selection,
    },
    protocol::{CiMethod, ItemContext},
};

/// The newest runs in `context`'s repository that `filter` selects, or those
/// on the head of the PR `pr`.
pub async fn runs(
    daemon: &impl Daemon,
    context: ItemContext,
    pr: Option<String>,
    filter: RunFilter,
) -> Result<Vec<Run>> {
    pages(
        daemon,
        CiMethod::ListCiRuns {
            context,
            pr,
            filter,
        },
    )
    .await
}

/// The run `run` names.
pub async fn run(daemon: &impl Daemon, context: Option<ItemContext>, run: String) -> Result<Run> {
    request(daemon, CiMethod::ViewCiRun { context, run }).await
}

/// Start `workflow`, or the only one, on `branch` with `inputs`.
pub async fn start(
    daemon: &impl Daemon,
    context: Option<ItemContext>,
    workflow: Option<String>,
    branch: Option<String>,
    inputs: BTreeMap<String, String>,
) -> Result<StartedRun> {
    let method = CiMethod::StartCiRun {
        context,
        workflow,
        branch,
        inputs,
    };
    request(daemon, method).await
}

/// Apply `action` to the run `run` names, only its failed jobs when `failed`.
pub async fn act_on_run(
    daemon: &impl Daemon,
    context: Option<ItemContext>,
    run: String,
    action: CiAction,
    failed: bool,
) -> Result<RunActed> {
    let method = CiMethod::CiRunAction {
        context,
        run,
        action,
        failed,
    };
    request(daemon, method).await
}

/// Apply `action` to the `checks` on the head of the PR `item`, by default
/// the linked one, or to all of them.
pub async fn act_on_checks(
    daemon: &impl Daemon,
    context: ItemContext,
    item: Option<String>,
    action: CiAction,
    checks: Vec<String>,
) -> Result<CiActed> {
    let method = CiMethod::CiAction {
        context,
        item,
        action,
        checks,
    };
    request(daemon, method).await
}

/// Approve the run `run` names, otherwise every waiting run on the head of
/// the PR `pr`.
pub async fn approve(
    daemon: &impl Daemon,
    context: ItemContext,
    run: Option<String>,
    pr: Option<String>,
) -> Result<RunsApproved> {
    request(daemon, CiMethod::ApproveCiRuns { context, run, pr }).await
}

/// The log of `check` on the item `selection` selects.
pub async fn check_log(
    daemon: &impl Daemon,
    context: ItemContext,
    selection: Selection,
    check: String,
) -> Result<CheckLog> {
    let method = CiMethod::CheckLog {
        context,
        selection,
        check,
    };
    request(daemon, method).await
}
