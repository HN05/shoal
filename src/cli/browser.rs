//! Opening web pages in the user's browser.
use std::{
    os::unix::process::CommandExt,
    process::{Command, Stdio},
};

use anyhow::{Context as _, Result};

/// Hand `url` to the desktop's opener: `open` on macOS, `xdg-open`
/// elsewhere. It is not waited for, since `xdg-open` may run the browser
/// itself until the browser closes, and runs in its own process group, so
/// it outlives the terminal's hangup when this command exits first.
pub(in crate::cli) fn open(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .with_context(|| format!("run {opener} to open {url}"))?;
    std::thread::spawn(move || child.wait());
    Ok(())
}
