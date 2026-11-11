//! Looking forge blockers up: an issue clears once it closes, a PR once it
//! merges or closes, and a CI run once it or a run of its workflow on the
//! default branch that started after it passes.
use std::collections::{HashMap, HashSet};

use anyhow::Result;

use super::{Lookup, Lookups, Target};
use crate::{
    daemon::{forge_hosts::ForgeHosts, workspace::Manager},
    forge::{ForgeRepo, account::Account, ci::run_action::Run, items::item::ItemState},
    state::BlockerKind,
};

/// The newest finished run of a workflow on a repository's default branch,
/// by the repository's URL and the workflow, read once per poll, with the
/// branch.
type NewestRuns = HashMap<(String, Option<String>), Result<(String, Option<Run>), String>>;

impl Manager {
    /// What the forge says of each of the forge blockers among `targets`.
    pub(super) async fn look_up_blockers(&self, targets: HashSet<Target>) -> Lookups {
        let account = self.user_account();
        let hosts = self.forge_hosts();
        let mut newest = NewestRuns::new();
        let mut lookups = Lookups::new();
        for (kind, target) in targets {
            let found = match kind {
                BlockerKind::Issue => issue(&hosts, &account, &target).await,
                BlockerKind::Pr => pull(&hosts, &account, &target).await,
                BlockerKind::CiRun => ci_run(&hosts, &account, &target, &mut newest).await,
                BlockerKind::Space | BlockerKind::Launch => continue,
            };
            let lookup = found.unwrap_or_else(|error| Lookup::Failed(format!("{error:#}")));
            lookups.insert((kind, target), lookup);
        }
        lookups
    }
}

async fn issue(hosts: &ForgeHosts<'_>, account: &Account, url: &str) -> Result<Lookup> {
    let forge = ForgeRepo::resolve_issue_url(url, hosts).await?;
    Ok(match forge.issue_url_closed(account, url).await? {
        true => Lookup::Cleared("it closed".into()),
        false => Lookup::Open,
    })
}

async fn pull(hosts: &ForgeHosts<'_>, account: &Account, url: &str) -> Result<Lookup> {
    let forge = ForgeRepo::resolve_pull_url(url, hosts).await?;
    Ok(match forge.pull_url_state(account, url).await? {
        ItemState::Merged => Lookup::Cleared("it merged".into()),
        ItemState::Closed => Lookup::Cleared("it closed".into()),
        ItemState::Open => Lookup::Open,
    })
}

/// The run at `url` passed, as after a rerun, or a run of its workflow on
/// the default branch that started after it did, whatever branch it ran on.
async fn ci_run(
    hosts: &ForgeHosts<'_>,
    account: &Account,
    url: &str,
    newest: &mut NewestRuns,
) -> Result<Lookup> {
    let forge = ForgeRepo::resolve_run_url(url, hosts).await?;
    let run = forge.view_run(account, url).await?;
    if run.passed() {
        return Ok(Lookup::Cleared("it passed".into()));
    }
    let key = (forge.repository_url(), run.workflow.clone());
    if !newest.contains_key(&key) {
        let found = newest_on_default_branch(&forge, account, run.workflow.as_deref()).await;
        newest.insert(key.clone(), found.map_err(|error| format!("{error:#}")));
    }
    let (branch, later) = match &newest[&key] {
        Ok(found) => found,
        Err(error) => anyhow::bail!("{error}"),
    };
    Ok(match later {
        Some(later) if later.passed() && later.started_after(&run) => {
            let at = (later.commit.as_deref()).map_or_else(String::new, |commit| {
                format!(" at {}", commit.get(..12).unwrap_or(commit))
            });
            Lookup::Cleared(format!("run {} passed on {branch}{at}", later.url))
        }
        _ => Lookup::Open,
    })
}

/// The repository's default branch and the newest finished run of
/// `workflow` on it.
async fn newest_on_default_branch(
    forge: &ForgeRepo,
    account: &Account,
    workflow: Option<&str>,
) -> Result<(String, Option<Run>)> {
    let branch = forge.default_branch(account).await?;
    let run = forge
        .newest_finished_run(account, workflow, &branch)
        .await?;
    Ok((branch, run))
}
