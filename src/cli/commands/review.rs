//! `shoal review`: run local reviews of workspaces and PRs, list and cancel
//! them, and answer their findings.
use anyhow::Result;

use crate::{
    cli::{
        args::changes::ReviewCommand,
        context::Context,
        ui::{self, Fallback},
    },
    model::{Chat, PresentSpace},
    reviews::{Review, ReviewTarget},
};

mod answer;
pub(super) mod show;
mod start;

pub(super) use start::fetch_pull_branch;

pub(super) async fn run(ctx: &Context, command: ReviewCommand) -> Result<i32> {
    match command {
        ReviewCommand::Run {
            space,
            pr,
            repository,
            post,
            no_post,
            manual,
            agent,
            model,
            args,
        } => {
            let post = (post || no_post).then_some(post);
            let target = start::Target::new(space, pr, repository, post)?;
            let model = model.into();
            let reviewer = start::Reviewer::new(manual, agent, &model);
            start::start(ctx, target, reviewer, model, args).await
        }
        ReviewCommand::List { space, repository } => show::list(ctx, space, repository).await,
        ReviewCommand::Cancel { id, space } => answer::cancel(ctx, id, space).await,
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

/// A space a review command acts on.
enum Space {
    Chat(Box<Chat>),
    Workspace(String),
}

/// The space `space` names, else the chat this runs in or the current
/// workspace, else one the user picks.
async fn select(ctx: &Context, space: Option<String>) -> Result<Space> {
    let selected = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    Ok(
        match crate::client::workspaces::present_space(&ctx.paths, &selected).await? {
            Some(PresentSpace::Chat(chat)) => Space::Chat(Box::new(chat)),
            _ => Space::Workspace(selected),
        },
    )
}

/// The running chat `space` names, else the chat this runs in when `space`
/// names nothing.
async fn chat(ctx: &Context, space: Option<&str>) -> Result<Option<Chat>> {
    let Some(named) = space.map(str::to_owned).or_else(crate::env::chat) else {
        return Ok(None);
    };
    Ok(
        match crate::client::workspaces::present_space(&ctx.paths, &named).await? {
            Some(PresentSpace::Chat(chat)) => Some(chat),
            _ => None,
        },
    )
}

/// A chat's reviews: those of its linked PRs, which run in the workspaces
/// that own their head branches, newest first.
async fn chat_reviews(ctx: &Context, chat: &Chat) -> Result<Vec<Review>> {
    let repository = Some(chat.repository_id.clone().into());
    let reviews = crate::client::reviews::list(&ctx.paths, None, repository).await?;
    Ok(reviews
        .into_iter()
        .filter(|review| match &review.target {
            ReviewTarget::Pr { url, .. } => chat.links.prs.contains(url),
            ReviewTarget::Workspace => false,
        })
        .collect())
}
