//! Values that settings take from a command's standard output.
use std::{
    ffi::{OsStr, OsString},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};

/// `setting`, an argument array, names a program and holds no NUL.
pub fn validate_command(setting: &str, command: &[String]) -> Result<()> {
    ensure!(
        command.first().is_some_and(|program| !program.is_empty()),
        "{setting} must name a program"
    );
    ensure!(
        command.iter().all(|word| !word.contains('\0')),
        "{setting} contains NUL"
    );
    Ok(())
}

/// Run `command` without input, so it never consumes the launching
/// terminal's or a tool's; its diagnostics reach the caller's stderr. `path`
/// replaces PATH for it. One trailing line break is the command's
/// formatting, not the value's. A command that fails, prints nothing or
/// prints NUL gives no value.
pub async fn command_output(command: &[String], path: Option<&OsStr>) -> Result<OsString> {
    use std::os::unix::ffi::OsStringExt;
    let program = &command[0];
    let mut process = tokio::process::Command::new(program);
    if let Some(path) = path {
        process.env("PATH", path);
    }
    let output = process
        .args(&command[1..])
        .stdin(Stdio::null())
        .stderr(Stdio::inherit())
        .output()
        .await
        .with_context(|| format!("run {program}"))?;
    ensure!(
        output.status.success(),
        "{program} exited with {}",
        output.status
    );
    let mut value = output.stdout;
    if value.ends_with(b"\n") {
        value.pop();
        if value.ends_with(b"\r") {
            value.pop();
        }
    }
    ensure!(!value.is_empty(), "{program} printed nothing");
    ensure!(!value.contains(&0), "{program} printed NUL");
    Ok(OsString::from_vec(value))
}
