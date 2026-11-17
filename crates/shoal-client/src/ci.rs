//! CI on a space's forge: listing, starting, rerunning, cancelling and
//! approving runs and checks, and reading check logs.
use crate::forge::items::reference::ItemInput;
use std::collections::BTreeMap;

use anyhow::Result;

use crate::{
    connection::{Daemon, pages, request},
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
};
use shoal_protocol::protocol::{CiMethod, RepoTarget};

/// The newest runs in the `target` repository that `filter` selects, or
/// those on the head of the PR `pr`.
pub async fn runs(
    daemon: &impl Daemon,
    target: RepoTarget,
    pr: Option<ItemInput>,
    filter: RunFilter,
) -> Result<Vec<Run>> {
    pages(daemon, CiMethod::ListCiRuns { target, pr, filter }).await
}

/// The run `run` names.
pub async fn run(daemon: &impl Daemon, target: RepoTarget, run: ItemInput) -> Result<Run> {
    request(daemon, CiMethod::ViewCiRun { target, run }).await
}

/// Start `workflow`, or the only one, on `branch` with `inputs`.
pub async fn start(
    daemon: &impl Daemon,
    target: RepoTarget,
    workflow: Option<String>,
    branch: Option<String>,
    inputs: BTreeMap<String, String>,
) -> Result<StartedRun> {
    let method = CiMethod::StartCiRun {
        target,
        workflow,
        branch,
        inputs,
    };
    request(daemon, method).await
}

/// Apply `action` to the run `run` names, only its failed jobs when `failed`.
pub async fn act_on_run(
    daemon: &impl Daemon,
    target: RepoTarget,
    run: ItemInput,
    action: CiAction,
    failed: bool,
) -> Result<RunActed> {
    let method = CiMethod::CiRunAction {
        target,
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
    target: RepoTarget,
    item: Option<ItemInput>,
    action: CiAction,
    checks: Vec<String>,
) -> Result<CiActed> {
    let method = CiMethod::CiAction {
        target,
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
    target: RepoTarget,
    run: Option<ItemInput>,
    pr: Option<ItemInput>,
) -> Result<RunsApproved> {
    request(daemon, CiMethod::ApproveCiRuns { target, run, pr }).await
}

/// The log of `check` on the item `selection` selects.
pub async fn check_log(
    daemon: &impl Daemon,
    target: RepoTarget,
    selection: Selection,
    check: String,
) -> Result<CheckLog> {
    let method = CiMethod::CheckLog {
        target,
        selection,
        check,
    };
    request(daemon, method).await
}
