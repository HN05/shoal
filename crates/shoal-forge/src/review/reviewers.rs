//! Who a PR can ask for a review, named as `pr edit --add-reviewer` takes
//! them, so a client can offer the choices without knowing the forge.

/// A reviewer name that names a team, split into its organization and team;
/// a username names no team.
pub(crate) fn team_name(name: &str) -> Option<(&str, &str)> {
    name.split_once('/')
}

#[cfg(test)]
mod tests;
