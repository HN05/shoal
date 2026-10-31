//! `shoal review`: run local reviews of workspaces and PRs, list and cancel
//! them, and answer their findings.
use anyhow::Result;

use crate::cli::{args::changes::ReviewCommand, context::Context};

mod answer;
pub(super) mod show;
mod start;

pub(super) use start::fetch_pull_branch;

pub(super) async fn run(ctx: &Context, command: ReviewCommand) -> Result<i32> {
    match command {
        ReviewCommand::Run {
            workspace,
            pr,
            repository,
            post,
            no_post,
            manual,
            agent,
            args,
        } => {
            let post = (post || no_post).then_some(post);
            let target = start::Target::new(workspace, pr, repository, post)?;
            let reviewer = start::Reviewer::new(manual, agent);
            start::start(ctx, target, reviewer, args).await
        }
        ReviewCommand::List {
            workspace,
            repository,
        } => show::list(ctx, workspace, repository).await,
        ReviewCommand::Cancel { id, workspace } => answer::cancel(ctx, id, workspace).await,
        ReviewCommand::Reply {
            finding,
            reply,
            body_file,
        } => answer::reply(ctx, finding, reply, body_file).await,
        ReviewCommand::Resolve {
            finding,
            outcome,
            reason,
            issue,
            of,
            undo,
        } => {
            let outcome = match (outcome, undo) {
                (Some(kind), false) => Some(crate::reviews::Outcome::new(kind, reason, issue, of)?),
                _ => None,
            };
            answer::resolve(ctx, finding, outcome).await
        }
    }
}
