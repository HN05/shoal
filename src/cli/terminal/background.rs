//! Demos where no terminal host keeps them: the demo worker runs as a
//! background process writing a log, which stands in for its screen.
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::Stdio,
};

use anyhow::{Context as _, Result};

use super::DemoWorker;
use crate::{
    model::Workspace,
    paths::Paths,
    sessions::{HostName, Origin},
};

/// Start the worker in its own process group, writing to a new log, and
/// return the log's path as the demo's place.
pub(super) fn start_demo(
    paths: &Paths,
    workspace: &Workspace,
    name: &str,
    worker: &DemoWorker<'_>,
) -> Result<Origin> {
    let log = log_path(paths, &workspace.id, name);
    let place = log.to_str().context("Shoal state path is not UTF-8")?;
    let place = Origin::created(HostName::Terminal, place.to_owned());
    let argv = worker(&place)?;
    if let Some(directory) = log.parent() {
        fs::create_dir_all(directory).with_context(|| format!("create {}", directory.display()))?;
    }
    let output = File::create(&log).with_context(|| format!("create {}", log.display()))?;
    let mut command = std::process::Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&workspace.path)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0);
    for name in super::NOT_INHERITED_BY_DEMOS {
        command.env_remove(name);
    }
    command
        .spawn()
        .context("start the demo in the background")?;
    Ok(place)
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

/// The end of the demo's log, which a long-running demo can make large.
pub(super) fn demo_screen(place: &str) -> Result<String> {
    tail(Path::new(place), SCREEN_BYTES).with_context(|| format!("read {place}"))
}

/// Refuse keys for a background demo, whose process reads no terminal.
pub(super) fn send_demo_keys() -> Result<()> {
    anyhow::bail!("a demo running in the background takes no keys; start it from zmx or Herdr")
}

/// How much of a log's end stands in for a screen.
const SCREEN_BYTES: u64 = 256 * 1024;

/// The last `limit` bytes of `path`, starting at a line when it had to cut.
fn tail(path: &Path, limit: u64) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let start = file.metadata()?.len().saturating_sub(limit);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let text = String::from_utf8_lossy(&bytes);
    let text = match (start, text.find('\n')) {
        (0, _) | (_, None) => &text[..],
        (_, Some(end)) => &text[end + 1..],
    };
    Ok(text.to_owned())
}

/// In the workspace's state directory, which its removal deletes.
fn log_path(paths: &Paths, workspace_id: &str, name: &str) -> PathBuf {
    paths
        .workspace_state(workspace_id)
        .join("demos")
        .join(format!("{name}.log"))
}

#[cfg(test)]
mod tests;
