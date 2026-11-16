//! `shoal open`: handing a space to a desktop app.
use std::ffi::OsString;

use anyhow::{Context as _, Result, ensure};

use crate::{
    cli::{
        agents,
        args::space::DesktopApp,
        context::Context,
        ui::{self, Fallback},
    },
    model::{Repository, Space},
    protocol::ConfigTarget,
};
use shoal_ids::space_ids::SpaceId;
use shoal_protocol::space_names::SpaceSelector;

/// Hand the named space, else the current directory's or a picked one, to
/// the named desktop app, else the one [`default_app`] gives.
pub(in crate::cli::commands) async fn open(
    ctx: &Context,
    space: Option<SpaceSelector>,
    name: Option<DesktopApp>,
    args: Vec<OsString>,
) -> Result<i32> {
    // A named app that cannot open is refused before a space is picked.
    if let Some(name) = name {
        agents::ensure_opens(agents::app(name), &args)?;
    }
    let selector = ui::select_space(ctx, space, Fallback::CurrentDirectory).await?;
    let space = read(ctx, &selector).await?;
    let app = agents::app(match name {
        Some(name) => name,
        None => default_app(ctx, &space.id).await?,
    });
    agents::ensure_opens(app, &args)?;
    let repository = repository(ctx, &space).await?;
    agents::hand_off(ctx, &space, repository.as_ref(), app, args).await
}

/// The space `selector` names, among those the caller sees.
async fn read(ctx: &Context, selector: &SpaceSelector) -> Result<Space> {
    let spaces = crate::client::spaces::list(&ctx.paths).await?;
    spaces
        .find(selector)
        .with_context(|| format!("no space {selector}"))
}

/// The registered repository `space` belongs to; none for a chat without
/// one.
async fn repository(ctx: &Context, space: &Space) -> Result<Option<Repository>> {
    let Some(id) = &space.repository_id else {
        return Ok(None);
    };
    let repositories = crate::client::repositories::list(&ctx.paths).await?;
    let repository = (repositories.into_iter())
        .find(|repository| repository.id == *id)
        .with_context(|| format!("the repository of space {} is missing", space.label()))?;
    Ok(Some(repository))
}

/// The app `shoal open` opens in space `id` without `--app`: its
/// `app.default`, else the only one installed, else one the user picks.
async fn default_app(ctx: &Context, id: &SpaceId) -> Result<DesktopApp> {
    let target = ConfigTarget::Space(SpaceSelector::id(id));
    let settings = shoal_workers::settings::of(&ctx.paths, target).await?;
    match settings.app.default {
        Some(name) => Ok(name),
        None => choose(ctx),
    }
}

/// The only desktop app installed, else one the user picks among those
/// installed.
fn choose(ctx: &Context) -> Result<DesktopApp> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    let installed: Vec<_> = (DesktopApp::ALL.iter().copied())
        .filter(|name| agents::app(*name).available(&search_path))
        .map(|name| (name, name.as_str()))
        .collect();
    match installed[..] {
        [] => {
            anyhow::bail!("no desktop app is installed; install the Claude, Codex or T3 Code app")
        }
        [(only, _)] => Ok(only),
        _ => {
            ensure!(
                ctx.interactive(),
                "several desktop apps are installed; choose one with --app"
            );
            ui::pick_choice(ctx, "App> ", &installed)
        }
    }
}
