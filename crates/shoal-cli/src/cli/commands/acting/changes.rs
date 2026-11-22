//! Change, branch, review, PR, issue and CI commands: the request each
//! sends for the space it acts on.
use crate::cli::{
    args::{
        changes::{BranchCommand, ChangesCommand, IssueCommand, PrCommand},
        ci::CiCommand,
    },
    commands::{base, branch_rename, conflicts, diff, items, merge, review, unmerge},
    ui::Acting,
};

/// What a change command sends for the space it acts on.
pub(super) fn acting(command: &ChangesCommand) -> Option<Acting> {
    match command {
        ChangesCommand::Branch { command } => branch(command),
        ChangesCommand::Diff {
            thread: Some(_), ..
        } => None,
        ChangesCommand::Diff { .. } => Some(diff::reading()),
        ChangesCommand::Conflicts { .. } => Some(conflicts::checking()),
        ChangesCommand::Review { command } => Some(review::acting(command)),
        ChangesCommand::Pr { command } => pr(command),
        ChangesCommand::Issue { command } => issue(command),
        ChangesCommand::Ci { command } => ci(command),
        ChangesCommand::Merge(args) => merge::acting(args),
        ChangesCommand::Unmerge { .. } => Some(unmerge::unmerging()),
    }
}

fn branch(command: &BranchCommand) -> Option<Acting> {
    match command {
        BranchCommand::Rename { .. } => Some(branch_rename::renaming()),
        BranchCommand::Base { command, .. } => Some(base::basing(command.as_ref())),
        // The forge's branches are listed and deleted in a space's context.
        BranchCommand::Ls { .. } | BranchCommand::Delete { .. } => None,
    }
}

/// A PR command's request; one that acts on a PR reads it in the context
/// of the space it names.
fn pr(command: &PrCommand) -> Option<Acting> {
    match command {
        PrCommand::Open { .. } => Some(items::opening_pull()),
        PrCommand::Revert { .. } => Some(items::reverting()),
        PrCommand::Ls(_)
        | PrCommand::Edit { .. }
        | PrCommand::Reviewers { .. }
        | PrCommand::Comment { .. }
        | PrCommand::Review { .. }
        | PrCommand::React { .. }
        | PrCommand::Reply { .. }
        | PrCommand::Resolve { .. }
        | PrCommand::Unresolve { .. }
        | PrCommand::Close { .. }
        | PrCommand::Reopen { .. }
        | PrCommand::Update { .. } => None,
    }
}

/// An issue command's request; one that acts on an issue reads it in the
/// context of the space it names.
fn issue(command: &IssueCommand) -> Option<Acting> {
    match command {
        IssueCommand::Open { .. } => Some(items::opening_issue()),
        IssueCommand::Ls(_)
        | IssueCommand::Edit { .. }
        | IssueCommand::Comment { .. }
        | IssueCommand::React { .. }
        | IssueCommand::Close { .. }
        | IssueCommand::Reopen { .. } => None,
    }
}

/// CI commands act on runs in the context of the space they name.
fn ci(command: &CiCommand) -> Option<Acting> {
    match command {
        CiCommand::Ls(_)
        | CiCommand::Run(_)
        | CiCommand::Rerun { .. }
        | CiCommand::Cancel { .. }
        | CiCommand::Approve { .. } => None,
    }
}
