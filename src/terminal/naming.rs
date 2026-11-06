//! What a place hosting a workspace is called, from the host's name
//! template, the repository and the workspace's issue.
use serde::{Deserialize, Serialize};

use crate::model::Repository;

/// The name of the place hosting a workspace, with the repository and issue
/// filled in, rendered once the branch is known.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct PlaceName {
    template: Option<String>,
    repo: String,
    issue_number: Option<u64>,
    issue_title: Option<String>,
}

impl PlaceName {
    /// The name of a place for a workspace of `repo`, for the `issue` given
    /// by its number and title, if any.
    pub(crate) fn new(
        template: Option<String>,
        repo: &Repository,
        issue: Option<(u64, &str)>,
    ) -> Self {
        Self {
            template,
            repo: crate::repository::name(repo).to_owned(),
            issue_number: issue.map(|(number, _)| number),
            issue_title: issue.map(|(_, title)| title.to_owned()),
        }
    }

    /// This name under the host's name `template`.
    pub(crate) fn templated(&self, template: Option<String>) -> Self {
        Self {
            template,
            ..self.clone()
        }
    }

    /// The label of a place hosting the workspace on `branch` with `title`.
    pub fn render(&self, branch: &str, title: Option<&str>) -> String {
        match &self.template {
            Some(template) => self.fill(template, branch, title),
            None => title.map_or_else(|| self.untitled(branch), str::to_owned),
        }
    }

    /// The label without a title or template: the issue's, else the branch.
    fn untitled(&self, branch: &str) -> String {
        match self.issue_number {
            Some(number) => issue_label(&self.repo, number),
            None => branch.to_owned(),
        }
    }

    /// `template` filled in; `{title}` falls back to the untitled label.
    fn fill(&self, template: &str, branch: &str, title: Option<&str>) -> String {
        let title = title.map_or_else(|| self.untitled(branch), str::to_owned);
        crate::config::templates::render(
            template,
            &[
                ("{repo}", &self.repo),
                ("{branch}", branch),
                ("{title}", &title),
                (
                    "{issue_number}",
                    &self.issue_number.map(|n| n.to_string()).unwrap_or_default(),
                ),
                (
                    "{issue_title}",
                    self.issue_title.as_deref().unwrap_or_default(),
                ),
            ],
        )
    }
}

/// `<repo>#<number>`: short, and unique across repositories sharing a host.
pub(crate) fn issue_label(repo: &str, number: u64) -> String {
    format!("{repo}#{number}")
}

#[cfg(test)]
mod tests;
