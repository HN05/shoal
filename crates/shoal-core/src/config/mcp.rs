//! `[mcp.<name>]`: the MCP servers a repository's agents start, which the
//! repository's presets bring and its tables add, change or turn off,
//! merged field by field across layers.
use std::collections::BTreeMap;

use anyhow::{Context as _, Result, bail, ensure};
use serde::{Deserialize, Serialize};

use crate::config::{options::Options, presets::Preset, repo::RepoConfig};

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = ServerSettings)]
pub struct ServerConfig {
    /// The program that runs the server, which defines it whole; unset
    /// changes the server a preset brings under the same name.
    pub command: Option<String>,
    pub args: Option<Vec<String>>,
    #[option(named)]
    pub env: BTreeMap<String, String>,
    #[option(default = true)]
    pub enabled: Option<bool>,
}

/// A server agents start: a program over stdio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Server {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub enabled: bool,
}

/// Every server the presets bring or a table defines, by name, turned off
/// ones among them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Servers(BTreeMap<String, Server>);

impl Servers {
    pub(crate) fn enabled(&self) -> impl Iterator<Item = (&str, &Server)> {
        (self.0.iter())
            .filter(|(_, server)| server.enabled)
            .map(|(name, server)| (name.as_str(), server))
    }
}

/// The servers of `config`'s presets, each table defining its own or
/// changing a preset's; a table without `command` that names no preset's
/// server is refused.
pub(crate) fn resolve(
    tables: BTreeMap<String, ServerSettings>,
    config: &RepoConfig,
) -> Result<Servers> {
    let presets = config.setup.presets.as_deref().unwrap_or_default();
    let mut servers = preset_servers(presets);
    for (name, table) in tables {
        let server = match (table.command, servers.remove(&name)) {
            (Some(command), _) => Server {
                command,
                args: table.args.unwrap_or_default(),
                env: table.env,
                enabled: table.enabled,
            },
            (None, Some(mut preset)) => {
                preset.args = table.args.unwrap_or(preset.args);
                preset.env.extend(table.env);
                preset.enabled = table.enabled;
                preset
            }
            (None, None) => {
                bail!("mcp.{name} needs a command: no preset brings a server of that name")
            }
        };
        servers.insert(name, server);
    }
    Ok(Servers(servers))
}

fn preset_servers(presets: &[Preset]) -> BTreeMap<String, Server> {
    (presets.iter())
        .flat_map(|preset| preset.mcp_servers())
        .filter_map(|(name, argv)| {
            let (command, args) = argv.split_first()?;
            let server = Server {
                command: (*command).to_owned(),
                args: args.iter().map(|&arg| arg.to_owned()).collect(),
                env: BTreeMap::new(),
                enabled: true,
            };
            Some(((*name).to_owned(), server))
        })
        .collect()
}

/// Each server's name, which agents' config keys spell, its command and
/// its variables, as one layer writes them.
pub(crate) fn validate(tables: &BTreeMap<String, ServerConfig>) -> Result<()> {
    for (name, table) in tables {
        crate::validate::lowercase_name("MCP server", name)
            .with_context(|| format!("invalid MCP server name: {name}"))?;
        if let Some(command) = &table.command {
            ensure!(
                !command.trim().is_empty() && !command.contains('\0'),
                "mcp.{name}.command must name a program"
            );
        }
        let table_name = format!("[mcp.{name}.env]");
        for (variable, value) in &table.env {
            crate::env::validate_configured_name(&table_name, variable)?;
            ensure!(
                !value.contains('\0'),
                "{table_name} value for {variable} contains NUL"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
