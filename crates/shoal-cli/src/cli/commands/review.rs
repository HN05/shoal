//! `shoal review`: run local reviews of workspaces and PRs, list and cancel
//! them; their findings are threads, which `shoal thread` resolves.
use anyhow::Result;

use crate::{
    cli::{
        args::changes::ReviewCommand,
        context::Context,
        ui::{self, Fallback},
    },
    model::Chat,
    reviews::{Review, ReviewTarget},
};
use shoal_protocol::space_names::SpaceSelector;

mod cancel;
pub(super) mod show;
mod start;

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
        ReviewCommand::Ls { space, repository } => show::list(ctx, space, repository).await,
        ReviewCommand::Cancel { id, space } => cancel::cancel(ctx, id, space).await,
    }
}

/// A space a review command acts on.
enum Space {
    Chat(Box<Chat>),
    Workspace(SpaceSelector),
}

/// The space `space` names, else the current workspace or the chat this
/// runs in, else one the user picks.
async fn select(ctx: &Context, space: Option<SpaceSelector>) -> Result<Space> {
    let selected = ui::select_space(
        ctx,
        space,
        Fallback::CurrentDirectory,
        &crate::cli::ui::Acting::Unconverted,
    )
    .await?;
    let found = crate::client::spaces::present_space(&ctx.paths, &selected).await?;
    Ok(match found.map(crate::model::Space::into_chat) {
        Some(Ok(chat)) => Space::Chat(Box::new(chat)),
        _ => Space::Workspace(selected),
    })
}

/// The running chat `space` names, else the chat this runs in when `space`
/// names nothing and the current directory is in no workspace below it.
async fn chat(ctx: &Context, space: Option<&SpaceSelector>) -> Result<Option<Chat>> {
    let named = match space {
        Some(space) => space.clone(),
        None if ui::current_chat(&None).is_some() => {
            ui::select_space(
                ctx,
                None,
                Fallback::CurrentDirectory,
                &crate::cli::ui::Acting::Unconverted,
            )
            .await?
        }
        None => return Ok(None),
    };
    let found = crate::client::spaces::present_space(&ctx.paths, &named).await?;
    Ok(found.and_then(|space| space.into_chat().ok()))
}

/// A chat's reviews: those of its linked PRs, which run in the workspaces
/// that own their head branches, newest first.
async fn chat_reviews(ctx: &Context, chat: &Chat) -> Result<Vec<Review>> {
    let repository = chat.repository_id.clone().map(Into::into);
    let reviews = crate::client::reviews::list(&ctx.paths, None, repository).await?;
    Ok(reviews
        .into_iter()
        .filter(|review| match &review.target {
            ReviewTarget::Pr { url, .. } => chat.links.prs.iter().any(|pr| pr.url == *url),
            ReviewTarget::Workspace => false,
        })
        .collect())
}
