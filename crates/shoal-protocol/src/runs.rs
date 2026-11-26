//! The `shoal run` commands the daemon admits by their measured memory:
//! what tells one command's runs apart from another's.
use serde::{Deserialize, Serialize};

/// What tells the runs of a `shoal run` command apart, whose peaks one
/// estimate reads: a configured command's name, whatever arguments follow
/// it, or the exact arguments of `shoal run -- …`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandKey {
    Named(String),
    Argv(Vec<String>),
}

impl std::fmt::Display for CommandKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Named(name) => f.write_str(name),
            Self::Argv(argv) => f.write_str(&argv.join(" ")),
        }
    }
}

#[cfg(test)]
mod tests;
