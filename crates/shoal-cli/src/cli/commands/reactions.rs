//! `shoal forge reactions`: the reactions the forge of the workspace's
//! repository, or another one, allows, so clients can offer them before
//! reacting.
use anyhow::Result;

use crate::{
    cli::{
        args::repository::RepositoryArgs, client, commands::items::select_repo_context,
        context::Context,
    },
    forge::items::reaction::allowed::AllowedReactions,
};

pub(super) async fn reactions(ctx: &Context, repository: RepositoryArgs) -> Result<i32> {
    let (context, named) = select_repo_context(ctx, repository.repo).await?;
    let read = client::reactions(&ctx.paths, context, named);
    let reactions = ctx.progress("Reading reactions", read).await?;
    ctx.show(&reactions, render)?;
    Ok(0)
}

/// One line per reaction, then whether the forge accepts others.
fn render(reactions: &AllowedReactions) {
    if reactions.names.is_empty() {
        outln!("No reactions");
    }
    for name in &reactions.names {
        outln!("{name}");
    }
    if reactions.accepts_other_names {
        outln!("Other emoji names are also accepted.");
    }
}
