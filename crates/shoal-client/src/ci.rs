//! CI on a space's forge: listing, starting, rerunning, cancelling and
//! approving runs and checks, and reading check logs.
use crate::forge::items::reference::ItemInput;
use std::collections::BTreeMap;

use anyhow::Result;

use crate::{
    connection::{Daemon, Prepared, PreparedPages, request},
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
pub fn runs(target: RepoTarget, pr: Option<ItemInput>, filter: RunFilter) -> PreparedPages<Run> {
    PreparedPages::new(CiMethod::ListCiRuns { target, pr, filter })
}

/// The run `run` names.
pub fn run(target: RepoTarget, run: ItemInput) -> Prepared<Run> {
    Prepared::new(CiMethod::ViewCiRun { target, run })
}

/// Start `workflow`, or the only one, on `branch` with `inputs`.
pub fn start(
    target: RepoTarget,
    workflow: Option<String>,
    branch: Option<String>,
    inputs: BTreeMap<String, String>,
) -> Prepared<StartedRun> {
    Prepared::new(CiMethod::StartCiRun {
        target,
        workflow,
        branch,
        inputs,
    })
}

/// Apply `action` to the run `run` names, only its failed jobs when `failed`.
pub fn act_on_run(
    target: RepoTarget,
    run: ItemInput,
    action: CiAction,
    failed: bool,
) -> Prepared<RunActed> {
    Prepared::new(CiMethod::CiRunAction {
        target,
        run,
        action,
        failed,
    })
}

/// Apply `action` to the `checks` on the head of the PR `item`, by default
/// the linked one, or to all of them.
pub fn act_on_checks(
    target: RepoTarget,
    item: Option<ItemInput>,
    action: CiAction,
    checks: Vec<String>,
) -> Prepared<CiActed> {
    Prepared::new(CiMethod::CiAction {
        target,
        item,
        action,
        checks,
    })
}

/// Approve the run `run` names, otherwise every waiting run on the head of
/// the PR `pr`.
pub fn approve(
    target: RepoTarget,
    run: Option<ItemInput>,
    pr: Option<ItemInput>,
) -> Prepared<RunsApproved> {
    Prepared::new(CiMethod::ApproveCiRuns { target, run, pr })
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
