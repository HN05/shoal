//! Desktop apps a launch hands a workspace to, which run untracked.
use std::{
    ffi::{OsStr, OsString},
    path::Path,
};

use anyhow::{Context as _, Result};

use crate::{env, execution};

/// A desktop app and how its launcher opens a workspace in it.
#[derive(Debug)]
pub(in crate::cli) struct App {
    /// What messages call the app.
    pub name: &'static str,
    /// The launcher that opens it.
    pub program: &'static str,
    /// The app bundle the launcher opens, when the launcher cannot install
    /// it.
    pub bundle: Option<&'static str>,
    /// The launcher's arguments that open a directory with the forwarded
    /// arguments, or why the app cannot open them.
    pub args: fn(&Path, Vec<OsString>) -> Result<Vec<OsString>>,
}

impl App {
    /// Whether the app can open: its launcher is on `search_path` and its
    /// bundle, if it names one, is installed.
    pub fn available(&self, search_path: &OsStr) -> bool {
        super::program_available(self.program, search_path)
            && self.bundle.is_none_or(|bundle| Path::new(bundle).exists())
    }
}

/// The T3 Code app, which must already be running.
pub(in crate::cli) const T3: App = App {
    name: "t3 app",
    program: "t3",
    bundle: None,
    args: app_subcommand,
};

/// Arguments of a launcher whose `app` subcommand takes the directory
/// followed by the forwarded arguments.
pub(super) fn app_subcommand(directory: &Path, args: Vec<OsString>) -> Result<Vec<OsString>> {
    Ok([OsString::from("app"), directory.into()]
        .into_iter()
        .chain(args)
        .collect())
}

/// Run `app`'s launcher with `args` from `directory`, returning its exit
/// code. The launcher hands the directory to another process; its
/// short-lived command is not the agent session and must not own/kill the
/// app's process group.
pub(super) async fn open(app: &App, directory: &Path, args: Vec<OsString>) -> Result<i32> {
    let status = tokio::process::Command::new(app.program)
        .args(args)
        .current_dir(directory)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .with_context(|| format!("launch {}", app.name))?;
    Ok(execution::exit_code(status))
}
