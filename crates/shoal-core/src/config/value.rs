//! A setting's value, as written, from a variable of the agent's
//! environment, or from a command's standard output.
use std::{
    ffi::{OsStr, OsString},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};

pub use shoal_protocol::config::value::Value;

/// The variable or command `value` names is one Shoal can read or run.
pub fn validate(value: &Value, setting: &str) -> Result<()> {
    match value {
        Value::Written(_) => Ok(()),
        Value::Env { env } => crate::env::validate_configured_name(setting, env),
        Value::Cmd { cmd } => validate_command(&format!("{setting}.cmd"), cmd),
    }
}

/// The value itself, reading `variable` of the agent's environment for one
/// it names.
pub async fn resolve(value: &Value, variable: impl Fn(&str) -> Option<OsString>) -> Result<String> {
    let source = match value {
        Value::Written(value) => return Ok(value.clone()),
        Value::Env { env } => format!("variable {env}"),
        Value::Cmd { cmd } => format!("the output of {}", cmd[0]),
    };
    output(value, variable, None)
        .await?
        .into_string()
        .map_err(|_| anyhow::anyhow!("{source} is not UTF-8"))
}

/// The value's bytes, reading `variable` for one it names, and running a
/// command with PATH replaced by `path`.
pub async fn output(
    value: &Value,
    variable: impl Fn(&str) -> Option<OsString>,
    path: Option<&OsStr>,
) -> Result<OsString> {
    match value {
        Value::Written(value) => Ok(value.into()),
        Value::Env { env } => variable(env)
            .filter(|value| !value.is_empty())
            .with_context(|| format!("variable {env} is not set")),
        Value::Cmd { cmd } => command_output(cmd, path).await,
    }
}

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
