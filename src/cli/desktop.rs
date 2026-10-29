//! Opening web pages and files in the desktop's applications.
use std::{
    ffi::OsStr,
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result};

/// Hand `target`, a URL or a file, to the desktop's opener: `open` on
/// macOS, `xdg-open` elsewhere, which open it in the application chosen for
/// it. It is not waited for, since `xdg-open` may run that application
/// itself until it closes, and runs in its own process group, so
/// it outlives the terminal's hangup when this command exits first.
pub(in crate::cli) fn open(target: impl AsRef<OsStr>) -> Result<()> {
    let target = target.as_ref();
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(opener)
        .arg(target)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("run {opener} to open {}", target.display()))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}
