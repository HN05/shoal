//! Hand long-running commands to the installed release after an upgrade
//! changes the daemon protocol.
use std::{ffi::OsString, path::PathBuf};

use super::client::ProtocolMismatch;
use crate::fsutil::FileIdentity;

/// The binary this command runs, so an upgrade that changes the daemon
/// protocol can hand the command to the installed release.
pub(crate) struct Executable {
    path: Option<PathBuf>,
    identity: Option<FileIdentity>,
}

impl Executable {
    pub(crate) fn current() -> Self {
        match crate::fsutil::invoked_executable() {
            Ok(path) => Self::at(path),
            Err(_) => Self {
                path: None,
                identity: None,
            },
        }
    }

    fn at(path: PathBuf) -> Self {
        let identity = FileIdentity::of(&path).ok();
        Self {
            path: Some(path),
            identity,
        }
    }

    /// Whether another binary now sits at this path, as after an upgrade,
    /// including one installed where none was found at the start.
    fn replaced(&self) -> bool {
        let Some(path) = self.path.as_deref() else {
            return false;
        };
        match self.identity {
            Some(identity) => identity.replaced_at(path),
            None => FileIdentity::of(path).is_ok(),
        }
    }

    /// After an upgrade replaced this binary, continue the command from the
    /// installed one with `arguments`. Returns `None` when nothing replaced
    /// it, and the failure when the restart fails.
    pub(crate) fn take_over(
        &self,
        arguments: impl FnOnce() -> Vec<OsString>,
    ) -> Option<anyhow::Error> {
        use std::{io::Write as _, os::unix::process::CommandExt as _};
        let path = self.path.as_ref().filter(|_| self.replaced())?;
        // The installed binary writes to the same output after this.
        let _ = std::io::stdout().flush();
        let error = std::process::Command::new(path).args(arguments()).exec();
        Some(anyhow::Error::new(error).context("continue with the installed shoal"))
    }

    /// Continue this command from the installed binary with `arguments` when
    /// `error` is the daemon protocol change an upgrade brought; otherwise
    /// return the error.
    pub(crate) fn resume_or(
        &self,
        error: anyhow::Error,
        arguments: impl FnOnce() -> Vec<OsString>,
    ) -> anyhow::Error {
        if !error.is::<ProtocolMismatch>() {
            return error;
        }
        self.take_over(arguments).unwrap_or(error)
    }
}

/// This command's arguments.
pub(crate) fn arguments() -> Vec<OsString> {
    std::env::args_os().skip(1).collect()
}

/// This command's arguments, with `option` set to any `value` in place of the
/// one it was given.
pub(crate) fn arguments_with(option: &str, value: Option<impl ToString>) -> Vec<OsString> {
    match value {
        Some(value) => replace_option(arguments(), option, &value.to_string()),
        None => arguments(),
    }
}

pub(crate) fn replace_option(
    arguments: impl IntoIterator<Item = OsString>,
    option: &str,
    value: &str,
) -> Vec<OsString> {
    let inline = format!("{option}=");
    let mut replaced = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        if argument == option {
            arguments.next();
        } else if !argument
            .to_str()
            .is_some_and(|argument| argument.starts_with(&inline))
        {
            replaced.push(argument);
        }
    }
    replaced.extend([option.into(), value.into()]);
    replaced
}

/// `arguments` with `flag` added unless they already have it.
pub(crate) fn with_flag(mut arguments: Vec<OsString>, flag: &str) -> Vec<OsString> {
    if !arguments.iter().any(|argument| argument == flag) {
        arguments.push(flag.into());
    }
    arguments
}

#[cfg(test)]
mod tests;
