//! Executable names, whether they are installed, and the dependencies
//! checked by doctor on every installation.
use std::{ffi::OsStr, path::Path};

use anyhow::{Result, ensure};

use crate::{forge::transport::programs, fsutil};

#[derive(Clone, Copy)]
pub enum Dependency {
    Required,
    Optional(&'static str),
}

macro_rules! tools {
    ($($tool:ident => ($program:expr, $dependency:expr)),+ $(,)?) => {
        #[derive(Clone, Copy)]
        pub enum Tool { $($tool),+ }

        impl Tool {
            pub const ALL: &[Self] = &[$(Self::$tool),+];

            pub const fn program(self) -> &'static str {
                match self { $(Self::$tool => $program),+ }
            }

            pub fn dependency(self) -> Option<Dependency> {
                match self { $(Self::$tool => $dependency),+ }
            }
        }
    };
}

tools! {
    Git => ("git", Some(Dependency::Required)),
    Worktrunk => ("wt", Some(Dependency::Required)),
    Fzf => ("fzf", Some(Dependency::Optional("needed for interactive pickers"))),
    // The default terminal host's program is checked as that host's;
    // feature-specific and platform tools are diagnosed when invoked.
    Zmx => ("zmx", None),
    Xcrun => ("xcrun", None),
    Tmux => ("tmux", None),
    Zellij => ("zellij", None),
    Curl => (programs::CURL, None),
    Tar => ("tar", None),
    Diff => ("diff", None),
    Ps => ("ps", None),
    Plutil => ("/usr/bin/plutil", None),
    Launchctl => ("launchctl", None),
    Systemctl => ("systemctl", None),
}

impl Tool {
    /// Whether the program is executable on this process's `PATH`.
    pub fn installed(self) -> bool {
        let path = std::env::var_os("PATH").unwrap_or_default();
        crate::fsutil::find_executable(std::ffi::OsStr::new(self.program()), &path).is_some()
    }

    pub fn dependencies() -> impl Iterator<Item = (Self, Dependency)> {
        Self::ALL
            .iter()
            .copied()
            .filter_map(|tool| tool.dependency().map(|dependency| (tool, dependency)))
    }
}

/// Refuse a launcher whose executable is not installed.
pub(crate) fn ensure_program(name: &str, program: &str) -> Result<()> {
    let search_path = std::env::var_os("PATH").unwrap_or_default();
    ensure!(
        program_available(program, &search_path),
        "cannot start {name}: {program} is not installed or not on PATH"
    );
    Ok(())
}

/// Programs with placeholders or workspace-relative paths resolve only at
/// launch, so they are assumed available.
pub(crate) fn program_available(program: &str, search_path: &OsStr) -> bool {
    let path = Path::new(program);
    if path.is_absolute() {
        fsutil::is_executable(path).unwrap_or(false)
    } else if program.contains('/') || program.contains('{') {
        true
    } else {
        fsutil::find_executable(OsStr::new(program), search_path).is_some()
    }
}
