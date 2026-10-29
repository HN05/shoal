//! Who a PR can ask for a review, named as `pr edit --add-reviewer` takes
//! them, so a client can offer the choices without knowing the forge.
use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::{ForgeRepo, account::Account, backends::Backend, items::link::ItemKind};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reviewer {
    /// What a reviewer change takes: a username, `owner/team` for a team,
    /// or an account ID where the forge names users by one.
    pub name: String,
    /// The name the forge shows, when it has one apart from `name`.
    pub display_name: String,
    pub team: bool,
}

impl Reviewer {
    pub(crate) fn user(name: impl Into<String>, display_name: Option<&str>) -> Self {
        Self {
            name: name.into(),
            display_name: display_name.unwrap_or_default().to_owned(),
            team: false,
        }
    }

    /// The team `team` of the organization `owner`, shown as `display_name`.
    pub(crate) fn team(owner: &str, team: &str, display_name: Option<&str>) -> Self {
        Self {
            name: format!("{owner}/{team}"),
            display_name: display_name.unwrap_or_default().to_owned(),
            team: true,
        }
    }
}

/// A reviewer name that names a team, split into its organization and team;
/// a username names no team.
pub(crate) fn team_name(name: &str) -> Option<(&str, &str)> {
    name.split_once('/')
}

impl ForgeRepo {
    /// The users and teams PR `number` can ask for a review, in the forge's
    /// order, leaving out its author.
    pub async fn reviewers(&self, account: &Account, number: u64) -> Result<Vec<Reviewer>> {
        // Boxed so each forge's user and team reads stay off the caller's stack.
        let (pr, candidates) = tokio::try_join!(
            self.read(account, ItemKind::Pr, number),
            Box::pin(self.kind.reviewers(self, account)),
        )?;
        Ok(candidates
            .into_iter()
            .filter(|reviewer| reviewer.team || pr.author.as_ref() != Some(&reviewer.name))
            .collect())
    }
}

#[cfg(test)]
mod tests;
