//! Executable names and the dependencies checked by doctor on every installation.
use crate::{forge::transport::programs, sessions::HostName};

#[derive(Clone, Copy)]
pub enum Dependency {
    Required,
    Optional(&'static str),
    /// The program of a terminal host, required while it is the default.
    Host(HostName),
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
    Lsof => ("lsof", Some(Dependency::Required)),
    Zmx => ("zmx", Some(Dependency::Host(HostName::Zmx))),
    Fzf => ("fzf", Some(Dependency::Optional("needed for interactive pickers"))),
    // Feature-specific and platform tools are diagnosed when invoked.
    Xcrun => ("xcrun", None),
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
