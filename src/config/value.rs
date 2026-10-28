//! A setting's value, as written, from a variable of the agent's
//! environment, or from a command's standard output.
use std::{
    ffi::{OsStr, OsString},
    process::Stdio,
};

use anyhow::{Context, Result, ensure};
use serde::Deserialize;

/// A setting's value: as written, `{ env = "<variable>" }` or
/// `{ cmd = ["<program>", ...] }`.
#[derive(Clone, PartialEq, Eq, Deserialize)]
#[serde(
    untagged,
    deny_unknown_fields,
    expecting = "a value, { env = \"<variable>\" } naming the variable that holds it, or { cmd = [\"<program>\", ...] } printing it"
)]
pub enum Value {
    Written(String),
    Env { env: String },
    Cmd { cmd: Vec<String> },
}

/// A value as written never shows in what Shoal prints.
impl std::fmt::Debug for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Written(_) => f.write_str("Written(..)"),
            Self::Env { env } => f.debug_struct("Env").field("env", env).finish(),
            Self::Cmd { cmd } => f.debug_struct("Cmd").field("cmd", cmd).finish(),
        }
    }
}

impl Value {
    /// The variable or command the value names is one Shoal can read or run.
    pub fn validate(&self, setting: &str) -> Result<()> {
        match self {
            Self::Written(_) => Ok(()),
            Self::Env { env } => crate::env::validate_configured_name(setting, env),
            Self::Cmd { cmd } => validate_command(&format!("{setting}.cmd"), cmd),
        }
    }

    /// The value itself, reading `variable` of the agent's environment for
    /// one it names. A build without the proxy reads none yet.
    #[cfg_attr(not(feature = "proxy"), allow(dead_code))]
    pub async fn resolve(&self, variable: impl Fn(&str) -> Option<OsString>) -> Result<String> {
        let (value, source) = match self {
            Self::Written(value) => return Ok(value.clone()),
            Self::Env { env } => {
                let value = variable(env)
                    .filter(|value| !value.is_empty())
                    .with_context(|| format!("variable {env} is not set"))?;
                (value, format!("variable {env}"))
            }
            Self::Cmd { cmd } => (
                command_output(cmd, None).await?,
                format!("the output of {}", cmd[0]),
            ),
        };
        value
            .into_string()
            .map_err(|_| anyhow::anyhow!("{source} is not UTF-8"))
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
