//! The routes of Forgejo, and of Gitea, which serves the same pages: where
//! their items, branches and runs are on the web.
use crate::{ForgeRepo, backends::Routes, numbers::RunNumber, repo::locator::ItemRoute};

#[derive(Clone, Copy)]
pub struct Forgejo;

impl Routes for Forgejo {
    fn name(&self) -> &'static str {
        "Forgejo or Gitea"
    }

    fn nests_groups(&self) -> bool {
        false
    }

    fn issue_route(&self) -> ItemRoute {
        ItemRoute::Issue
    }

    fn pull_route(&self) -> ItemRoute {
        ItemRoute::Pulls
    }

    fn branch_route(&self) -> &'static str {
        "/src/branch/"
    }

    fn locate_run(&self, repo: &ForgeRepo, target: &str) -> Option<RunNumber> {
        run_number(repo, target)
    }
}

/// The run number of a run's or job's web URL in the repository, which may
/// be relative to the host.
pub fn run_number(repo: &ForgeRepo, target: &str) -> Option<RunNumber> {
    let parts = runs_path(repo, target)?;
    parts
        .first()?
        .parse()
        .ok()
        .filter(|number| *number > 0)
        .map(RunNumber::new)
}

/// The path components of `target` after the repository's runs. A host may
/// link relative to itself, or over HTTPS while its remote uses HTTP.
pub fn runs_path<'a>(repo: &ForgeRepo, target: &'a str) -> Option<Vec<&'a str>> {
    let relative = ["https", "http"]
        .iter()
        .find_map(|scheme| target.strip_prefix(&format!("{scheme}://{}", repo.host)))
        .unwrap_or(target);
    let rest = relative.strip_prefix(&format!("/{}/actions/runs/", repo.path))?;
    Some(rest.split('/').collect())
}
