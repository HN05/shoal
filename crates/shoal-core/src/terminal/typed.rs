//! Commands for hosts that cannot set a new place's environment, so Shoal
//! starts the place with the command, or types it into the place's shell
//! where the host cannot. Either stays short: Shoal's launch step, which
//! loads the command and its variables from a private file and deletes the
//! file before running it, so neither shows on screen or in the shell's
//! history. Only the shell directive goes to the shell itself, which enters
//! the workspace through it.
use std::{
    collections::BTreeMap, ffi::OsString, os::unix::process::CommandExt, path::Path, time::Duration,
};

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

use crate::{env, workers};

/// How long a new place's shell may take to start its line editor. Past it,
/// Shoal types anyway, as into a shell that has none, whose terminal echoes
/// the text once.
const PROMPT_LIMIT: Duration = Duration::from_secs(5);
/// How often a wait for the line editor reads the terminal's mode.
const PROMPT_POLL: Duration = Duration::from_millis(50);

/// A command a host types into a new place's shell, with the file that
/// carries it until the place runs it.
pub(crate) struct Typed {
    text: String,
    command: [String; 3],
    file: tempfile::TempPath,
}

/// What the launch step runs.
#[derive(Serialize, Deserialize)]
struct Launch {
    argv: Vec<String>,
    env: BTreeMap<String, String>,
    /// Variables the place may carry that the command runs without.
    #[serde(default)]
    clear: Vec<String>,
}

impl Typed {
    /// Write `argv` and `env` to a new private file and name the command,
    /// for the state directory `state`, that runs them from it. The shell
    /// stays once it ends.
    pub(crate) fn new(state: &Path, argv: &[OsString], env: &[(&str, String)]) -> Result<Self> {
        Self::clearing(state, argv, env, &[], false)
    }

    /// [`Typed::new`] for a command whose end also ends the shell, closing
    /// its place, as a demo's does.
    pub(crate) fn ending_shell(
        state: &Path,
        argv: &[OsString],
        env: &[(&str, String)],
    ) -> Result<Self> {
        Self::clearing(state, argv, env, &[], true)
    }

    /// [`Typed::new`], or [`Typed::ending_shell`] when `ends_shell`, for a
    /// place whose environment may carry the variables `clear`, which the
    /// command runs without.
    pub(crate) fn clearing(
        state: &Path,
        argv: &[OsString],
        env: &[(&str, String)],
        clear: &[&str],
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
            clear: clear.iter().map(|name| (*name).to_owned()).collect(),
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
        let line = shell_line(&launch_argv(state, &file)?, directive);
        Ok(Self {
            text: typed_text(&line, ends_shell),
            command: place_command(&line, clear, ends_shell),
            file,
        })
    }

    /// What to type, ending with Enter.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// What a host that starts a place with a command starts it with
    /// instead of typing.
    pub(crate) fn command(&self) -> &[String] {
        &self.command
    }

    /// Leave the file to the launch step once the host typed the command or
    /// started the place with it.
    pub(crate) fn hand_over(self) -> Result<()> {
        self.file.keep().context("keep the place's launch file")?;
        Ok(())
    }
}

/// This executable's launch step for `file`, naming the state directory
/// `state` and the output as every worker does.
fn launch_argv(state: &Path, file: &Path) -> Result<Vec<String>> {
    workers::internal_command_for(state, false, workers::Worker::Launch { file })?
        .into_iter()
        .map(|word| word.into_string().ok())
        .collect::<Option<_>>()
        .context("Shoal's launch path is not UTF-8")
}

/// The shell line that runs `launch`, keeping the `directive` the shell
/// enters the workspace through.
fn shell_line(launch: &[String], directive: Option<&str>) -> String {
    let export = directive.map_or_else(String::new, |path| {
        format!(
            "export {}={}; ",
            env::SHELL_DIRECTIVE,
            crate::shell::quote(&[path])
        )
    });
    format!("{export}{}", crate::shell::quote(launch))
}

/// The `line` to type: a leading space keeps it out of shells' history, and
/// the shell exits after it when it `ends_shell`.
fn typed_text(line: &str, ends_shell: bool) -> String {
    let exit = if ends_shell { "; exit" } else { "" };
    format!(" {line}{exit}\r")
}

/// A place's command that runs `line`, then the user's login shell unless it
/// `ends_shell`, both without the variables `clear`. Job control gives the
/// launched command the terminal's foreground, as a shell at its prompt does,
/// so the place does not read as idle at a shell until the user's shell
/// starts.
fn place_command(line: &str, clear: &[&str], ends_shell: bool) -> [String; 3] {
    let unset = if clear.is_empty() {
        String::new()
    } else {
        format!("unset {}; ", crate::shell::quote(clear))
    };
    let shell = if ends_shell {
        ""
    } else {
        "; exec \"${SHELL:-/bin/sh}\" -l"
    };
    [
        "/bin/sh".into(),
        "-c".into(),
        format!("set -m; {unset}{line}{shell}"),
    ]
}

/// Wait until a new place's shell reads input through its line editor, at
/// its prompt, which takes the terminal out of line-at-a-time input, as
/// `canonical` reads it. Text typed before shows twice: the terminal echoes
/// it, then the line editor draws it again. A terminal whose mode
/// `canonical` cannot read is not waited for, and the limit bounds its reads.
pub(crate) async fn await_prompt<Canonical, Read>(mut canonical: Canonical)
where
    Canonical: FnMut() -> Read,
    Read: Future<Output = Option<bool>>,
{
    let wait = async {
        while canonical().await == Some(true) {
            tokio::time::sleep(PROMPT_POLL).await;
        }
    };
    let _ = tokio::time::timeout(PROMPT_LIMIT, wait).await;
}

/// The launch step: run the command `file` carries, after deleting it.
pub fn run(file: &Path) -> Result<i32> {
    let launch = read(file)?;
    let (program, args) = launch
        .argv
        .split_first()
        .context("the launch file names no command")?;
    let mut command = std::process::Command::new(program);
    command.args(args);
    for name in &launch.clear {
        command.env_remove(name);
    }
    let error = command.envs(&launch.env).exec();
    Err(error).with_context(|| format!("run {program}"))
}

fn read(file: &Path) -> Result<Launch> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    std::fs::remove_file(file).with_context(|| format!("remove {}", file.display()))?;
    serde_json::from_slice(&bytes).context("invalid launch file")
}

#[cfg(test)]
mod tests;
