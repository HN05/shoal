//! The worker of tracked setup that copies the repository checkout's
//! ignored files into the new workspace before the setup command runs.
use std::path::{Path, PathBuf};

use anyhow::{Result, ensure};

use crate::{env, git};

/// Copy the main checkout's ignored files but those `exclude` matches inside
/// tracked setup, then become the setup command so its exit status and
/// signals are the execution's.
pub async fn copy_ignored_worker(exclude: &[String], setup_cmd: Option<PathBuf>) -> Result<i32> {
    ensure!(
        env::is_scoped(),
        "copy-ignored worker requires a tracked execution"
    );
    // Tracked setup runs in the workspace root.
    let workspace = std::env::current_dir()?;
    let copied = git::ignored::copy(&workspace, exclude).await?;
    if copied > 0 {
        eprintln!(
            "Copied {copied} ignored file{} from the repository checkout",
            if copied == 1 { "" } else { "s" }
        );
    }
    match setup_cmd {
        Some(setup_cmd) => Err(become_setup(&setup_cmd)),
        None => Ok(0),
    }
}

/// Replace this process with `setup_cmd`; returns only the error when that fails.
fn become_setup(setup_cmd: &Path) -> anyhow::Error {
    let error =
        std::os::unix::process::CommandExt::exec(&mut std::process::Command::new(setup_cmd));
    anyhow::Error::new(error).context(format!("run {}", setup_cmd.display()))
}
