//! A setting's value, as written, from a variable of the environment, or
//! from a command's standard output. Shoal's own code reads it.
use serde::{Deserialize, Serialize};

/// A setting's value: as written, `{ env = "<variable>" }` or
/// `{ cmd = ["<program>", ...] }`.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
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
    /// Whether reading the value runs a command, which takes as long as the
    /// command does.
    pub fn runs_command(&self) -> bool {
        matches!(self, Self::Cmd { .. })
    }
}

#[cfg(test)]
mod tests;
