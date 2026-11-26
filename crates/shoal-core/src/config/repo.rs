//! A repository's config: its options, one module per section, and the
//! layers it is read from. The configuration reference describes each option.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::config::{hooks::HookCommand, options::Options};

mod agent;
mod alerts;
mod app;
mod caches;
mod cleanup;
mod conflicts;
mod containers;
mod hooks;
mod issue;
mod load;
mod merge;
mod ports;
mod pr;
mod priority;
mod review;
mod runner;
mod setup;
mod simulators;
mod sleep;
mod terminal;

pub(crate) use agent::*;
pub(crate) use alerts::*;
pub use app::*;
pub use caches::*;
pub(crate) use cleanup::*;
pub(crate) use conflicts::*;
pub(crate) use containers::*;
pub(crate) use hooks::*;
pub(crate) use issue::*;
pub use load::*;
pub use merge::*;
pub use ports::*;
pub(crate) use pr::*;
pub(crate) use priority::*;
pub use review::*;
pub use runner::*;
pub use setup::*;
pub(crate) use shoal_protocol::config::repo::ConfigLayer;
pub use shoal_protocol::{config::repo::ConflictPolicy, containers::ContainerStart};
pub use simulators::*;
pub(crate) use sleep::*;
pub use terminal::*;

#[derive(Debug, Default, Clone, Deserialize, Serialize, Options)]
#[serde(default, deny_unknown_fields)]
#[options(resolved = Settings)]
pub struct RepoConfig {
    #[option(named)]
    pub commands: crate::config::named_commands::Commands,
    #[option(named_sections, default = crate::config::agents::built_in())]
    pub(crate) agents: BTreeMap<String, crate::config::agents::AgentToolConfig>,
    pub git_profile: Option<String>,
    #[option(section)]
    pub agent: AgentConfig,
    #[option(section)]
    pub app: AppConfig,
    /// Each app's own settings, which Shoal hands it unread.
    #[option(named_sections)]
    pub apps: BTreeMap<String, crate::config::apps::AppTable>,
    #[option(named_sections, default = crate::config::modes::built_in_modes())]
    pub modes: BTreeMap<String, crate::config::modes::ModeConfig>,
    #[option(named_sections, default = crate::config::modes::built_in_rules())]
    pub rules: BTreeMap<String, crate::config::modes::RuleConfig>,
    #[option(section)]
    pub terminal: TerminalConfig,
    #[option(section)]
    pub setup: SetupConfig,
    #[option(section)]
    pub hooks: HooksConfig,
    #[option(section)]
    pub ports: PortsConfig,
    /// Environment variables rendered for a workspace, with the presets'.
    #[option(named, resolve = preset_environment)]
    pub env: BTreeMap<String, String>,
    #[option(named, resolve = preset_caches)]
    pub caches: BTreeMap<String, Cache>,
    /// MCP servers agents start, with the presets'.
    #[option(named_sections, resolve = crate::config::mcp::resolve -> crate::config::mcp::Servers)]
    pub(crate) mcp: BTreeMap<String, crate::config::mcp::ServerConfig>,
    #[option(named)]
    pub demos: BTreeMap<String, shoal_protocol::demos::Demo>,
    /// Machine-specific, so the global config or local config sets it, never
    /// the worktree file.
    #[option(named)]
    pub secrets: crate::secrets::Secrets,
    #[option(named)]
    pub resources: BTreeMap<String, crate::resources::ResourceConfig>,
    #[option(named)]
    pub resource_pools: BTreeMap<String, crate::resources::PoolConfig>,
    #[option(section)]
    pub simulators: SimulatorsConfig,
    #[option(section)]
    pub containers: ContainersConfig,
    #[option(section)]
    pub cleanup: CleanupConfig,
    #[option(section)]
    pub sleep: SleepConfig,
    #[option(section)]
    pub pr: PrConfig,
    #[option(section)]
    pub issue: IssueConfig,
    #[option(section)]
    pub priority: PriorityConfig,
    #[option(section)]
    pub review: ReviewConfig,
    #[option(section)]
    pub gate: crate::config::gate::GateConfig,
    #[option(section)]
    pub alerts: AlertsConfig,
    #[option(section)]
    pub merge: MergeConfig,
    #[option(section)]
    pub conflicts: ConflictsConfig,
    #[option(section)]
    pub(crate) permissions: crate::config::permissions::PermissionsConfig,
    #[option(section)]
    pub runner: RunnerConfig,
    /// What starts agents on its own, by name. A repository's triggers
    /// start in it; the global config's are its own, never a repository's.
    #[option(named)]
    pub triggers: crate::triggers::Triggers,
    /// The options of this layer that take another's value.
    #[serde(
        default,
        skip_serializing_if = "crate::config::same_as::References::is_empty"
    )]
    #[option(references)]
    pub(crate) same_as: crate::config::same_as::References,
}

