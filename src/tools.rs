//! Executable names and the dependencies checked by doctor on every installation.
use crate::forge::transport::programs;

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
    Lsof => ("lsof", Some(Dependency::Required)),
    Zmx => ("zmx", Some(Dependency::Required)),
    Fzf => ("fzf", Some(Dependency::Optional("needed for interactive pickers"))),
    // Feature-specific and platform tools are diagnosed when invoked.
    Xcrun => ("xcrun", None),
    GitHub => (programs::GH, None),
    Forgejo => (programs::FJ, None),
    Tea => (programs::TEA, None),
    GitLab => (programs::GLAB, None),
    Curl => (programs::CURL, None),
    Tar => ("tar", None),
    Diff => ("diff", None),
    Ps => ("ps", None),
    Plutil => ("/usr/bin/plutil", None),
    Launchctl => ("launchctl", None),
    Systemctl => ("systemctl", None),
}

impl Tool {
    pub fn dependencies() -> impl Iterator<Item = (Self, Dependency)> {
        Self::ALL
            .iter()
            .copied()
            .filter_map(|tool| tool.dependency().map(|dependency| (tool, dependency)))
    }

    pub fn check_name(self) -> String {
        format!("dependency:{}", self.program())
    }
}
