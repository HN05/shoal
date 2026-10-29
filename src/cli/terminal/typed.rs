//! Commands for hosts that cannot set a new place's environment, so Shoal
//! types the command into the place's shell instead. What it types stays
//! short: Shoal's launch step, which loads the command and its variables
//! from a private file and deletes the file before running it, so neither
//! shows on screen or in the shell's history. Only the shell directive goes
//! to the shell itself, which enters the workspace through it.
use std::{collections::BTreeMap, ffi::OsString, os::unix::process::CommandExt, path::Path};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{cli::internal, env};

/// A command a host types into a new place's shell, with the file that
/// carries it until the place runs it.
pub(in crate::cli) struct Typed {
    text: String,
    file: tempfile::TempPath,
}

/// What the launch step runs.
#[derive(Serialize, Deserialize)]
struct Launch {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
}

impl Typed {
    /// Write `argv` and `env` to a new private file and name the command,
    /// for the state directory `state`, that runs them from it. The shell
    /// stays once it ends.
    pub fn new(state: &Path, argv: &[OsString], env: &[(&str, String)]) -> Result<Self> {
        Self::write(state, argv, env, false)
    }

    /// [`Typed::new`] for a command whose end also ends the shell, closing
    /// its place, as a demo's does.
    pub fn ending_shell(state: &Path, argv: &[OsString], env: &[(&str, String)]) -> Result<Self> {
        Self::write(state, argv, env, true)
    }

    fn write(
        state: &Path,
        argv: &[OsString],
        env: &[(&str, String)],
        ends_shell: bool,
    ) -> Result<Self> {
        let launch = Launch {
            argv: argv
                .iter()
                .map(|arg| arg.to_str().map(str::to_owned))
                .collect::<Option<_>>()
                .context("Shoal's command is not UTF-8")?,
            env: env
                .iter()
                .map(|(name, value)| ((*name).to_owned(), value.clone()))
                .collect(),
        };
        let mut file = tempfile::Builder::new()
            .prefix("shoal-launch.")
            .tempfile()
            .context("create the place's launch file")?;
        serde_json::to_writer(&mut file, &launch).context("write the place's launch file")?;
        let file = file.into_temp_path();
        let directive = env
            .iter()
            .find(|(name, _)| *name == env::SHELL_DIRECTIVE)
            .map(|(_, path)| path.as_str());
        let text = typed_text(&launch_argv(state, &file)?, directive, ends_shell);
        Ok(Self { text, file })
    }

    /// What to type, ending with Enter.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Leave the file to the launch step once the host typed the command.
    pub fn hand_over(self) -> Result<()> {
        self.file.keep().context("keep the place's launch file")?;
        Ok(())
    }
}

/// This executable's launch step for `file`, naming the state directory
/// `state` and the output as every worker does.
fn launch_argv(state: &Path, file: &Path) -> Result<Vec<String>> {
    internal::internal_command_for(state, false, internal::Worker::Launch { file })?
        .into_iter()
        .map(|word| word.into_string().ok())
        .collect::<Option<_>>()
        .context("Shoal's launch path is not UTF-8")
}

/// The line to type: a leading space keeps it out of shells' history, the
/// shell keeps the `directive` it enters the workspace through, and it exits
/// after the command when it `ends_shell`.
fn typed_text(launch: &[String], directive: Option<&str>, ends_shell: bool) -> String {
    let export = directive.map_or_else(String::new, |path| {
        format!(
            "export {}={}; ",
            env::SHELL_DIRECTIVE,
            crate::shell::quote(&[path])
        )
    });
    let exit = if ends_shell { "; exit" } else { "" };
    format!(" {export}{}{exit}\r", crate::shell::quote(launch))
}

/// The launch step: run the command `file` carries, after deleting it.
pub(in crate::cli) fn run(file: &Path) -> Result<i32> {
    let launch = read(file)?;
    let (program, args) = launch
        .argv
        .split_first()
        .context("the launch file names no command")?;
    let error = std::process::Command::new(program)
        .args(args)
        .envs(&launch.env)
        .exec();
    Err(error).with_context(|| format!("run {program}"))
}

fn read(file: &Path) -> Result<Launch> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    std::fs::remove_file(file).with_context(|| format!("remove {}", file.display()))?;
    serde_json::from_slice(&bytes).context("invalid launch file")
}

#[cfg(test)]
mod tests;
