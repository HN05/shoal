//! One declaration of each repository-overridable option drives both
//! layering and provenance, so a reported layer is the one whose value is in
//! effect.
use std::{collections::BTreeMap, sync::LazyLock};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::config::{
    Config,
    agents::AgentTool,
    options::{self, Entry, Merge, Options},
    repo::{ConfigLayer as Layer, ConfigLayers, RepoConfig, Settings},
};

/// Settings after every layer, with each AI tool resolved from them and
/// what the machine adds to it.
#[derive(Debug, Serialize)]
pub struct Effective {
    #[serde(flatten)]
    settings: Settings,
    /// Every AI tool.
    #[serde(skip)]
    pub tools: crate::ai::Tools,
    /// Each agent's launcher, or the nearest of its bases'.
    #[serde(skip)]
    pub launchers: crate::config::named_commands::Commands,
    /// Each agent's command that restores a session.
    #[serde(skip)]
    pub resume_commands: crate::config::named_commands::Commands,
    /// The proxy each AI tool's tracked agents reach their upstream through.
    #[serde(skip)]
    pub proxies: BTreeMap<String, crate::proxy::Proxy>,
}

impl std::ops::Deref for Effective {
    type Target = Settings;

    fn deref(&self) -> &Settings {
        &self.settings
    }
}

impl std::ops::DerefMut for Effective {
    fn deref_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }
}

/// Every layer of one target, lowest first.
#[derive(Debug, Clone)]
pub struct Stack {
    layers: [(RepoConfig, Layer); 4],
    /// Settings a layer sets that repeat Shoal's guidance.
    warnings: Vec<String>,
}

impl Stack {
    pub fn new(global: &Config, repository: &ConfigLayers) -> Self {
        Self::over(global.repository_layer(), repository)
    }

    /// The stack of `global`, the global config's repository layer, under
    /// `repository`'s layers.
    pub fn over(global: RepoConfig, repository: &ConfigLayers) -> Self {
        let mut warnings = Vec::new();
        let mut normalize = |config: RepoConfig, layer: Layer| {
            warnings.extend(
                config
                    .normalize()
                    .into_iter()
                    .map(|warning| format!("{layer}: {warning}")),
            );
            (config, layer)
        };
        let layers = [
            normalize(RepoConfig::built_in(), Layer::BuiltInDefault),
            normalize(global, Layer::GlobalConfig),
            normalize(repository.worktree_file.clone(), Layer::WorktreeFile),
            normalize(repository.local_config.clone(), Layer::LocalConfig),
        ];
        Self { layers, warnings }
    }

    /// The settings in effect; fails when the layers combine into an invalid
    /// setting, such as a repository `ports.start` above the global `ports.end`.
    pub fn resolve(self) -> Result<Settings> {
        let [(mut base, _), (global, _), (mut repository, _), (saved, _)] = self.layers;
        repository.merge(saved, Merge::SameLevel);
        // Each layer is valid alone; the repository's layered names must agree
        // too. A repository name that shadows a global one is allocation's
        // concern, so a conflict there never blocks the workspace's settings.
        crate::daemon::resources::definitions(&repository.resources, &repository.resource_pools)
            .context("layered repository config")?;
        base.merge(global, Merge::SameLevel);
        base.merge(repository, Merge::RepositoryOverGlobal);
        let settings = base.resolve("")?;
        settings.ports.validate()?;
        Ok(settings)
    }

    /// Every option with its value and winning layer, in configuration order.
    pub fn report(&self) -> Result<Vec<Entry>> {
        self.clone().resolve()?;
        let mut entries = Vec::new();
        RepoConfig::entries("", &self.sources(), &mut entries)?;
        Ok(entries)
    }

    /// Warnings for the settings each layer sets that repeat Shoal's guidance.
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The entries of one named table with the layer each came from.
    pub fn named<T: Clone>(
        &self,
        table: fn(&RepoConfig) -> &BTreeMap<String, T>,
    ) -> BTreeMap<String, (T, Layer)> {
        options::winners(&options::project(&self.sources(), table))
            .into_iter()
            .map(|(name, (value, layer))| (name.clone(), (value.clone(), layer)))
            .collect()
    }

    /// Highest layer first, the order provenance is looked up in.
    fn sources(&self) -> Vec<(&RepoConfig, Layer)> {
        self.layers
            .iter()
            .rev()
            .map(|(config, layer)| (config, *layer))
            .collect()
    }
}

#[cfg(test)]
impl RepoConfig {
    /// This config layered over `base`: an option set here wins, an omitted
    /// one falls through, and a named port, resource, pool or command
    /// replaces the one below it whole.
    pub fn over(self, mut base: Self) -> Self {
        base.merge(self, Merge::SameLevel);
        base
    }
}

impl Effective {
    pub fn new(
        settings: Settings,
        tools: crate::ai::Tools,
        proxies: BTreeMap<String, crate::proxy::Proxy>,
    ) -> Self {
        let commands = |command: fn(&AgentTool) -> &Option<Vec<String>>| {
            settings
                .agents
                .iter()
                .filter_map(|(name, agent)| Some((name.clone(), command(agent).clone()?)))
                .collect::<crate::config::named_commands::Commands>()
        };
        let mut launchers = commands(|agent| &agent.command);
        crate::ai::inherit_launchers(&tools, &mut launchers);
        let resume_commands = commands(|agent| &agent.resume_command);
        Self {
            settings,
            tools,
            launchers,
            resume_commands,
            proxies,
        }
    }

    pub fn into_settings(self) -> Settings {
        self.settings
    }

    /// Agent `name`'s launch profile; an agent no AI tool describes takes no
    /// arguments and changes nothing in its environment.
    pub fn launch(&self, name: &str) -> &crate::ai::LaunchProfile {
        static NONE: LazyLock<crate::ai::LaunchProfile> = LazyLock::new(Default::default);
        self.tools.get(name).map_or(&NONE, |tool| &tool.launch)
    }

    /// The integration agent `name` runs with; none for an agent no AI tool
    /// describes.
    pub fn integration(&self, name: &str) -> crate::ai::IntegrationKind {
        self.tools
            .get(name)
            .map_or(crate::ai::IntegrationKind::None, |tool| tool.integration)
    }

    /// The proxy agent `name`'s tracked launches reach their upstream
    /// through.
    pub fn proxy(&self, name: &str) -> Option<&crate::proxy::Proxy> {
        self.proxies.get(name)
    }
}

#[cfg(test)]
mod tests;
