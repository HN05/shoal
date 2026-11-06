//! Desktop apps `shoal open` hands a workspace to, which run untracked.
use std::{
    ffi::{OsStr, OsString},
    os::unix::ffi::OsStrExt as _,
    path::Path,
};

use anyhow::{Context as _, Result, ensure};

use super::{
    AgentEnvironment,
    trust::{CLAUDE_CODE, CODEX, TrustFile},
};
use crate::{
    cli::{args::workspace::DesktopApp, context::Context, ui},
    env, execution,
    model::Workspace,
    protocol::ConfigTarget,
};

#[cfg(test)]
mod tests;

/// A desktop app and how its launcher opens a workspace in it.
pub(in crate::cli) struct App {
    /// What messages call the app.
    pub name: &'static str,
    /// The launcher that opens it.
    pub program: &'static str,
    /// The app bundle the launcher opens, when the launcher cannot install
    /// it.
    pub bundle: Option<&'static str>,
    /// The config file in which the app records trusted directories.
    pub trust: Option<&'static TrustFile>,
    /// Refuse forwarded arguments the app cannot open with on this host,
    /// before a workspace is picked.
    pub check: fn(&[OsString]) -> Result<()>,
    /// The launcher's arguments that open a directory with the forwarded
    /// arguments.
    pub args: fn(&Path, Vec<OsString>) -> Vec<OsString>,
}

impl App {
    /// Whether the app can open: its launcher is on `search_path` and its
    /// bundle, if it names one, is installed.
    pub fn available(&self, search_path: &OsStr) -> bool {
        super::program_available(self.program, search_path)
            && self.bundle.is_none_or(|bundle| Path::new(bundle).exists())
    }
}

/// The app `shoal open --app` names.
pub(in crate::cli) fn app(name: DesktopApp) -> &'static App {
    match name {
        DesktopApp::Claude => &CLAUDE,
        DesktopApp::Codex => &CODEX_APP,
        DesktopApp::T3 => &T3,
    }
}

/// The Claude app, opened with the link `claude --desktop` opens, which needs
/// no Claude account sign-in of the CLI.
pub(in crate::cli) static CLAUDE: App = App {
    name: "Claude app",
    program: "open",
    bundle: Some("/Applications/Claude.app"),
    trust: Some(&CLAUDE_CODE),
    check: macos_without_arguments,
    args: new_session,
};

/// The Codex app, which `codex app` offers to install when it is missing.
pub(in crate::cli) static CODEX_APP: App = App {
    name: "codex app",
    program: "codex",
    bundle: None,
    trust: Some(&CODEX),
    check: any_arguments,
    args: app_subcommand,
};

/// The T3 Code app, which must already be running.
pub(in crate::cli) static T3: App = App {
    name: "t3 app",
    program: "t3",
    bundle: None,
    trust: None,
    check: any_arguments,
    args: app_subcommand,
};

/// The check of an app that takes any arguments on any host.
fn any_arguments(_args: &[OsString]) -> Result<()> {
    Ok(())
}

/// Arguments of a launcher whose `app` subcommand takes the directory
/// followed by the forwarded arguments.
fn app_subcommand(directory: &Path, args: Vec<OsString>) -> Vec<OsString> {
    [OsString::from("app"), directory.into()]
        .into_iter()
        .chain(args)
        .collect()
}

/// The Claude app opens through `open` on macOS, and its link carries no
/// arguments.
fn macos_without_arguments(args: &[OsString]) -> Result<()> {
    ensure!(
        cfg!(target_os = "macos"),
        "the Claude app opens workspaces only on macOS"
    );
    ensure!(args.is_empty(), "the Claude app takes no arguments");
    Ok(())
}

/// `open`'s argument: the link that starts a Claude Code session in
/// `directory`.
fn new_session(directory: &Path, _args: Vec<OsString>) -> Vec<OsString> {
    let folder = percent_encode(directory.as_os_str().as_bytes());
    vec![format!("claude://code/new?folder={folder}").into()]
}

/// `bytes` with every byte but the URL-unreserved ones percent-encoded.
fn percent_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// `shoal open`: hand the named workspace, else the current directory's or
/// a picked one, to the named desktop app, else the one [`default_app`]
/// gives.
pub(in crate::cli) async fn open(
    ctx: &Context,
    workspace: Option<String>,
    name: Option<DesktopApp>,
    args: Vec<OsString>,
) -> Result<i32> {
    // A named app that cannot open is refused before a workspace is picked.
    if let Some(name) = name {
        ensure_opens(app(name), &args)?;
    }
    let workspace = super::launch::shortcut_workspace(ctx, workspace).await?;
    let app = app(match name {
        Some(name) => name,
        None => default_app(ctx, &workspace).await?,
    });
    ensure_opens(app, &args)?;
    hand_off(ctx, &workspace, app, args).await
}

/// The app `shoal open` opens in `workspace` without `--app`: its
/// `app.default`, else the only one installed, else one the user picks.
async fn default_app(ctx: &Context, workspace: &Workspace) -> Result<DesktopApp> {
    let target = ConfigTarget::Workspace(workspace.id.clone());
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
        .filter(|name| app(*name).available(&search_path))
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

/// Hand `workspace` to desktop `app` with `args`, untracked, trusting it
/// first in the app's trust file, when it has one.
async fn hand_off(
    ctx: &Context,
    workspace: &Workspace,
    app: &App,
    args: Vec<OsString>,
) -> Result<i32> {
    ensure!(workspace.path.is_dir(), "workspace directory is missing");
    let args = (app.args)(&workspace.path, args);
    if let Some(trust) = app.trust {
        // The app runs with this process's environment, not the workspace's.
        let environment = AgentEnvironment::inherited(&ctx.paths.home);
        trust.trust_workspace(ctx, workspace, &environment).await;
    }
    run_launcher(app, &workspace.path, args).await
}

/// Refuse an `app` that cannot open with `args`: its check refuses them or
/// its launcher is missing.
fn ensure_opens(app: &App, args: &[OsString]) -> Result<()> {
    (app.check)(args)?;
    super::ensure_program(app.name, app.program)
}

/// Run `app`'s launcher with `args` from `directory`, returning its exit
/// code. The launcher hands the directory to another process; its
/// short-lived command is not the agent session and must not own/kill the
/// app's process group.
async fn run_launcher(app: &App, directory: &Path, args: Vec<OsString>) -> Result<i32> {
    let status = tokio::process::Command::new(app.program)
        .args(args)
        .current_dir(directory)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .with_context(|| format!("launch {}", app.name))?;
    Ok(execution::exit_code(status))
}
