//! Per-invocation CLI settings shared by every command handler.
use std::io::{self, IsTerminal};

use anyhow::Result;
use serde::Serialize;

use crate::paths::Paths;

pub struct Context {
    pub paths: Paths,
    /// Emit machine-readable output and never prompt.
    pub json: bool,
}

impl Context {
    pub fn new(paths: Paths, json: bool) -> Self {
        Self { paths, json }
    }

    /// Prompts are allowed: a human is on both stdin and stderr, and the caller
    /// did not ask for JSON.
    pub fn interactive(&self) -> bool {
        Self::is_interactive(self.json)
    }

    pub fn is_interactive(json: bool) -> bool {
        !json && io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    /// Print a human-readable line, or `value` as one JSON line with `--json`.
    pub fn emit(&self, message: &str, value: impl Serialize) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string(&value)?);
        } else {
            println!("{message}");
        }
        Ok(())
    }

    /// Print `value` as JSON with `--json`; otherwise render it with `text`.
    pub fn show<T: Serialize>(&self, value: &T, text: impl FnOnce(&T)) -> Result<()> {
        if self.json {
            println!("{}", serde_json::to_string(value)?);
        } else {
            text(value);
        }
        Ok(())
    }
}

/// `" (detail)"`-style suffix for an optional field, empty when absent.
pub fn optional(value: Option<&str>, format: impl FnOnce(&str) -> String) -> String {
    value.map(format).unwrap_or_default()
}