fn preset_environment(
    env: BTreeMap<String, String>,
    config: &RepoConfig,
) -> Result<BTreeMap<String, String>> {
    let presets = config.setup.presets.as_deref().unwrap_or_default();
    Ok(crate::config::presets::environment(presets, env))
}

fn preset_caches(
    caches: BTreeMap<String, Cache>,
    config: &RepoConfig,
) -> Result<BTreeMap<String, Cache>> {
    let presets = config.setup.presets.as_deref().unwrap_or_default();
    Ok(crate::config::presets::caches(presets, caches))
}

impl RepoConfig {
    /// Warn about a rule that repeats the built-in guidance.
    pub fn normalize(&self) -> Vec<String> {
        self.rules
            .iter()
            .filter(|(_, rule)| {
                (rule.text.as_deref()).is_some_and(crate::config::templates::repeats_guidance)
            })
            .map(|(name, _)| {
                format!(
                    "rules.{name}.text repeats Shoal's built-in guidance, which every launch \
                     already receives; keep only your additions"
                )
            })
            .collect()
    }

    /// Each option's value on its own; layers combine into settings that
    /// [`Stack::resolve`](crate::config::resolve::Stack::resolve) checks again.
    pub(crate) fn validate(&self) -> Result<()> {
        crate::config::named_commands::validate(&self.commands)?;
        crate::config::agents::validate(&self.agents)?;
        crate::config::modes::validate(self)?;
        for name in [&self.git_profile, &self.agent.git_profile]
            .into_iter()
            .flatten()
        {
            crate::validate::name("git profile", name)?;
        }
        for &kind in crate::config::hooks::HookKind::ALL {
            kind.validate(kind.repository_command(self))?;
        }
        self.containers.validate()?;
        self.cleanup.validate()?;
        self.alerts.validate()?;
        self.gate.validate()?;
        self.conflicts.validate()?;
        self.runner.validate()?;
        crate::config::validate_port_range(self.ports.start, self.ports.end)?;
        for (name, definition) in &self.ports.definitions {
            crate::validate::lowercase_name("port", name)
                .with_context(|| format!("invalid configured port name: {name}"))?;
            ensure!(
                definition.port != Some(0),
                "configured port {name} cannot use port zero"
            );
        }
        crate::resources::definitions(&self.resources, &self.resource_pools)?;
        crate::triggers::validate(&self.triggers)?;
        crate::env::validate_configured_environment(&self.env)?;
        crate::config::validate_agent_dirs(self.agent.dirs.as_deref().unwrap_or_default())?;
        crate::agent::ModelChoice {
            model: self.agent.model.clone(),
            effort: self.agent.effort.clone(),
        }
        .validate("agent")?;
        validate_caches(&self.caches)?;
        crate::config::mcp::validate(&self.mcp)?;
        shoal_protocol::demos::validate(&self.demos)?;
        crate::config::presets::validate(self.setup.presets.as_deref().unwrap_or_default())?;
        crate::secrets::validate(&self.secrets)?;
        self.setup.seed.validate()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
