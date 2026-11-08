//! `shoal open`: handing a workspace to a desktop app.
use std::ffi::OsString;

use anyhow::{Result, ensure};

use crate::client::space_names::SpaceSelector;
use crate::{
    cli::{
        agents,
        args::workspace::DesktopApp,
        context::Context,
        ui::{self, Fallback},
    },
    model::Workspace,
    protocol::ConfigTarget,
};

/// Hand the named workspace, else the current directory's or a picked one,
/// to the named desktop app, else the one [`default_app`] gives.
pub(in crate::cli::commands) async fn open(
    ctx: &Context,
    workspace: Option<SpaceSelector>,
    name: Option<DesktopApp>,
    args: Vec<OsString>,
) -> Result<i32> {
    // A named app that cannot open is refused before a workspace is picked.
    if let Some(name) = name {
        agents::ensure_opens(agents::app(name), &args)?;
    }
    let workspace = ui::select_workspace(ctx, workspace, Fallback::CurrentDirectory).await?;
    let workspace = crate::client::workspaces::inspect(&ctx.paths, workspace.clone())
        .await?
        .workspace;
    let app = agents::app(match name {
        Some(name) => name,
        None => default_app(ctx, &workspace).await?,
    });
    agents::ensure_opens(app, &args)?;
    agents::hand_off(ctx, &workspace, app, args).await
}

/// The app `shoal open` opens in `workspace` without `--app`: its
/// `app.default`, else the only one installed, else one the user picks.
async fn default_app(ctx: &Context, workspace: &Workspace) -> Result<DesktopApp> {
    let target = ConfigTarget::Workspace(SpaceSelector::id(&workspace.id));
    let settings = crate::client::workspaces::settings(&ctx.paths, target).await?;
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
