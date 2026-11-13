//! How `shoal daemon stop` and `restart` name the work in progress that the
//! daemon's exit would cut off.
use crate::protocol::{Work, WorkPlace, operations::OperationKind};

/// Each piece of `work` named, as `3 api (removing), 5 docs (merging)`.
pub(super) fn names(work: &[Work]) -> String {
    let names: Vec<_> = work.iter().map(name).collect();
    names.join(", ")
}

fn name(work: &Work) -> String {
    match work {
        Work::Workspace { workspace, state } => in_workspace(workspace, state.as_str()),
        Work::Operation {
            workspace,
            operation,
        } => in_workspace(workspace, doing(*operation)),
        Work::Command { workspace } => in_workspace(workspace, "running a command"),
        Work::Other => "requests and background tasks".into(),
    }
}

fn in_workspace(workspace: &WorkPlace, doing: &str) -> String {
    format!("{} {} ({doing})", workspace.number, workspace.name)
}

/// What an operation of `kind` does.
fn doing(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Prepare => "preparing",
        OperationKind::OpenPull => "opening a PR",
        OperationKind::Merge => "merging",
        OperationKind::Unmerge => "undoing a merge",
        OperationKind::Push => "pushing",
    }
}
