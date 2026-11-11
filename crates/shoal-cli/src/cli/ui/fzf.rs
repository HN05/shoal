//! The oldest fzf pickers work with, and the check that refuses an older one
//! before it opens.
use std::process::Command;

use anyhow::{Context as _, Result, ensure};

use crate::tools::Tool;

/// The oldest fzf Shoal's pickers support: the picker of several items
/// reads `{*f}`, which fzf 0.63.0 added. An older fzf rejects or ignores
/// options pickers pass.
const OLDEST: (u32, u32, u32) = (0, 63, 0);

/// Refuse the installed fzf when it is older than [`OLDEST`].
pub(super) fn ensure_supported() -> Result<()> {
    let output = Command::new(Tool::Fzf.program())
        .arg("--version")
        .output()
        .context("open picker; install fzf or supply an explicit target")?;
    supported(&String::from_utf8_lossy(&output.stdout))
}

/// Refuse the version `fzf --version` reported when it is older than
/// [`OLDEST`]; one it does not report as `X.Y.Z` passes.
fn supported(reported: &str) -> Result<()> {
    let Some(found) = parse_version(reported) else {
        return Ok(());
    };
    ensure!(
        found >= OLDEST,
        "fzf {}.{}.{} is too old; Shoal needs fzf {}.{}.{} or later",
        found.0,
        found.1,
        found.2,
        OLDEST.0,
        OLDEST.1,
        OLDEST.2
    );
    Ok(())
}

/// The version `fzf --version` reports first, as in `0.75.0 (Homebrew)`.
fn parse_version(reported: &str) -> Option<(u32, u32, u32)> {
    let version = reported.split_whitespace().next()?;
    let mut parts = version.split('.').map(|part| {
        let digits = part
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(part.len());
        part[..digits].parse().ok()
    });
    Some((parts.next()??, parts.next()??, parts.next()??))
}

#[cfg(test)]
mod tests;
