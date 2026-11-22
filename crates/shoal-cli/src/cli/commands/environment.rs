//! `shoal internal env`: a space's environment, with a scope token of its
//! own, for processes started outside Shoal, and revoking that token.
use anyhow::Result;

use crate::cli::{
    context::Context,
    output::Style,
    ui::{self, Fallback},
};
use crate::client::output::spaces::Revoked;
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceSelector};

pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    revoke: Option<ScopeToken>,
) -> Result<i32> {
    let space = ui::select_space(
        ctx,
        space,
        Fallback::CurrentDirectory(&crate::cli::ui::Acting::Unconverted),
    )
    .await?;
    match revoke {
        Some(token) => revoke_scope(ctx, space, token).await,
        None => export(ctx, space).await,
    }?;
    Ok(0)
}

async fn revoke_scope(ctx: &Context, space: SpaceSelector, token: ScopeToken) -> Result<()> {
    crate::client::spaces::revoke_scope(&ctx.paths, space, token).await?;
    ctx.emit_styled(
        Style::Success,
        "Scope token revoked",
        Revoked { revoked: true },
    )
}

/// Print the space's variables and a new scope token for it, issued only
/// once its environment could be read.
async fn export(ctx: &Context, space: SpaceSelector) -> Result<()> {
    let inherited = std::env::var_os("PATH");
    let mut values =
        crate::client::spaces::environment(&ctx.paths, space.clone(), inherited).await?;
    let token = crate::client::spaces::issue_scope(&ctx.paths, space).await?;
    values.insert(crate::env::SCOPE_TOKEN.into(), token.expose().into());
    ctx.show(&values, |values| {
        for (name, value) in values {
            outln!("{name}={value}");
        }
    })
}
