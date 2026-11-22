//! Moving the shell that started this process into a directory once the
//! process ends, through the directive shell integration gives it.
use std::path::Path;

use anyhow::{Context as _, Result, ensure};

use crate::paths::Paths;

/// Ask the calling shell to enter `path`, whatever the output format.
/// Whether it can: without shell integration it cannot. A scoped process's
/// shell, including one that cleared its token, is its agent's, which
/// keeps its own working directory, so it is never moved.
pub fn enter_directory(paths: &Paths, path: &Path) -> Result<bool> {
    if crate::process::scoped::inherits_scope(&paths.state) {
        return Ok(true);
    }
    let Some(destination) = std::env::var_os(crate::env::SHELL_DIRECTIVE) else {
        return Ok(false);
    };
    let path = path
        .to_str()
        .context("shell navigation path is not UTF-8")?;
    ensure!(
        !path.contains(['\n', '\r']),
        "shell navigation does not support newlines in paths"
    );
    std::fs::write(destination, format!("{path}\n")).context("write shell directory change")?;
    Ok(true)
}
