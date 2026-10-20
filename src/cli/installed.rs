//! Hand long-running commands to the installed release after an upgrade
//! changes the daemon protocol.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};

use super::client::ProtocolMismatch;

/// The binary this command runs, so an upgrade that changes the daemon
/// protocol can hand the command to the installed release.
pub(crate) struct Executable {
    path: Option<PathBuf>,
    identity: Option<FileIdentity>,
}

type FileIdentity = (u64, u64, i64, i64);

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
        let identity = file_identity(&path);
        Self {
            path: Some(path),
            identity,
        }
    }

    /// Whether another binary now sits at this path, as after an upgrade.
    fn replaced(&self) -> bool {
        self.path
            .as_deref()
            .and_then(file_identity)
            .is_some_and(|now| self.identity != Some(now))
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

// Follows symlinks, so retargeting a versioned install link counts as a change.
fn file_identity(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = std::fs::metadata(path).ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
    ))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn executable_is_replaced_by_a_new_file_or_link_target() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("first");
        let second = temp.path().join("second");
        let link = temp.path().join("shoal");
        std::fs::write(&first, "old").unwrap();
        std::fs::write(&second, "new").unwrap();
        std::os::unix::fs::symlink(&first, &link).unwrap();
        let executable = Executable::at(link.clone());
        assert!(!executable.replaced());

        std::fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(&second, &link).unwrap();
        assert!(executable.replaced());

        let executable = Executable::at(first.clone());
        std::fs::rename(&second, &first).unwrap();
        assert!(executable.replaced());
        std::fs::remove_file(&first).unwrap();
        assert!(!executable.replaced(), "a missing binary cannot take over");
    }

    // A failed exec would leave this process without its ignored SIGPIPE.
    #[test]
    fn only_a_protocol_change_on_a_replaced_binary_hands_the_command_over() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("shoal");
        std::fs::write(&path, "old").unwrap();
        let executable = Executable::at(path.clone());
        let mismatch = executable.resume_or(ProtocolMismatch { daemon: 0 }.into(), || {
            unreachable!("the binary was not replaced")
        });
        assert!(mismatch.is::<ProtocolMismatch>());
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, "new").unwrap();
        let error = executable.resume_or(anyhow::anyhow!("lookup failed"), || {
            unreachable!("only a protocol change hands the command over")
        });
        assert_eq!(error.to_string(), "lookup failed");
    }

    #[test]
    fn a_replaced_option_keeps_the_other_arguments() {
        let arguments = |arguments: &[&str]| {
            replace_option(arguments.iter().map(OsString::from), "--since", "9")
        };
        assert_eq!(
            arguments(&["internal", "events", "--since", "3", "--follow"]),
            ["internal", "events", "--follow", "--since", "9"]
        );
        assert_eq!(
            arguments(&["--json", "internal", "events", "--since=3"]),
            ["--json", "internal", "events", "--since", "9"]
        );
        assert_eq!(
            arguments(&["internal", "events", "--follow"]),
            ["internal", "events", "--follow", "--since", "9"]
        );
    }
}
