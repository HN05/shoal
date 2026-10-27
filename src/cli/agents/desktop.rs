//! Desktop apps a launch hands a workspace to, which run untracked.
use std::{ffi::OsString, path::Path};

use anyhow::{Context as _, Result};

use crate::{env, execution};

/// Open `program`'s app at `directory` with `args`, returning its
/// launcher's exit code. The launcher hands the directory to another
/// process; its short-lived command is not the agent session and must not
/// own/kill the app's process group.
pub(super) async fn open(program: &str, directory: &Path, args: Vec<OsString>) -> Result<i32> {
    let status = tokio::process::Command::new(program)
        .arg("app")
        .arg(directory)
        .args(args)
        .current_dir(directory)
        .env_remove(env::SHELL_DIRECTIVE)
        .status()
        .await
        .with_context(|| format!("launch {program} app"))?;
    Ok(execution::exit_code(status))
}
