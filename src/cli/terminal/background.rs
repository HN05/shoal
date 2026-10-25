//! Demos where no terminal host keeps them: the demo worker runs as a
//! background process writing a log, which stands in for its screen.
use std::{
    fs::{self, File},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context as _, Result};

use super::DemoWorker;
use crate::{model::Workspace, paths::Paths};

/// Start the worker in its own process group, writing to a new log, and
/// return the log's path as the demo's place.
pub(super) fn start_demo(
    paths: &Paths,
    workspace: &Workspace,
    name: &str,
    worker: &DemoWorker<'_>,
) -> Result<String> {
    let log = log_path(paths, &workspace.id, name);
    let place = log.to_str().context("Shoal state path is not UTF-8")?;
    let argv = worker(place)?;
    if let Some(directory) = log.parent() {
        fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    }
    let output = File::create(&log).with_context(|| format!("create {}", log.display()))?;
    std::process::Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(&workspace.path)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0)
        .spawn()
        .context("start the demo in the background")?;
    Ok(place.to_owned())
}

/// Follow the demo's log until the user interrupts it.
pub(super) async fn open_demo(place: &str) -> Result<()> {
    tokio::process::Command::new("tail")
        .args(["-n", "+1", "-F", place])
        .status()
        .await
        .context("follow the demo's log")?;
    Ok(())
}

pub(super) fn demo_screen(place: &str) -> Result<String> {
    let log = fs::read(Path::new(place)).with_context(|| format!("read {place}"))?;
    Ok(String::from_utf8_lossy(&log).into_owned())
}

fn log_path(paths: &Paths, workspace_id: &str, name: &str) -> PathBuf {
    paths
        .state
        .join("demos")
        .join(workspace_id)
        .join(format!("{name}.log"))
}
