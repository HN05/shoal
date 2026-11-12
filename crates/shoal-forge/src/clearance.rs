//! Whether an issue, PR or CI run that blocks work has cleared: an issue
//! once it closes, a PR once it merges or closes, and a CI run once it, or a
//! run of its workflow on the default branch that started after it, passes.
use crate::items::reference::ItemRef;
use std::collections::HashMap;

use anyhow::Result;

use crate::{
    ForgeRepo,
    account::Account,
    ci::run_action::Run,
    hosts::HostTypes,
    items::{item::ItemState, link::ItemKind},
};

/// What a blocker's lookup found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Clearance {
    /// It cleared, as this says, such as `run <url> passed on main at <commit>`.
    Cleared(String),
    Open,
}

/// A repository's URL and a workflow, which a forge that runs one pipeline
/// per branch leaves out.
type Workflow = (String, Option<String>);

/// The default branch and the newest finished run on it, or why reading
/// them failed.
type Newest = Result<(String, Option<Run>), String>;

/// The default branch of a repository and the newest finished run of a
/// workflow on it, read once for every blocking run of that workflow.
#[derive(Debug, Default)]
pub struct NewestRuns(HashMap<Workflow, Newest>);

/// Whether `item`, on the forge `hosts` says its host serves, has cleared;
/// `newest` keeps the runs read for CI runs.
pub async fn clearance(
    item: &ItemRef,
    hosts: &impl HostTypes,
    account: &Account,
    newest: &mut NewestRuns,
) -> Result<Clearance> {
    let forge = item.forge(hosts).await?;
    match item.kind().item() {
        Some(kind) => {
            let current = forge.read(account, kind, item.number().get()).await?;
            Ok(item_clearance(kind, current.item.state))
        }
        None => forge.run_clearance(account, item, newest).await,
    }
}

/// An issue clears once it closes, a PR once it merges or closes.
fn item_clearance(kind: ItemKind, state: ItemState) -> Clearance {
    match (kind, state) {
        (_, ItemState::Open) => Clearance::Open,
        (ItemKind::Pr, ItemState::Merged) => Clearance::Cleared("it merged".into()),
        _ => Clearance::Cleared("it closed".into()),
    }
}

impl ForgeRepo {
    /// Whether the run at `url` passed, as after a rerun, or a run of its
    /// workflow on the default branch that started after it did, whatever
    /// branch it ran on.
    async fn run_clearance(
        &self,
        account: &Account,
        run: &ItemRef,
        newest: &mut NewestRuns,
    ) -> Result<Clearance> {
        let run = self.view_run(account, run).await?;
        if run.passed() {
            return Ok(Clearance::Cleared("it passed".into()));
        }
        let key = (self.repository_url(), run.workflow.clone());
        if !newest.0.contains_key(&key) {
            let found = self
                .newest_on_default_branch(account, run.workflow.as_deref())
                .await;
            newest
                .0
                .insert(key.clone(), found.map_err(|error| format!("{error:#}")));
        }
        match &newest.0[&key] {
            Ok((branch, later)) => Ok(later_run_clearance(&run, branch, later.as_ref())),
            Err(error) => anyhow::bail!("{error}"),
        }
    }

    /// The default branch and the newest finished run of `workflow` on it.
    async fn newest_on_default_branch(
        &self,
        account: &Account,
        workflow: Option<&str>,
    ) -> Result<(String, Option<Run>)> {
        let branch = self.default_branch(account).await?;
        let run = self.newest_finished_run(account, workflow, &branch).await?;
        Ok((branch, run))
    }
}

/// `run` clears once `later`, the newest finished run of its workflow on
/// `branch`, the default branch, passed and started after it.
fn later_run_clearance(run: &Run, branch: &str, later: Option<&Run>) -> Clearance {
    match later {
        Some(later) if later.passed() && later.started_after(run) => {
            let at = (later.commit.as_deref()).map_or_else(String::new, |commit| {
                format!(" at {}", commit.get(..12).unwrap_or(commit))
            });
            Clearance::Cleared(format!("run {} passed on {branch}{at}", later.url))
        }
        _ => Clearance::Open,
    }
}

#[cfg(test)]
mod tests;
