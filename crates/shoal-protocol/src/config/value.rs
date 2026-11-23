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

    /// The value as a record that binds it keeps it, such as an approval
    /// of a secret: a written one stands as its SHA-256 digest, so no
    /// record holds it.
    pub fn recorded(&self) -> Self {
        match self {
            Self::Written(value) => {
                use sha2::Digest;
                let digest = sha2::Sha256::digest(value.as_bytes());
                let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
                Self::Written(format!("sha256:{hex}"))
            }
            value => value.clone(),
        }
    }
}

/// A setting whose value a process asks its daemon for, as one it cannot
/// read itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueKey {
    /// `[secrets.NAME]`'s value, which the caller's launch resolved as
    /// `source`.
    Secret {
        name: String,
        source: crate::approvals::SecretSpecification,
    },
}

impl std::fmt::Display for ValueKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Secret { name, .. } => write!(f, "secrets.{name}.value"),
        }
    }
}

/// The value the daemon read for a [`ValueKey`], which never shows in what
/// Shoal prints.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Resolved(pub String);

impl std::fmt::Debug for Resolved {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resolved(..)")
    }
}

#[cfg(test)]
mod tests;
