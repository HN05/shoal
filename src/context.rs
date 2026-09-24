//! Per-invocation CLI settings shared by every command handler.
use std::io::{self, IsTerminal};

use anyhow::Result;
use serde::Serialize;

use crate::{
    output::{Palette, Style},
    paths::Paths,
};

pub struct Context {
    pub paths: Paths,
    /// Emit machine-readable output and never prompt.
    pub json: bool,
}

impl Context {
    /// Show elapsed time while waiting for silent work, without changing its result.
    pub async fn progress<T>(
        &self,
        message: &str,
        work: impl std::future::Future<Output = T>,
    ) -> T {
        crate::progress::run(self.json, message, work).await
    }

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

    /// Style a human-facing outcome without changing its machine-readable value.
    pub fn emit_styled(&self, style: Style, message: &str, value: impl Serialize) -> Result<()> {
        self.emit(&Palette::stdout(self.json).paint(style, message), value)
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

/// Pluralize a regular noun by appending `s` unless the count is one.
pub fn plural(count: u64, noun: &str) -> String {
    format!("{noun}{}", if count == 1 { "" } else { "s" })
}
