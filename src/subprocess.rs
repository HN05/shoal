//! Run external tools with argument arrays and capture their output.
use anyhow::{Context, Result, bail};
use std::process::Stdio;
use tokio::process::Command;

/// Stderr kept in error messages; longer output would exceed a protocol frame.
const MAX_DIAGNOSTIC_CHARS: usize = 8192;

/// Run to completion with no stdin and return stdout as UTF-8. A nonzero exit
/// becomes an error carrying the program name and trimmed stderr.
pub async fn output(mut command: Command) -> Result<String> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .with_context(|| {
            format!("run {program}; ensure it is installed and on the daemon's PATH")
        })?;
    if !output.status.success() {
        let diagnostic = String::from_utf8_lossy(&output.stderr);
        bail!(
            "{program} failed ({}): {}",
            output.status,
            diagnostic
                .chars()
                .take(MAX_DIAGNOSTIC_CHARS)
                .collect::<String>()
        );
    }
    String::from_utf8(output.stdout).context("tool output is not UTF-8")
}
