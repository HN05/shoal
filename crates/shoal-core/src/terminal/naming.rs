//! What a place hosting a workspace is called: its name, or the host's name
//! template filled with it, the repository, the branch and the issue.
use serde::{Deserialize, Serialize};

use crate::model::Repository;

/// The name of the place hosting a workspace, with the repository and issue
/// filled in, rendered once the branch is known.
#[derive(Clone, Serialize, Deserialize)]
pub struct PlaceName {
    template: Option<String>,
    repo: String,
    issue_number: Option<u64>,
    issue_title: Option<String>,
}

impl PlaceName {
    /// The name of a place for a workspace of `repo`, for the `issue` given
    /// by its number and title, if any.
    pub fn new(template: Option<String>, repo: &Repository, issue: Option<(u64, &str)>) -> Self {
        Self {
            template,
            repo: crate::repository::name(repo).to_owned(),
            issue_number: issue.map(|(number, _)| number),
            issue_title: issue.map(|(_, title)| title.to_owned()),
        }
    }

    /// This name under the host's name `template`.
    pub fn templated(&self, template: Option<String>) -> Self {
        Self {
            template,
            ..self.clone()
        }
    }

    /// The label of a place hosting the workspace `name` on `branch`: the
    /// name, or the host's template filled in.
    pub fn render(&self, branch: &str, name: &str) -> String {
        match &self.template {
            Some(template) => self.fill(template, branch, name),
            None => name.to_owned(),
        }
    }

    /// `template` filled in.
    fn fill(&self, template: &str, branch: &str, name: &str) -> String {
        crate::config::templates::render(
            template,
            &[
                ("{repo}", &self.repo),
                ("{branch}", branch),
                ("{name}", name),
                // As templates written before names replaced titles say it.
                ("{title}", name),
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

#[cfg(test)]
mod tests;
