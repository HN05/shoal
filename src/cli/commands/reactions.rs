//! `shoal forge reactions`: the reactions the workspace repository's forge
//! allows, so clients can offer them before reacting.
use anyhow::Result;

use crate::{
    cli::{commands::capabilities::workspace_forge, context::Context},
    forge::items::reaction::allowed::AllowedReactions,
};

pub(super) async fn reactions(ctx: &Context, workspace: Option<String>) -> Result<i32> {
    let forge = workspace_forge(ctx, workspace).await?;
    let reactions = ctx
        .progress(
            "Reading reactions",
            forge.allowed_reactions(&ctx.forge_account()?),
        )
        .await?;
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
