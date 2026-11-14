//! What a gated request is about: the subject whose gate session it
//! continues, and the action its denials count against.
use shoal_refs::BranchName;

use crate::daemon::approvals::{ApprovalRequest, Specification};

/// What one of a space's gate sessions covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Subject {
    /// The space's change: pushes of its own branch, its PR, its merge and
    /// unmerge.
    Change,
    /// Its issue proposals for one repository: by its URL, or none for the
    /// space's own.
    Issues(Option<String>),
    /// Its pushes to another ref, named as its request is.
    Ref(String),
}

impl Subject {
    /// The subject of `request` from a space on `branch`.
    pub(crate) fn of(request: &ApprovalRequest, branch: Option<&BranchName>) -> Self {
        match &request.specification {
            Specification::Issue(issue) => Self::Issues(issue.repository.clone()),
            Specification::Push(push)
                if branch.is_none_or(|branch| push.short_destination() != branch.as_str()) =>
            {
                Self::Ref(push.request_name())
            }
            _ => Self::Change,
        }
    }

    /// How the store names it.
    pub(crate) fn key(&self) -> String {
        match self {
            Self::Change => "change".to_owned(),
            Self::Issues(None) => "issues".to_owned(),
            Self::Issues(Some(repository)) => format!("issues {repository}"),
            Self::Ref(name) => format!("ref {name}"),
        }
    }

    /// Whether the session follows commits, so a resumed one is given those
    /// since its last verdict.
    pub(crate) fn follows_commits(&self) -> bool {
        !matches!(self, Self::Issues(_))
    }
}

/// The action `request` asks for, which a retry asks for again: the same
/// target bound to the same commit or ref, a PR proposal with the same
/// title, description and labels at the same commit, and an issue proposal
/// by its repository and title.
pub(crate) fn action(request: &ApprovalRequest) -> String {
    let bound = match &request.specification {
        Specification::Issue(issue) => serde_json::json!([issue.repository, issue.issue.title]),
        Specification::Pull(pull) => serde_json::json!([
            pull.pull.title,
            pull.pull.body,
            pull.pull.labels,
            pull.commit,
            pull.push
                .as_ref()
                .map(|push| (&push.destination, &push.commit))
        ]),
        Specification::Push(push) => {
            serde_json::json!([push.request_name(), push.commit, push.replaces])
        }
        Specification::Merge(merge) => {
            serde_json::json!([merge.branch, merge.head, merge.push, merge.unpushed])
        }
        Specification::Unmerge(unmerge) => serde_json::json!([unmerge.merge]),
        other => serde_json::json!([other.to_string()]),
    };
    format!("{} {bound}", request.target)
}

#[cfg(test)]
mod tests;
