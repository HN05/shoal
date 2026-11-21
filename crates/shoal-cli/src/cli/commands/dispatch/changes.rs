use anyhow::Result;

use crate::cli::{
    args::changes::{BranchCommand, ChangesCommand},
    commands::{
        base, branch_rename, changes, ci, conflicts, diff, forge_branches, items, merge, review,
        unmerge,
    },
    context::Context,
};

pub(in crate::cli::commands) async fn run(ctx: Context, command: ChangesCommand) -> Result<i32> {
    match command {
        ChangesCommand::Pr { command } => items::pr(&ctx, command).await,
        ChangesCommand::Issue { command } => items::issue(&ctx, command).await,
        ChangesCommand::Ci { command } => ci::run(&ctx, command).await,
        ChangesCommand::Branch { command } => branch(&ctx, command).await,
        ChangesCommand::Diff {
            thread: Some(thread),
            since,
            ..
        } => changes::thread_diff(&ctx, thread, since).await,
        ChangesCommand::Diff { workspace, .. } => diff::diff(&ctx, workspace).await,
        ChangesCommand::Conflicts { target, workspace } => {
            conflicts::conflicts(&ctx, workspace, target).await
        }
        ChangesCommand::Review { command } => review::run(&ctx, command).await,
        ChangesCommand::Merge(args) => merge::run(&ctx, args).await,
        ChangesCommand::Unmerge { space } => unmerge::unmerge(&ctx, space).await,
    }
}

async fn branch(ctx: &Context, command: BranchCommand) -> Result<i32> {
    match command {
        BranchCommand::Rename { workspace, branch } => {
            branch_rename::rename_branch(ctx, workspace, branch).await
        }
        BranchCommand::Base { command, workspace } => base::run(ctx, command, workspace).await,
        BranchCommand::Ls {
            space,
            repo,
            merged,
            unmerged,
        } => {
            let filter = forge_branches::MergedFilter { merged, unmerged };
            forge_branches::list(ctx, space, repo, filter).await
        }
        BranchCommand::Delete {
            branches,
            space,
            repo,
            force,
        } => forge_branches::delete(ctx, space, repo, branches, force).await,
    }
}
