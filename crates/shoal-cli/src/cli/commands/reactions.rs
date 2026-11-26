//! `shoal forge reactions`.
use anyhow::Result;

use crate::{
    cli::{
        args::repository::RepositoryArgs, client, commands::items::select_repo_context,
        context::Context,
    },
    forge::items::reaction::allowed::AllowedReactions,
};

pub(super) async fn reactions(ctx: &Context, repository: RepositoryArgs) -> Result<i32> {
    let target = select_repo_context(ctx, repository.repo).await?;
    let read = client::reactions(&ctx.paths, target);
    let reactions = ctx.progress("Reading reactions", read).await?;
    ctx.show(&reactions, render)?;
    Ok(0)
}

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
