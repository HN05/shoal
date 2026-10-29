//! Opening web pages in the user's browser.
use std::process::{Command, Stdio};

use anyhow::{Context as _, Result, ensure};

/// Open `url` with the desktop's opener: `open` on macOS, `xdg-open` elsewhere.
pub(in crate::cli) fn open(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let status = Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .with_context(|| format!("run {opener}"))?;
    ensure!(status.success(), "{opener} could not open {url}");
    Ok(())
}
