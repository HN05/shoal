//! Run external tools with argument arrays and capture their output.
use anyhow::{Context, Result, bail};
use std::{
    io,
    process::{Output, Stdio},
};
use tokio::process::Command;

/// Stderr kept in error messages; longer output would exceed a protocol frame.
const MAX_DIAGNOSTIC_CHARS: usize = 8192;

/// Run to completion with no stdin and return stdout as UTF-8. A nonzero exit
/// becomes an error carrying the program name and trimmed stderr.
pub async fn output(command: Command) -> Result<String> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    checked_output(&program, capture(command).await)
}

/// Capture bytes with no stdin, terminating the child if the future is dropped.
pub async fn capture(mut command: Command) -> io::Result<Output> {
    command
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
}

/// Apply the standard tool diagnostics to a captured process result.
pub fn checked_output(program: &str, output: io::Result<Output>) -> Result<String> {
    let output = output.with_context(|| {
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
