//! A setting's value, as written, from a variable of the agent's
//! environment, or from a command's standard output. A command another
//! machine runs only the daemon reads, through [`super::values::Values`].
use std::{
    ffi::{OsStr, OsString},
    process::Stdio,
};

use anyhow::{Context, Result, bail, ensure};

pub use shoal_protocol::config::value::Value;

/// The variable or command `value` names is one Shoal can read or run.
pub(crate) fn validate(value: &Value, setting: &str) -> Result<()> {
    match value {
        Value::Written(_) => Ok(()),
        Value::Env { env } => crate::env::validate_configured_name(setting, env),
        Value::Cmd { cmd, on } => {
            validate_command(&format!("{setting}.cmd"), cmd)?;
            ensure!(
                on.as_ref()
                    .is_none_or(|machine| !machine.trim().is_empty()
                        && !machine.chars().any(char::is_control)),
                "{setting}.on must name a machine of the hub"
            );
            Ok(())
        }
    }
}

/// The value itself, reading `variable` of the agent's environment for one
/// it names.
pub async fn resolve(value: &Value, variable: impl Fn(&str) -> Option<OsString>) -> Result<String> {
    let source = match value {
        Value::Written(value) => return Ok(value.clone()),
        Value::Env { env } => format!("variable {env}"),
        Value::Cmd { cmd, .. } => format!("the output of {}", cmd[0]),
    };
    output(value, variable, None)
        .await?
        .into_string()
        .map_err(|_| anyhow::anyhow!("{source} is not UTF-8"))
}

/// The value's bytes, reading `variable` for one it names, and running a
/// command with PATH replaced by `path`. One another machine prints is
/// refused: its process asks the daemon for it.
pub(crate) async fn output(
    value: &Value,
    variable: impl Fn(&str) -> Option<OsString>,
    path: Option<&OsStr>,
) -> Result<OsString> {
    match value {
        Value::Written(value) => Ok(value.into()),
        Value::Env { env } => variable(env)
            .filter(|value| !value.is_empty())
            .with_context(|| format!("variable {env} is not set")),
        Value::Cmd { cmd, on: None } => command_output(cmd, path).await,
        Value::Cmd {
            cmd,
            on: Some(machine),
        } => bail!("{} runs on {machine}, through the daemon", cmd[0]),
    }
}

/// Bounds a command that prints a value for the daemon, which may wait on
/// a password manager.
const BOUNDED_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);

/// The most such a command may print.
const MAX_BOUNDED_BYTES: usize = 64 * 1024;

/// What `command` prints for the daemon, less one trailing line break,
/// bounded in time and size; its diagnostics go into the error rather than
/// to the daemon's stderr. A command that prints nothing or NUL gives no
/// value, as one [`command_output`] runs gives none.
pub async fn bounded_output(command: &[String]) -> Result<String> {
    let (program, args) = command
        .split_first()
        .context("the command names no program")?;
    let mut process = tokio::process::Command::new(program);
    process.args(args);
    let mut output = shoal_subprocess::Run::new(process)
        .timeout(BOUNDED_TIMEOUT)
        .output_limit(MAX_BOUNDED_BYTES)
        .output()
        .await?;
    if output.ends_with('\n') {
        output.pop();
        if output.ends_with('\r') {
            output.pop();
        }
    }
    ensure!(!output.is_empty(), "{program} printed nothing");
    ensure!(!output.contains('\0'), "{program} printed NUL");
    Ok(output)
}

/// `setting`, an argument array, names a program and holds no NUL.
pub(crate) fn validate_command(setting: &str, command: &[String]) -> Result<()> {
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
pub(crate) async fn command_output(command: &[String], path: Option<&OsStr>) -> Result<OsString> {
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
        .kill_on_drop(true)
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

#[cfg(test)]
mod tests;
