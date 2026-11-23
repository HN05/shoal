//! Who a PR can ask for a review, named as `pr edit --add-reviewer` takes
//! them, so a client can offer the choices without knowing the forge.

use serde::{Deserialize, Serialize};

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
    pub fn user(name: impl Into<String>, display_name: Option<&str>) -> Self {
        Self {
            name: name.into(),
            display_name: display_name.unwrap_or_default().to_owned(),
            team: false,
        }
    }

    /// The team `team` of the organization `owner`, shown as `display_name`.
    pub fn team(owner: &str, team: &str, display_name: Option<&str>) -> Self {
        Self {
            name: format!("{owner}/{team}"),
            display_name: display_name.unwrap_or_default().to_owned(),
            team: true,
        }
    }
}
