//! `shoal internal env`: a space's environment, with a scope token of its own,
//! for processes started outside Shoal.
use anyhow::Result;

use crate::cli::{
    context::Context,
    output::Style,
    ui::{self, Acting, Fallback},
};
use crate::client::output::spaces::Revoked;
use shoal_protocol::{scope_tokens::ScopeToken, space_names::SpaceSelector};

pub(super) async fn run(
    ctx: &Context,
    space: Option<SpaceSelector>,
    revoke: Option<ScopeToken>,
) -> Result<i32> {
    let acting = exporting(revoke.clone());
    let space = ui::select_space(ctx, space, Fallback::CurrentDirectory(&acting)).await?;
    match revoke {
        Some(token) => revoke_scope(ctx, space, token).await,
        None => export(ctx, space).await,
    }?;
    Ok(0)
}

async fn revoke_scope(ctx: &Context, space: SpaceSelector, token: ScopeToken) -> Result<()> {
    crate::client::spaces::revoke_scope(space, token)
        .send(&ctx.paths)
        .await?;
    ctx.emit_styled(
        Style::Success,
        "Scope token revoked",
        Revoked { revoked: true },
    )
}

/// The token is issued only once the environment could be read.
async fn export(ctx: &Context, space: SpaceSelector) -> Result<()> {
    let inherited = std::env::var_os("PATH");
    let mut values =
        crate::client::spaces::environment(&ctx.paths, space.clone(), inherited).await?;
    let token = crate::client::spaces::issue_scope(space)
        .send(&ctx.paths)
        .await?;
    values.insert(crate::env::SCOPE_TOKEN.into(), token.expose().into());
    ctx.show(&values, |values| {
        for (name, value) in values {
            outln!("{name}={value}");
        }
    })
}

pub(super) fn exporting(revoke: Option<ScopeToken>) -> Acting {
    match revoke {
        Some(token) => {
            Acting::request(move |space| crate::client::spaces::revoke_scope(space, token.clone()))
        }
        None => Acting::request(crate::client::spaces::issue_scope),
    }
}
