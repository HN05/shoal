//! Per-invocation CLI settings shared by every command handler.
use std::io::{self, IsTerminal};

use anyhow::Result;
use serde::Serialize;

use crate::{
    cli::{
        availability::Need,
        output::{Palette, Style},
    },
    paths::Paths,
};

pub struct Context {
    pub paths: Paths,
    /// The place a backend opened for this process, when it runs in one.
    pub(in crate::cli) place: Option<super::terminal::Place>,
    /// Emit machine-readable output and never prompt.
    pub json: bool,
    /// What the command needs of the workspace a picker offers.
    pub(in crate::cli) need: Need,
    /// The name under which the command remembers the workspaces it acts on,
    /// for `-`; `None` for commands without that history.
    pub(in crate::cli) recent: Option<String>,
    /// The subcommand path, such as `["repo", "rm"]`, whose arguments
    /// refusals name.
    pub(in crate::cli) command: Vec<String>,
}

impl Context {
    /// Show elapsed time while waiting for silent work, without changing its result.
    pub async fn progress<T>(
        &self,
        message: &str,
        work: impl std::future::Future<Output = T>,
    ) -> T {
        crate::cli::progress::run(self.json, message, work).await
    }

    /// Your own forge logins, with the Bitbucket credentials the global
    /// config names.
    pub fn forge_account(&self) -> Result<crate::forge::account::Account> {
        let config = crate::config::Config::load(&self.paths)?;
        Ok(crate::forge::account::Account::user(&config.bitbucket))
    }

    /// Classify pasted links of the forge hosts the global config pins as
    /// their forges, reading the config only once a forge link is parsed. A
    /// config that does not load reports itself to the command that reads it.
    pub fn learn_pinned_forges(&self) {
        let paths = self.paths.clone();
        crate::forge::hosts::learn_pins(move || {
            crate::config::Config::load(&paths)
                .map(|config| config.forges.pins())
                .unwrap_or_default()
        });
    }

    /// What the global config and the daemon say each forge host serves.
    pub fn forge_hosts(&self) -> Result<super::forge_hosts::ForgeHosts<'_>> {
        let config = crate::config::Config::load(&self.paths)?;
        Ok(super::forge_hosts::ForgeHosts {
            paths: &self.paths,
            forges: config.forges,
        })
    }

    pub fn new(paths: Paths, json: bool) -> Self {
        Self {
            paths,
            json,
            place: None,
            need: Need::Any,
            recent: None,
            command: Vec::new(),
        }
    }

    /// Prompts are allowed: a human is on both stdin and stderr, and the caller
    /// did not ask for JSON.
    pub fn interactive(&self) -> bool {
        Self::is_interactive(self.json)
    }

    /// Inspect-and-act flows require their displayed details to be visible too.
    pub fn interactive_output(&self) -> bool {
        self.interactive() && io::stdout().is_terminal()
    }

    pub fn is_interactive(json: bool) -> bool {
        !json && io::stdin().is_terminal() && io::stderr().is_terminal()
    }

    /// Print a human-readable line, or `value` as one JSON line with `--json`.
    pub fn emit(&self, message: &str, value: impl Serialize) -> Result<()> {
        if self.json {
            outln!("{}", serde_json::to_string(&value)?);
        } else {
            outln!("{message}");
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
            outln!("{}", serde_json::to_string(value)?);
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
